//! crates/ramaria-service/src/engine/assemble.rs - Ramaria 引擎装配与依赖访问器
//!
//! 设计特点:
//! - 装配顺序固定：数据库连接池 + migration → 配置只读加载 → 后端配置（DB 真源）
//!   → LLM provider（按 `[cache]` 注入精确缓存）→ 嵌入 provider（缺失降级）→ 检索器占位
//! - 注入构造（`from_parts`）：不创建连接池与迁移、不加载配置文件，调用方对依赖生命周期负责
//! - 访问器统一返回"已脱离锁"的句柄：Arc 克隆（provider 快照）或引用（存储 / 锁槽），
//!   异步路径不跨 `.await` 持锁
//! - 策略与钩子（召回策略 / 封存钩子 / 封存许可）由入口层注入，缺省按配置闸门映射
//! - provider 热更新：整体替换快照，读取方取到替换前或替换后的完整快照，无中间态
//! - 装配辅助（配置只读加载 / provider 构造 / 嵌入恢复）与热更新共用同一构造口径

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::config::{EmbeddingDevice, RamariaConfig};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{lock_recover, read_recover, write_recover};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::{AppState, BackendConfig, LlmProvider as LlmProviderKind};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::keyword::KeywordService;
use ramaria_memory::retriever::Retriever;
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;

use crate::proactive::ProactiveSink;
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;

use super::{Engine, EngineOptions};

// =========================================================
// 内部依赖访问器（crate 内用例实现使用）
// =========================================================
//
// 说明:
// - 统一返回 Arc 克隆（provider 快照）或引用（存储 / 锁槽），
//   用例实现拿到的都是"已脱离锁"的句柄，异步路径不会跨 `.await` 持锁。

impl Engine {
    /// 存储后端（crate 内用例实现使用）。
    pub(crate) fn storage_ref(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// LLM provider 快照（crate 内用例实现使用）。
    pub(crate) fn llm_ref(&self) -> Arc<dyn LlmProvider> {
        self.llm()
    }

    /// 嵌入 provider 快照（crate 内用例实现使用）。
    pub(crate) fn embedding_ref(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedding()
    }

    /// 行为层待定池（crate 内编排与测试使用）。
    pub(crate) fn behavior_pending_ref(&self) -> &Arc<Mutex<PendingPool>> {
        &self.behavior_pending
    }
}

// =========================================================
// 装配
// =========================================================

impl Engine {
    /// 装配引擎（配置路径缺省：数据库同目录 `config.toml`）。
    ///
    /// 参数:
    /// - `db_path`: 数据库文件路径（不存在时自动创建并执行 migration）。
    ///
    /// 返回:
    /// - 成功时返回完成装配的引擎实例。
    /// - 数据库初始化 / migration 失败时返回 Storage 错误；LLM provider 构建失败时返回对应错误。
    ///
    /// 说明:
    /// - 嵌入模型缺失 / 加载失败不阻塞装配（降级为 BM25 + 关键词镜像）。
    /// - 检索索引此时为空，首次召回时构建。
    pub async fn open(db_path: impl Into<PathBuf>) -> RamariaResult<Self> {
        Self::open_with(EngineOptions::new(db_path)).await
    }

    /// 按选项装配引擎。
    ///
    /// 装配顺序:
    /// 1. 数据库连接池 + migration（`ramaria-storage`）；
    /// 2. 配置加载（只读，不写回 DB）；
    /// 3. 后端配置（DB 侧 `backend_config`，无记录回退 LM Studio 默认）；
    /// 4. LLM provider（按 `[cache]` 配置注入精确缓存，缓存实例由引擎持有供热更新复用）；
    /// 5. 嵌入 provider（可选，缺失降级）；
    /// 6. 检索器占位（懒加载）。
    pub async fn open_with(options: EngineOptions) -> RamariaResult<Self> {
        let db_path = options.db_path;

        // ---- 1. 数据库连接池 + migration ----
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone())).await?;
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool.clone()));
        // 连接池句柄随引擎保留（缓存装配会消费一份克隆，先留出引擎侧句柄）
        let engine_pool = pool.clone();

        // ---- 2. 配置加载（只读，不双写）----
        let config_path = options
            .config_path
            .unwrap_or_else(|| default_config_path(&db_path));
        let config = load_config_readonly(&config_path, &db_path);

        // ---- 3. 后端配置（DB 为真相源；无记录回退 LM Studio 默认）----
        let backend_config = storage
            .get_backend_config()
            .await?
            .unwrap_or_else(BackendConfig::lm_studio_default);

        // ---- 4. LLM provider（按 [cache] 配置注入精确缓存）----
        let cache: Option<Arc<dyn LlmResponseCache>> = if config.cache.enabled {
            Some(Arc::new(ramaria_storage::SqliteLlmCache::new(
                pool,
                config.cache.max_entries,
                config.cache.eviction,
            )))
        } else {
            None
        };
        let keychain = Arc::new(Keychain::new());
        let llm = build_llm_provider(&backend_config, &keychain, cache.clone())?;

        // ---- 5. 嵌入 provider（可选，缺失降级）----
        let embedding = restore_embedding(&backend_config, config.embedding.device);

        tracing::info!(
            db = %path_log_label(&db_path),
            provider = %llm.name(),
            embedding = embedding.is_some(),
            "服务层引擎装配完成"
        );

        // 召回策略缺省按配置闸门映射（宿主可在装配后注入覆盖）
        let recall_policy = RecallPolicy::from_config(&config);

        Ok(Self {
            storage,
            llm: RwLock::new(llm),
            embedding: RwLock::new(embedding),
            keychain,
            llm_cache: RwLock::new(cache),
            behavior_pending: Arc::new(Mutex::new(PendingPool::new(&config.behavior))),
            config: RwLock::new(Arc::new(config)),
            config_path,
            db_path,
            pool: RwLock::new(Some(engine_pool)),
            // ---- 6. 检索器占位：首次召回时构建（懒加载）----
            retriever: Arc::new(RwLock::new(None)),
            // ---- 7. 关键词镜像与策略 / 钩子：空镜像 + 配置映射策略 ----
            keyword_mirror: Arc::new(RwLock::new(KeywordService::new())),
            index_dirty: Arc::new(AtomicBool::new(false)),
            index_stamp: Arc::new(RwLock::new(None)),
            last_index_build_ms: Arc::new(AtomicI64::new(0)),
            index_rebuild_failed: Arc::new(AtomicBool::new(false)),
            index_build_failure: Arc::new(RwLock::new(None)),
            recall_policy: Arc::new(RwLock::new(recall_policy)),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
            seal_allowed: AtomicBool::new(true),
            proactive_sink: Arc::new(RwLock::new(None)),
            // 状态机初值：首次配置判定由 setup 用例推进（装配阶段不做网络探测）
            state: Mutex::new(AppState::NeedsSetup),
        })
    }

    /// 用已装配的依赖构造引擎（测试注入 / 上层已持有依赖时复用）。
    ///
    /// 参数:
    /// - `storage` / `llm` / `embedding` / `config`: 直接注入的依赖。
    ///
    /// 说明:
    /// - 不创建连接池与迁移，不加载配置文件；调用方对依赖生命周期负责。
    /// - `db_path` 视为未设置（诊断信息为空路径）。
    /// - keychain 使用系统默认实例；响应缓存默认未启用（注入路径由调用方自行装配 provider）。
    pub fn from_parts(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        config: RamariaConfig,
    ) -> Self {
        // 召回策略缺省按配置闸门映射（宿主可在装配后注入覆盖）
        let recall_policy = RecallPolicy::from_config(&config);
        Self {
            storage,
            llm: RwLock::new(llm),
            embedding: RwLock::new(embedding),
            keychain: Arc::new(Keychain::new()),
            llm_cache: RwLock::new(None),
            behavior_pending: Arc::new(Mutex::new(PendingPool::new(&config.behavior))),
            config: RwLock::new(Arc::new(config)),
            config_path: PathBuf::new(),
            db_path: PathBuf::new(),
            pool: RwLock::new(None),
            retriever: Arc::new(RwLock::new(None)),
            keyword_mirror: Arc::new(RwLock::new(KeywordService::new())),
            index_dirty: Arc::new(AtomicBool::new(false)),
            index_stamp: Arc::new(RwLock::new(None)),
            last_index_build_ms: Arc::new(AtomicI64::new(0)),
            index_rebuild_failed: Arc::new(AtomicBool::new(false)),
            index_build_failure: Arc::new(RwLock::new(None)),
            recall_policy: Arc::new(RwLock::new(recall_policy)),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
            seal_allowed: AtomicBool::new(true),
            proactive_sink: Arc::new(RwLock::new(None)),
            state: Mutex::new(AppState::NeedsSetup),
        }
    }
}

// =========================================================
// 依赖访问器
// =========================================================

impl Engine {
    /// 存储后端引用（用例实现与诊断使用）。
    pub fn storage(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// 当前 LLM provider 快照（读锁内克隆 Arc，调用方在锁外使用，可安全移入异步任务）。
    pub fn llm(&self) -> Arc<dyn LlmProvider> {
        read_recover(&self.llm, "engine.llm").clone()
    }

    /// 嵌入 provider 快照（None = 向量通道降级）。
    pub fn embedding(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        read_recover(&self.embedding, "engine.embedding").clone()
    }

    /// 向量通道是否可用（嵌入模型已加载且自测可用）。
    ///
    /// 说明:
    /// - 判定包含 `is_available`（模型文件完整且未被标记降级），
    ///   与桌面 / CLI 对"嵌入可用"的口径一致；
    /// - 不发起推理调用，可高频调用（设置页轮询 / 每次召回前判定均可）。
    pub fn is_embedding_available(&self) -> bool {
        read_recover(&self.embedding, "engine.embedding")
            .as_ref()
            .is_some_and(|provider| provider.is_available())
    }

    /// OS keychain 引用（线上 provider 的 API key 读写）。
    pub fn keychain(&self) -> &Keychain {
        &self.keychain
    }

    /// OS keychain（Arc 克隆，供 provider 构造移入异步任务）。
    pub fn keychain_arc(&self) -> Arc<Keychain> {
        Arc::clone(&self.keychain)
    }

    /// 当前 LLM 响应精确缓存（None = `[cache].enabled=false`）。
    ///
    /// 用途:
    /// - 热更新 provider 时复用同一实例，切换后端后既有缓存不失效。
    pub fn llm_cache(&self) -> Option<Arc<dyn LlmResponseCache>> {
        read_recover(&self.llm_cache, "engine.llm_cache").clone()
    }

    /// 当前应用状态（首次配置 → 索引构建 → 就绪 / 降级）。
    pub fn current_state(&self) -> AppState {
        *lock_recover(&self.state, "engine.state")
    }

    /// 设置应用状态（状态变更记 info 日志，便于诊断流程卡点）。
    pub fn set_state(&self, state: AppState) {
        let old = {
            let mut guard = lock_recover(&self.state, "engine.state");
            let old = *guard;
            *guard = state;
            old
        };
        if old != state {
            tracing::info!(from = %old, to = %state, "应用状态变更");
        }
    }

    /// 生效配置快照（读锁内克隆 Arc，调用方在锁外使用）。
    pub fn config(&self) -> Arc<RamariaConfig> {
        read_recover(&self.config, "engine.config").clone()
    }

    /// 实际使用的配置文件路径（`from_parts` 构造时为空路径）。
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// 数据库文件路径（`from_parts` 构造时为空路径）。
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 底层 SQLite 连接池（未附着时为 None）；读锁内克隆句柄后释放锁。
    ///
    /// 用法:
    /// - 以 `&SqlitePool` 为入口的用例（如导入）在入口层取句柄使用；
    /// - `open_with` 装配路径自动携带，`from_parts` 注入路径需先
    ///   [`Engine::attach_sqlite_pool`]。
    pub fn sqlite_pool(&self) -> Option<SqlitePool> {
        read_recover(&self.pool, "engine.pool").clone()
    }

    /// 附着底层 SQLite 连接池（宿主 / 测试已持有连接池时复用）。
    ///
    /// 用法:
    /// - 注入构造（`from_parts`）不创建连接池；需要导入等连接的用例时，
    ///   由持有方在本方法附着（`open_with` 装配路径无需调用）。
    pub fn attach_sqlite_pool(&self, pool: SqlitePool) {
        let mut guard = write_recover(&self.pool, "engine.pool");
        *guard = Some(pool);
    }

    /// 检索索引是否已加载（懒加载占位状态）。
    pub fn is_retriever_loaded(&self) -> bool {
        read_recover(&self.retriever, "engine.retriever").is_some()
    }

    /// 检索器槽句柄（探针与诊断的只读访问使用）。
    ///
    /// 语义:
    /// - 返回懒加载槽的共享句柄（`Arc` 克隆，可在锁外跨任务使用）；
    ///   槽内为 `None` 表示索引尚未构建，调用方按只读用途适配
    ///   （如探针读取文档数 / 执行自检查询）；
    /// - 读写锁纪律：调用方在锁内只做同步操作，不跨 `.await` 持锁。
    pub fn retriever_slot(&self) -> Arc<RwLock<Option<Retriever>>> {
        Arc::clone(&self.retriever)
    }

    /// 关键词镜像句柄（探针与诊断的只读访问使用）。
    ///
    /// 语义:
    /// - 返回关键词镜像（倒排 + 词典池）的共享句柄（`Arc` 克隆，可在锁外跨任务使用）；
    ///   镜像内容随索引重建与 L1 增量维护，写入路径由索引用例持有；
    /// - 读写锁纪律：调用方在锁内只做同步操作，不跨 `.await` 持锁。
    pub fn keyword_mirror(&self) -> Arc<RwLock<KeywordService>> {
        Arc::clone(&self.keyword_mirror)
    }
}

// =========================================================
// 策略与钩子（入口层注入）
// =========================================================

impl Engine {
    /// 设置召回隐私与边界策略。
    ///
    /// 用法:
    /// - 入口层按需注入覆盖装配缺省（如按 `[mcp].allow_raw_text`、
    ///   `allowed_personas` 收紧口径）；未注入时使用 [`RecallPolicy::from_config`]
    ///   的配置映射缺省。
    ///
    /// 参数:
    /// - `policy`: 召回策略快照。
    pub fn set_recall_policy(&self, policy: RecallPolicy) {
        tracing::info!(
            allow_raw_text = policy.allow_raw_text,
            allowed_persona_rules = policy.allowed_personas.len(),
            "召回策略已更新"
        );
        let mut guard = write_recover(&self.recall_policy, "engine.recall_policy");
        *guard = policy;
    }

    /// 当前召回策略快照（副本）。
    pub fn recall_policy(&self) -> RecallPolicy {
        read_recover(&self.recall_policy, "engine.recall_policy").clone()
    }

    /// 注册封存钩子（行为 / 风格 / L2 触发；未注册的步骤在封存时跳过）。
    ///
    /// 用法:
    /// - 入口层启动时注入（可用 `default_seal_hooks` / `full_seal_hooks` 两套默认装配）；
    ///   未注册的步骤在封存时跳过（L1 / utt / examples 不受影响）。
    pub fn set_seal_hooks(&self, hooks: SealHooks) {
        tracing::info!(
            behavior = hooks.behavior.is_some(),
            style = hooks.style.is_some(),
            l2_trigger = hooks.l2_trigger.is_some(),
            "封存钩子已更新"
        );
        let mut guard = write_recover(&self.seal_hooks, "engine.seal_hooks");
        *guard = hooks;
    }

    /// 当前封存钩子快照（Arc 克隆，供封存流程在锁外调用）。
    pub fn seal_hooks(&self) -> SealHooks {
        read_recover(&self.seal_hooks, "engine.seal_hooks").clone()
    }

    /// 设置封存许可（入口层按配置注入，如 `[mcp].allow_seal`）。
    ///
    /// 语义:
    /// - `false`: 封存用例直接跳过（不关闭会话、不生成摘要、不消耗 LLM 做记忆加工）；
    ///   写用例（`ingest` / `chat_send`）仍可正常写入，超时会话留待允许封存的
    ///   宿主（桌面）或下次允许时的空闲检查处理；
    /// - `true`: 恢复默认行为（抢占式封存与空闲检查照常）。
    ///
    /// 用法:
    /// - 入口层启动时注入一次（进程级快照语义，与其它配置一致）。
    pub fn set_seal_allowed(&self, allowed: bool) {
        tracing::info!(allow_seal = allowed, "封存许可已更新（服务层门禁）");
        self.seal_allowed.store(allowed, Ordering::Release);
    }

    /// 当前封存许可（`false` = 只写不封存）。
    pub fn seal_allowed(&self) -> bool {
        self.seal_allowed.load(Ordering::Acquire)
    }

    /// 注册主动消息投递接收端（覆盖已有注册）。
    ///
    /// 说明:
    /// - 宿主启动时注入（桌面宿主实现系统通知与应用内转发）；
    /// - 未注册时调度静默丢弃（降级不阻塞）。
    pub fn set_proactive_sink(&self, sink: Arc<dyn ProactiveSink>) {
        tracing::info!("主动消息投递接收端已注册");
        let mut guard = write_recover(&self.proactive_sink, "engine.proactive_sink");
        *guard = Some(sink);
    }

    /// 当前注册的主动消息投递接收端（未注册 → None）。
    pub fn proactive_sink(&self) -> Option<Arc<dyn ProactiveSink>> {
        read_recover(&self.proactive_sink, "engine.proactive_sink").clone()
    }
}

// =========================================================
// 依赖热更新（后端配置 / 嵌入模型变更时整体替换快照）
// =========================================================

impl Engine {
    /// 热更新 LLM provider（后端配置变更后整体替换）。
    ///
    /// 参数:
    /// - `provider`: 已构造好的新 provider（线上 provider 需已注入 keychain 密钥来源）。
    ///
    /// 说明:
    /// - 并发读取方取到的是替换前或替换后的完整快照，不存在"半个 provider"的中间态；
    /// - 缓存实例不在本方法内替换（由 [`Engine::llm_cache`] 持有，构造新 provider 时复用）。
    pub fn update_llm(&self, provider: Arc<dyn LlmProvider>) {
        let new_name = provider.name();
        let old = {
            let mut guard = write_recover(&self.llm, "engine.llm");
            std::mem::replace(&mut *guard, provider)
        };
        tracing::info!(
            old_provider = old.name(),
            new_provider = new_name,
            "LLM provider 已热更新"
        );
    }

    /// 热更新嵌入 provider（加载 / 卸载模型后整体替换）。
    ///
    /// 参数:
    /// - `provider`: `Some` 为加载（向量通道就绪），`None` 为卸载（向量通道降级）。
    ///
    /// 说明:
    /// - 替换只影响后续召回与索引构建；既有内存索引在下一次懒加载刷新时重建
    ///   （与"跨进程写入后刷新"同一路径）。
    pub fn update_embedding(&self, provider: Option<Arc<dyn EmbeddingProvider>>) {
        match provider.as_ref() {
            Some(provider) => {
                let info = provider.model_info();
                tracing::info!(
                    model = %info.model_id,
                    dimension = info.dimension,
                    "嵌入模型已热更新（向量通道就绪）"
                );
            }
            None => tracing::info!("嵌入模型已卸载（向量通道降级，BM25 + 关键词镜像继续可用）"),
        }
        let mut guard = write_recover(&self.embedding, "engine.embedding");
        *guard = provider;
    }
}

// =========================================================
// 装配辅助（文件内私有）
// =========================================================

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// 默认配置文件路径：数据库同目录 `config.toml`。
fn default_config_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .map(|dir| dir.join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

/// 只读加载配置：读取 `config.toml` 并填充实际路径；缺失 / 解析失败回退默认值。
///
/// 说明:
/// - 装配路径不生成模板、不回写 DB（无副作用的只读装配；写入只经配置用例）。
/// - 路径字段以数据库所在目录为数据根填充（与入口层同一约定）。
fn load_config_readonly(config_path: &Path, db_path: &Path) -> RamariaConfig {
    let mut config = if config_path.exists() {
        match std::fs::read_to_string(config_path) {
            Ok(text) => match toml::from_str::<RamariaConfig>(&text) {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::warn!(
                        path = %path_log_label(config_path),
                        error = %e,
                        "config.toml 解析失败，回退默认配置"
                    );
                    RamariaConfig::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    path = %path_log_label(config_path),
                    error = %e,
                    "config.toml 读取失败，回退默认配置"
                );
                RamariaConfig::default()
            }
        }
    } else {
        tracing::debug!(
            path = %path_log_label(config_path),
            "config.toml 不存在，使用默认配置"
        );
        RamariaConfig::default()
    };

    let data_dir = db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    config.paths.data_dir = data_dir.to_string_lossy().into_owned();
    config.paths.log_dir = data_dir.join("logs").to_string_lossy().into_owned();
    config.paths.config_dir = data_dir.to_string_lossy().into_owned();
    config.paths.vector_index_dir = data_dir.join("vectors").to_string_lossy().into_owned();
    config
}

/// 按后端配置构建 LLM provider（可选注入响应缓存）。
///
/// 参数:
/// - `backend_config`: 后端配置（provider / base_url / model）。
/// - `keychain`: 线上 provider 的 API key 来源（本地 provider 不使用）。
/// - `cache`: 精确缓存（`[cache].enabled=false` 时为 None）。
///
/// 说明:
/// - 装配与后端配置热更新共用本函数，保证两处的 provider 构造口径与缓存注入条件一致。
pub(crate) fn build_llm_provider(
    backend_config: &BackendConfig,
    keychain: &Arc<Keychain>,
    cache: Option<Arc<dyn LlmResponseCache>>,
) -> RamariaResult<Arc<dyn LlmProvider>> {
    let provider: Arc<dyn LlmProvider> = match backend_config.provider {
        LlmProviderKind::LmStudio => {
            let provider = ramaria_llm::lm_studio::LmStudioProvider::new(backend_config.clone())?;
            match cache {
                Some(cache) => Arc::new(provider.with_cache(cache)),
                None => Arc::new(provider),
            }
        }
        LlmProviderKind::DeepSeek => {
            let provider = ramaria_llm::deepseek::DeepSeekProvider::new(
                backend_config.clone(),
                Arc::clone(keychain),
            )?;
            match cache {
                Some(cache) => Arc::new(provider.with_cache(cache)),
                None => Arc::new(provider),
            }
        }
        LlmProviderKind::OpenAI => {
            let provider = ramaria_llm::openai::OpenAIProvider::new(
                backend_config.clone(),
                Arc::clone(keychain),
            )?;
            match cache {
                Some(cache) => Arc::new(provider.with_cache(cache)),
                None => Arc::new(provider),
            }
        }
        // non_exhaustive 兜底：未知 provider 显式报错，不静默退化
        other => {
            return Err(RamariaError::unsupported(format!(
                "不支持的 LLM provider: {}",
                other.as_str()
            )));
        }
    };
    Ok(provider)
}

/// 尝试恢复已保存的嵌入模型；缺失 / 加载失败返回 None（向量通道降级）。
///
/// 说明:
/// - 模型目录存在但加载失败（文件损坏 / 设备不可用）同样降级，
///   记 warn 不阻塞装配（BM25 + 关键词镜像继续工作）。
fn restore_embedding(
    backend_config: &BackendConfig,
    device: EmbeddingDevice,
) -> Option<Arc<dyn EmbeddingProvider>> {
    let saved_path = backend_config
        .embedding_model_path
        .as_deref()
        .filter(|path| !path.is_empty())?;

    let model_dir = Path::new(saved_path);
    if !model_dir.exists() {
        tracing::warn!(
            path = %path_log_label(model_dir),
            "已保存的嵌入模型目录不存在，向量通道降级（BM25 + 关键词镜像继续可用）"
        );
        return None;
    }

    match ramaria_llm::embedding::native::create_native_provider_with_device(model_dir, device) {
        Ok(provider) => {
            let info = provider.model_info();
            tracing::info!(
                path = %path_log_label(model_dir),
                model_id = %info.model_id,
                dim = info.dimension,
                device = device.as_str(),
                "已恢复嵌入模型（向量通道可用）"
            );
            Some(Arc::new(provider) as Arc<dyn EmbeddingProvider>)
        }
        Err(e) => {
            tracing::warn!(
                path = %path_log_label(model_dir),
                error = %e,
                "加载已保存的嵌入模型失败，向量通道降级（BM25 + 关键词镜像继续可用）"
            );
            None
        }
    }
}
