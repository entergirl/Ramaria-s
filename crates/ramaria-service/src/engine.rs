//! crates/ramaria-service/src/engine.rs - 服务层引擎装配与用例挂载点
//!
//! 设计特点:
//! - `Engine` 自持依赖装配：storage（连接池 + migration）→ 配置（只读）→ LLM → 嵌入（可选）→ 检索占位
//! - 与传输无关：不依赖 app / cli / desktop / tauri，不持有界面或协议概念
//! - 配置纪律：config.toml 为配置权威源（桌面 / CLI 双写同步以文件为准），本层只读不写回
//! - 热更新：LLM 与嵌入 provider 以 `RwLock` 持有快照，后端配置 / 嵌入模型变更时整体替换；
//!   读取路径取克隆后在锁外使用，异步代码不跨 `.await` 持锁
//! - 降级链：嵌入模型缺失 → 向量通道不可用（BM25 + 关键词镜像继续工作），不阻塞装配
//! - 懒加载：检索索引在首次召回时构建，本层仅持有占位槽（避免进程启动即加载大库）；
//!   占位槽未加载期间产生的 L1 增量会置脏标记，保证下次加载重建不漏（见 `index_dirty`）
//! - 重建节流：跨进程代次变化触发的重建受 `[index].refresh_interval_seconds` 约束
//!   （0 = 不节流，见 `index_rebuild_cooldown_elapsed`）
//! - 宿主后台任务：进程内空闲检查循环由入口层拉起（`spawn_idle_loop`），退出时优雅关停
//! - 用例挂载点：recall / ingest / seal / tick_idle / history / persona / 模型管理均由用例实现接入

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::config::{EmbeddingDevice, RamariaConfig};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{lock_recover, read_recover, write_recover};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::{
    AppState, BackendConfig, LlmProvider as LlmProviderKind, MemoryL1, now_ms,
};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::keyword::KeywordService;
use ramaria_memory::retriever::Retriever;
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

use crate::idle::{IdleLoop, IdleLoopOptions};
use crate::index::IndexStamp;
use crate::lifecycle::{Lifecycle, LifecycleOptions};
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;
use crate::types::{
    ChatSendOutcome, ChatSendRequest, DegradedReason, EmbeddingModelView, EmbeddingValidation,
    HistoryRequest, HistoryResult, IngestOutcome, IngestRequest, PersonaCardRequest,
    PersonaCardView, PersonaSummaryView, RecallRequest, RecallResult, SealOutcome, SetupRequest,
    SetupStatus,
};

// =========================================================
// 装配选项
// =========================================================

/// 引擎装配选项。
///
/// 字段约定:
/// - `db_path`: 数据库文件路径（不存在时自动创建目录与库并执行 migration）。
/// - `config_path`: 配置文件路径；缺省取数据库同目录 `config.toml`。
#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub db_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

impl EngineOptions {
    /// 以数据库路径创建选项（配置路径缺省）。
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            config_path: None,
        }
    }

    /// 指定配置文件路径（链式调用）。
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }
}

// =========================================================
// 引擎
// =========================================================

/// 记忆与对话服务引擎。
///
/// 职责:
/// - 持有一整套与传输无关的依赖：存储后端、LLM provider、嵌入 provider、生效配置、检索器占位。
/// - 为入口层（MCP / CLI / 桌面 / 未来社交通道）提供统一用例入口。
///
/// 生命周期:
/// - 由入口层在进程启动时装配（[`Engine::open`]），进程退出时随 Arc 释放。
/// - 存储连接池（含 WAL 与 migration）随引擎创建，无需调用方管理。
///
/// 并发约定:
/// - `Engine` 自身为 `Send + Sync`，可放入 `Arc` 跨任务共享。
/// - 检索器槽为 `RwLock<Option<..>>`：检索读多写少，懒加载在首次召回时写入。
/// - LLM / 嵌入 provider 为 `RwLock` 快照：热更新整体替换，读取方取克隆后在锁外使用
///   （异步路径不持有锁跨 `.await`）。
pub struct Engine {
    /// 存储后端（业务 CRUD + 基础设施）。
    storage: Arc<dyn StorageBackend>,
    /// 当前 LLM provider 快照（按 DB 侧 backend_config 装配；后端配置变更时整体替换）。
    llm: RwLock<Arc<dyn LlmProvider>>,
    /// 嵌入模型 provider 快照（None = 向量通道降级，BM25 + 关键词镜像继续可用）。
    embedding: RwLock<Option<Arc<dyn EmbeddingProvider>>>,
    /// OS keychain（线上 provider 的 API key 来源；首次配置与后端配置热更新共用同一实例）。
    keychain: Arc<Keychain>,
    /// LLM 响应精确缓存（`[cache].enabled=false` 时为 None）。
    /// 热更新 provider 时复用同一实例，保证切换后端后既有缓存不失效。
    llm_cache: RwLock<Option<Arc<dyn LlmResponseCache>>>,
    /// 应用状态机（首次配置 → 索引构建 → 就绪 / 降级）。
    state: Mutex<AppState>,
    /// 生效配置（config.toml 为权威源；本层只读不写回）。
    config: RamariaConfig,
    /// 数据库文件路径（诊断与客户端配置片段展示用）。
    db_path: PathBuf,
    /// 内存检索器槽（懒加载：首次召回时由 `ensure_index_loaded` 构建并整体替换）。
    retriever: Arc<RwLock<Option<Retriever>>>,
    /// 关键词镜像（倒排 + 词典池）：召回第四通道，L1 生成后增量维护。
    keyword_mirror: Arc<RwLock<KeywordService>>,
    /// 索引脏标记：L1 增量镜像时检索器尚未加载 → 置脏，
    /// 保证下次加载（`ensure_index_loaded`）会重建，不在进程生命周期内漏掉该 L1。
    index_dirty: Arc<AtomicBool>,
    /// 索引代次快照（构建时记录；召回前与库内比对 → 其他进程写入后刷新内存索引）。
    index_stamp: Arc<RwLock<Option<IndexStamp>>>,
    /// 内存索引最近一次构建完成时间（Unix 毫秒；0 = 尚未构建）。
    /// 用途：`[index].refresh_interval_seconds` 生效时限制两次重建的最小间隔
    /// （写入密集期抑制整库重建风暴，代价是刷新延迟不超过该间隔）。
    last_index_build_ms: Arc<AtomicI64>,
    /// 行为层待定池（跨会话内存态：行为增量编排的归簇状态，与 app 侧同一机制）。
    behavior_pending: Arc<Mutex<PendingPool>>,
    /// 召回隐私与边界策略（默认保守；入口层按 `[mcp]` 配置注入）。
    recall_policy: Arc<RwLock<RecallPolicy>>,
    /// 封存钩子（行为 / 风格 / L2 触发；未注册则跳过，见 [`SealHooks`]）。
    seal_hooks: Arc<RwLock<SealHooks>>,
    /// 封存许可（服务层策略）：`false` 时禁止一切封存与摘要生成。
    ///
    /// 说明:
    /// - 语义对应入口层配置（如 `[mcp].allow_seal`，D-V21-009：「只写不封存」）；
    /// - 默认 `true`：桌面 / CLI / 测试路径行为不变；
    /// - 门禁落在 `seal` 用例与空闲检查入口，避免「只写不封存」被惰性体检
    ///   （续写超时会话先封存）或空闲循环绕过。
    seal_allowed: AtomicBool,
}

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

    /// 检索器懒加载槽（crate 内用例实现使用）。
    pub(crate) fn retriever_slot(&self) -> &Arc<RwLock<Option<Retriever>>> {
        &self.retriever
    }

    /// 关键词镜像（crate 内用例实现使用）。
    pub(crate) fn keyword_mirror_ref(&self) -> &Arc<RwLock<KeywordService>> {
        &self.keyword_mirror
    }

    /// 行为层待定池（crate 内编排与测试使用）。
    pub(crate) fn behavior_pending_ref(&self) -> &Arc<Mutex<PendingPool>> {
        &self.behavior_pending
    }
}

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
            db = %db_path.display(),
            provider = %llm.name(),
            embedding = embedding.is_some(),
            "服务层引擎装配完成"
        );

        Ok(Self {
            storage,
            llm: RwLock::new(llm),
            embedding: RwLock::new(embedding),
            keychain,
            llm_cache: RwLock::new(cache),
            behavior_pending: Arc::new(Mutex::new(PendingPool::new(&config.behavior))),
            config,
            db_path,
            // ---- 6. 检索器占位：首次召回时构建（懒加载）----
            retriever: Arc::new(RwLock::new(None)),
            // ---- 7. 关键词镜像与策略 / 钩子：空镜像 + 默认保守策略 ----
            keyword_mirror: Arc::new(RwLock::new(KeywordService::new())),
            index_dirty: Arc::new(AtomicBool::new(false)),
            index_stamp: Arc::new(RwLock::new(None)),
            last_index_build_ms: Arc::new(AtomicI64::new(0)),
            recall_policy: Arc::new(RwLock::new(RecallPolicy::default())),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
            seal_allowed: AtomicBool::new(true),
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
        Self {
            storage,
            llm: RwLock::new(llm),
            embedding: RwLock::new(embedding),
            keychain: Arc::new(Keychain::new()),
            llm_cache: RwLock::new(None),
            behavior_pending: Arc::new(Mutex::new(PendingPool::new(&config.behavior))),
            config,
            db_path: PathBuf::new(),
            retriever: Arc::new(RwLock::new(None)),
            keyword_mirror: Arc::new(RwLock::new(KeywordService::new())),
            index_dirty: Arc::new(AtomicBool::new(false)),
            index_stamp: Arc::new(RwLock::new(None)),
            last_index_build_ms: Arc::new(AtomicI64::new(0)),
            recall_policy: Arc::new(RwLock::new(RecallPolicy::default())),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
            seal_allowed: AtomicBool::new(true),
            state: Mutex::new(AppState::NeedsSetup),
        }
    }

    // =========================================================
    // 依赖访问器
    // =========================================================

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

    /// 生效配置引用（只读）。
    pub fn config(&self) -> &RamariaConfig {
        &self.config
    }

    /// 数据库文件路径（`from_parts` 构造时为空路径）。
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 检索索引是否已加载（懒加载占位状态）。
    pub fn is_retriever_loaded(&self) -> bool {
        read_recover(&self.retriever, "engine.retriever").is_some()
    }

    // =========================================================
    // 策略与钩子（入口层注入）
    // =========================================================

    /// 设置召回隐私与边界策略。
    ///
    /// 用法:
    /// - 入口层（MCP / CLI / 桌面）在启动时按配置注入（`[mcp].allow_raw_text`、
    ///   `allowed_personas` 等），未注入时保持默认保守值（原文不出端）。
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
    /// - 入口层启动时注入：桌面 / CLI 复用既有 App 侧实现；MCP 进程未注册时
    ///   仅跳过对应步骤（L1 / utt / examples 不受影响）。
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

    // =========================================================
    // 用例入口
    // =========================================================

    /// 召回用例：按对话片段与分层选择装配可直接使用的记忆上下文。
    ///
    /// 职责:
    /// - 检索（向量 / BM25 / 关键词镜像 / 图谱，与在线管线同一份实现）→ Persona-Aware 过滤
    ///   → 衰减重排 → 分层装配（行为 / 知识 / 表达 / 脉络 / 记忆 / 原文）→ 预算裁剪。
    /// - `query` 与 `messages` 均为空时进入概览模式（时间线返回最近记忆）。
    ///
    /// 返回:
    /// - 成功时返回 `context` / `items` / `stats`。
    /// - 目标人格不在策略白名单时返回 `Privacy` 错误（越权可见性拒绝）。
    pub async fn recall(&self, req: RecallRequest) -> RamariaResult<RecallResult> {
        crate::recall::run(self, req).await
    }

    /// 生成用例：以指定人格回复一条消息（记忆检索 + 五段式装配 + LLM）。
    ///
    /// 职责:
    /// - 与在线管线同源：记忆上下文走共用召回、系统 Prompt 走共用装配（含脉络 / 行为 /
    ///   知识 / 示例素材），生成后把用户消息与助手回复一并落库。
    ///
    /// 返回:
    /// - 成功时返回 `reply` / `session_id` / `chars`。
    pub async fn chat_send(&self, req: ChatSendRequest) -> RamariaResult<ChatSendOutcome> {
        crate::chat::run(self, req).await
    }

    /// 写入用例：把外部对话回流入库（进 L0），使内容在桌面可见并参与后续记忆加工。
    ///
    /// 职责:
    /// - 会话解析（显式标识 > 单流退化）→ 惰性封存体检 → 重发跳过 + 指纹去重落库
    ///   → 可选封存（`finalize`）。
    ///
    /// 返回:
    /// - 成功时返回 `session_id` / `written` / `deduplicated` / `finalized`。
    pub async fn ingest(&self, req: IngestRequest) -> RamariaResult<IngestOutcome> {
        crate::ingest::run(self, req).await
    }

    /// 封存用例：抢占式关闭会话并触发封存链路（L1 → 索引镜像 → utt → examples → 钩子）。
    ///
    /// 职责:
    /// - 条件更新抢占（`ended_at IS NULL`）；仅抢到者生成 L1，未抢到直接返回
    ///   （多进程 / 多线程同时封存时保证 L1 只生成一次）。
    ///
    /// 返回:
    /// - 成功时返回 `sealed` 与本次生成的 `l1_count`。
    pub async fn seal(&self, session_id: Uuid) -> RamariaResult<SealOutcome> {
        crate::seal::run(self, session_id).await
    }

    /// 空闲检查用例：遍历全库活跃会话，对超时者执行封存。
    ///
    /// 返回:
    /// - 成功时返回本次封存的会话数量（抢占失败者不计入）。
    pub async fn tick_idle(&self) -> RamariaResult<usize> {
        crate::idle::tick(self).await
    }

    /// 会话历史用例：按会话或人格读取消息历史（分页）。
    ///
    /// 返回:
    /// - 成功时返回 `messages`（页内时间正序）与分页前的 `total`。
    pub async fn history(&self, req: HistoryRequest) -> RamariaResult<HistoryResult> {
        crate::session::history(self, req).await
    }

    /// 人格列表用例：列出全部人格摘要（uid / 名称 / 类型 / 来源 / 启用状态）。
    pub async fn persona_list(&self) -> RamariaResult<Vec<PersonaSummaryView>> {
        crate::persona::list(self).await
    }

    /// 人格卡片用例：性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度。
    pub async fn persona_card(&self, req: PersonaCardRequest) -> RamariaResult<PersonaCardView> {
        crate::persona::card(self, req).await
    }

    /// 确保检索索引已加载（懒加载：首次召回前构建一次，重复调用为空操作）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次调用完成了构建。
    /// - `Ok(false)`: 索引此前已加载（或无需构建）。
    pub async fn ensure_index_loaded(&self) -> RamariaResult<bool> {
        crate::index::ensure_loaded(self).await
    }

    // =========================================================
    // 记忆管线手动触发
    // =========================================================

    /// 手动触发 L2 事件提取检查（全 persona 扫描 + L3 级联）。
    ///
    /// 用法:
    /// - 宿主手动触发（批量导入后补检查等场景），与后台调度共用同一份实现；
    /// - 内部失败只记日志，不向调用方抛错。
    pub async fn trigger_l2_check(&self) {
        let storage = self.storage_ref().as_ref();

        tracing::info!("trigger_l2_check: 开始遍历 persona...");

        // L1 → L2（仅检查未吸收 L1）
        crate::lifecycle::l2_l3::check_l2_trigger(self, None).await;

        // L2 → L3（独立检查未吸收事件，即使 L1 已全部吸收）
        let personas = match storage.list_personas().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "trigger_l2_check: 查询 persona 列表失败，跳过 L3");
                return;
            }
        };

        for persona in &personas {
            let unabsorbed_events = match storage.list_unabsorbed_events(&persona.uid).await {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(persona_uid = %persona.uid, error = %e, "查询未吸收事件失败");
                    continue;
                }
            };

            tracing::info!(
                persona_uid = %persona.uid,
                persona_name = %persona.name,
                unabsorbed_event_count = unabsorbed_events.len(),
                "检查 L3 触发条件"
            );

            crate::lifecycle::l2_l3::check_l3_trigger(self, None, &persona.uid).await;
        }
    }

    /// 手动触发指定 persona 的 L3 性格推断检查。
    ///
    /// 用法:
    /// - 宿主手动触发（批量导入后补检查等场景），与后台调度共用同一份实现；
    /// - 未吸收事件达到阈值（或最早事件超龄）时执行推断，否则直接返回；
    /// - 内部失败只记日志，不向调用方抛错。
    pub async fn trigger_l3_check(&self, persona_uid: &str) {
        crate::lifecycle::l2_l3::check_l3_trigger(self, None, persona_uid).await;
    }

    // =========================================================
    // L1 摘要手动重生成与补扫
    // =========================================================

    /// 为指定会话重新生成单段 L1 摘要（手动重试，末尾触发 L2 检查）。
    ///
    /// 用法:
    /// - 供封存中 L1 生成失败后的手动补救；会话可已关闭，也可仍在活跃中；
    /// - 单段口径：即使开启渐进式配置也按单段生成（与封存路径口径不同）。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: 生成成功（已写库并增量镜像，可立即召回）；
    /// - `Ok(None)`: 会话无消息（跳过）。
    pub async fn regenerate_l1(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<MemoryL1>> {
        crate::lifecycle::l1::regenerate_l1(
            self,
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        )
        .await
    }

    /// 生成单段 L1 摘要但不触发 L2 级联（幂等；供批量导入场景）。
    ///
    /// 用法:
    /// - 与 [`Engine::regenerate_l1`] 相同，但跳过末尾 L2 检查；
    ///   调用方应在全部 L1 生成完成后自行触发级联；
    /// - 幂等：目标 persona 已有 L1 时不重复生成（返回 `Ok(None)`）。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: 本次生成成功；
    /// - `Ok(None)`: 会话无消息，或已有目标 persona 的 L1（跳过）。
    pub async fn regenerate_l1_no_cascade(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<MemoryL1>> {
        crate::lifecycle::l1::regenerate_l1_no_cascade(
            self,
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        )
        .await
    }

    /// 为指定会话重新生成 L1 摘要（渐进式感知口径）。
    ///
    /// 用法:
    /// - 与封存路径口径一致：`[l1.progressive]` 开启且会话触发阈值（消息数 / 时间跨度）时
    ///   按段生成多条 L1，未触发时回退单段摘要；末尾触发 L2 检查。
    ///
    /// 返回:
    /// - `Ok(l1_list)`: 本次生成的全部段 L1（未触发渐进时为 1 条）；
    /// - `Ok(vec![])`: 会话无消息。
    pub async fn regenerate_l1_progressive(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Vec<MemoryL1>> {
        crate::lifecycle::l1::regenerate_l1_progressive(
            self,
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        )
        .await
    }

    /// 补扫封存失败遗留的 L1 摘要任务（宿主启动与定时消费点）。
    ///
    /// 返回:
    /// - 本轮成功补跑出 L1 摘要的任务数。
    pub async fn retry_pending_l1_jobs(&self) -> usize {
        crate::lifecycle::l1::retry_pending_l1_jobs(self).await
    }

    // =========================================================
    // 宿主后台任务
    // =========================================================

    /// 启动进程内空闲检查循环（超时会话按 `[session].l1_idle_minutes` 触发封存）。
    ///
    /// 用法:
    /// - 仅需"超时会话自动封存"的轻量宿主（MCP 服务端等）启动时拉起，退出时
    ///   [`IdleLoop::shutdown`] 优雅关停；
    /// - 需要活跃指针与 L2/L3 调度的宿主改用 [`Engine::start_lifecycle`]（同一份空闲检查实现）；
    /// - 封存消耗 LLM 并改变记忆状态：入口层可自行按配置门禁决定是否拉起（如 `[mcp].allow_seal`）。
    ///
    /// 返回:
    /// - 循环句柄；drop 或 [`IdleLoop::shutdown`] 均会置停止位。
    pub fn spawn_idle_loop(self: &Arc<Self>) -> IdleLoop {
        let options = IdleLoopOptions::from_config(&self.config);
        IdleLoop::spawn(Arc::clone(self), options)
    }

    /// 以显式选项启动空闲检查循环（测试与需要非配置间隔的宿主使用）。
    ///
    /// 参数:
    /// - `options`: 循环选项（间隔秒数）。生产路径应走
    ///   [`IdleLoopOptions::from_config`]（含下限夹取，避免热循环）。
    pub fn spawn_idle_loop_with(self: &Arc<Self>, options: IdleLoopOptions) -> IdleLoop {
        IdleLoop::spawn(Arc::clone(self), options)
    }

    /// 拉起会话生命周期（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）。
    ///
    /// 用法:
    /// - 长驻宿主启动时按选项拉起（见 [`LifecycleOptions`]：桌面 / MCP / 单次执行），
    ///   退出时调用 [`Lifecycle::shutdown`] 优雅关停；
    /// - 仅需空闲封存的轻量宿主可继续使用 [`Engine::spawn_idle_loop`]。
    ///
    /// 返回:
    /// - 生命周期容器句柄（持有引擎，引擎不反向持有容器，避免引用环）。
    pub fn start_lifecycle(self: &Arc<Self>, options: LifecycleOptions) -> Arc<Lifecycle> {
        Lifecycle::start(Arc::clone(self), options)
    }

    // =========================================================
    // 依赖热更新（后端配置 / 嵌入模型变更时整体替换快照）
    // =========================================================

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

    // =========================================================
    // 首次配置用例（状态机推进与缺项诊断）
    // =========================================================

    /// 读取首次配置缺项诊断（后端配置 / 模型选择 / 索引 / 嵌入四项）。
    pub async fn check_setup_status(&self) -> RamariaResult<SetupStatus> {
        crate::setup::check(self).await
    }

    /// 执行首次配置：密钥入 keychain → 后端配置落库 → provider 热替换 → 健康探测 → 推进状态机。
    ///
    /// 返回:
    /// - 探测通过时返回按缺项诊断判定的状态；全部失败返回 `Degraded`（不报错）。
    pub async fn run_setup(&self, req: &SetupRequest) -> RamariaResult<AppState> {
        crate::setup::run(self, req).await
    }

    /// 刷新应用状态（索引构建完成 / 嵌入热加载 / 配置变更后调用）。
    pub async fn refresh_setup_state(&self) -> RamariaResult<AppState> {
        crate::setup::refresh(self).await
    }

    /// 探测当前 LLM 后端可达性（最多 3 次、间隔 2 秒）。
    ///
    /// 返回:
    /// - `true`: 至少一次探测通过；`false`: 全部失败。
    ///
    /// 用途:
    /// - 入口层的「测试连接」动作；与首次配置使用的探测实现同一份（重试口径一致）。
    pub async fn probe_llm_health(&self) -> bool {
        let llm = self.llm_ref();
        crate::setup::probe_health_with_retry(
            llm.as_ref(),
            crate::setup::HEALTH_PROBE_ATTEMPTS,
            crate::setup::HEALTH_PROBE_INTERVAL_SECONDS,
        )
        .await
    }

    // =========================================================
    // 模型管理用例（后端配置 / 嵌入模型）
    // =========================================================

    /// 更新 LLM 后端配置并热加载 provider（写入 keychain → 落库 → 重建 → 替换）。
    ///
    /// 参数:
    /// - `config`: 新的后端配置（provider / base_url / model / 嵌入路径等）。
    /// - `api_key`: 可选的线上 provider 密钥；`None` 或空白表示不更新密钥。
    ///
    /// 返回:
    /// - 成功时返回 `Ok(())`，此后读取路径取到新 provider。
    pub async fn update_backend_config(
        &self,
        config: &BackendConfig,
        api_key: Option<&str>,
    ) -> RamariaResult<()> {
        crate::model::update_backend_config(self, config, api_key).await
    }

    /// 校验指定目录能否作为嵌入模型使用（无副作用探测）。
    ///
    /// 返回:
    /// - `valid=false` + `reason` 表达目录缺失 / 加载失败 / 推理失败，不抛错。
    pub async fn validate_embedding_model(&self, path: &str) -> RamariaResult<EmbeddingValidation> {
        crate::model::validate_embedding_model(path, self.config.embedding.device).await
    }

    /// 保存嵌入模型配置并热加载（`None` 或空白路径 = 卸载）。
    ///
    /// 返回:
    /// - 成功时内存 provider 与持久化路径同时生效；加载失败时保持原状态不变。
    pub async fn save_embedding_model(&self, path: Option<&str>) -> RamariaResult<()> {
        crate::model::save_embedding_model(self, path).await
    }

    /// 读取当前嵌入模型配置（已加载 → 维度 / 可用性；未加载 → 配置中的路径）。
    pub async fn embedding_model(&self) -> RamariaResult<Option<EmbeddingModelView>> {
        crate::model::embedding_model(self).await
    }

    /// 读取当前降级原因（非 `Degraded` 状态返回 `None`）。
    pub async fn degraded_reason(&self) -> RamariaResult<Option<DegradedReason>> {
        crate::model::degraded_reason(self).await
    }

    // =========================================================
    // 索引脏标记（懒加载与增量镜像的协同）
    // =========================================================

    /// 标记索引需要重建（增量镜像时检索器尚未加载 → 该批 L1 未进内存索引）。
    pub(crate) fn mark_index_dirty(&self) {
        self.index_dirty.store(true, Ordering::Release);
    }

    /// 查询索引是否需要重建。
    pub(crate) fn index_dirty(&self) -> bool {
        self.index_dirty.load(Ordering::Acquire)
    }

    /// 清除索引脏标记（重建开始前调用：构建期间新产生的增量会重新置脏）。
    pub(crate) fn clear_index_dirty(&self) {
        self.index_dirty.store(false, Ordering::Release);
    }

    /// 记录索引代次快照（索引构建完成后调用）。
    ///
    /// 说明:
    /// - 记录的是**构建前**读取的库内快照：构建窗口内其他进程新写入的内容
    ///   会使下次比对不等 → 再刷新一次（收敛，不漏新记忆）。
    pub(crate) fn record_index_stamp(&self, stamp: IndexStamp) {
        let mut guard = write_recover(&self.index_stamp, "engine.index_stamp");
        *guard = Some(stamp);
    }

    /// 当前已记录的索引代次快照（索引未构建过时为 None）。
    pub(crate) fn index_stamp(&self) -> Option<IndexStamp> {
        *read_recover(&self.index_stamp, "engine.index_stamp")
    }

    /// 记录内存索引最近一次构建完成时间（索引构建成功后调用）。
    pub(crate) fn record_index_build_time(&self, at_ms: i64) {
        self.last_index_build_ms.store(at_ms, Ordering::Release);
    }

    /// 内存索引最近一次构建完成时间（Unix 毫秒；0 = 尚未构建）。
    pub(crate) fn last_index_build_time(&self) -> i64 {
        self.last_index_build_ms.load(Ordering::Acquire)
    }

    /// 判断当前是否允许重建内存索引（`[index].refresh_interval_seconds` 冷却窗口）。
    ///
    /// 语义:
    /// - 允许重建：间隔配置为 0（不节流）、从未构建过、或距上次构建已完成超过间隔；
    /// - 不允许：冷却窗口内——本次沿用现有索引，窗口过后的下一次召回补上重建
    ///   （用于写入密集期抑制整库重建风暴；不适用于首次加载与同进程脏标记路径）。
    pub(crate) fn index_rebuild_cooldown_elapsed(&self) -> bool {
        let interval_seconds = self.config.index.refresh_interval_seconds;
        if interval_seconds == 0 {
            return true;
        }
        let last_build_ms = self.last_index_build_time();
        if last_build_ms == 0 {
            return true;
        }
        now_ms().saturating_sub(last_build_ms) >= interval_seconds as i64 * 1_000
    }
}

// =========================================================
// 装配辅助（文件内私有）
// =========================================================

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
/// - 与桌面 / CLI 的双写同步不同，本层不生成模板、不回写 DB（无副作用的只读装配）。
/// - 路径字段以数据库所在目录为数据根填充（与 CLI / 桌面同一约定）。
fn load_config_readonly(config_path: &Path, db_path: &Path) -> RamariaConfig {
    let mut config = if config_path.exists() {
        match std::fs::read_to_string(config_path) {
            Ok(text) => match toml::from_str::<RamariaConfig>(&text) {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::warn!(
                        path = %config_path.display(),
                        error = %e,
                        "config.toml 解析失败，回退默认配置"
                    );
                    RamariaConfig::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    path = %config_path.display(),
                    error = %e,
                    "config.toml 读取失败，回退默认配置"
                );
                RamariaConfig::default()
            }
        }
    } else {
        tracing::debug!(
            path = %config_path.display(),
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
            path = %saved_path,
            "已保存的嵌入模型目录不存在，向量通道降级（BM25 + 关键词镜像继续可用）"
        );
        return None;
    }

    match ramaria_llm::embedding::native::create_native_provider_with_device(model_dir, device) {
        Ok(provider) => {
            let info = provider.model_info();
            tracing::info!(
                path = %saved_path,
                model_id = %info.model_id,
                dim = info.dimension,
                device = device.as_str(),
                "已恢复嵌入模型（向量通道可用）"
            );
            Some(Arc::new(provider) as Arc<dyn EmbeddingProvider>)
        }
        Err(e) => {
            tracing::warn!(
                path = %saved_path,
                error = %e,
                "加载已保存的嵌入模型失败，向量通道降级（BM25 + 关键词镜像继续可用）"
            );
            None
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::config::RamariaConfig as TestConfig;
    use ramaria_core::traits::StoreInfrastructure;

    /// 创建唯一临时目录（测试结束前由调用方清理）。
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应可读")
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("ramaria-service-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
        dir
    }

    /// 无 config.toml 时：使用默认配置、LLM 回退 LM Studio、嵌入降级不报错。
    #[tokio::test]
    async fn open_without_config_uses_defaults_and_degrades_embedding() {
        let dir = temp_dir("defaults");
        let db_path = dir.join("assistant.db");

        let engine = Engine::open(db_path.clone()).await.expect("引擎装配应成功");

        // 存储可用（空库也可正常查询）
        assert!(
            engine.storage().list_personas().await.is_ok(),
            "装配后存储后端应可查询"
        );
        // 无后端配置记录 → 回退 LM Studio 默认
        assert_eq!(engine.llm().name(), "LM Studio");
        // 无嵌入模型 → 降级但装配成功
        assert!(
            !engine.is_embedding_available(),
            "无嵌入模型时向量通道应降级"
        );
        // 检索器懒加载占位：装配后未加载
        assert!(!engine.is_retriever_loaded(), "装配阶段不应加载检索索引");
        // 默认配置生效
        assert_eq!(engine.config().session.l1_idle_minutes, 10);
        assert_eq!(engine.db_path(), db_path.as_path());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// config.toml 存在时：按文件生效，且本层不写回（只读装配）。
    #[tokio::test]
    async fn open_reads_config_toml_readonly() {
        let dir = temp_dir("config");
        let db_path = dir.join("assistant.db");
        std::fs::write(
            dir.join("config.toml"),
            "[session]\nl1_idle_minutes = 25\n\n[utt]\ntheta_gap_minutes = 45\n",
        )
        .expect("写入 config.toml 应成功");
        let before = std::fs::read_to_string(dir.join("config.toml")).expect("读取配置应成功");

        let engine = Engine::open(db_path).await.expect("引擎装配应成功");
        assert_eq!(
            engine.config().session.l1_idle_minutes,
            25,
            "config.toml 的 [session] 必须被服务层读取"
        );
        assert_eq!(
            engine.config().utt.theta_gap_minutes,
            45,
            "config.toml 的 [utt] 必须被服务层读取"
        );

        // 只读纪律：装配过程不得改写配置文件
        let after = std::fs::read_to_string(dir.join("config.toml")).expect("读取配置应成功");
        assert_eq!(before, after, "服务层装配不得写回 config.toml");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DB 侧 backend_config 记录被采用（验证后端配置来源为数据库）。
    #[tokio::test]
    async fn open_uses_saved_backend_config() {
        let dir = temp_dir("backend");
        let db_path = dir.join("assistant.db");

        // 预写 DB：LM Studio 自定义 base_url（与默认不同，用于断言读取生效）
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
            .await
            .expect("初始化测试库应成功");
        let storage = SqliteStorage::new(pool.clone());
        let mut backend = BackendConfig::lm_studio_default();
        backend.base_url = "http://localhost:9999/v1".to_string();
        backend.capability.base_url = "http://localhost:9999/v1".to_string();
        storage
            .save_backend_config(&backend)
            .await
            .expect("保存后端配置应成功");
        pool.close().await;

        let engine = Engine::open(db_path).await.expect("引擎装配应成功");
        assert_eq!(engine.llm().name(), "LM Studio");
        assert_eq!(
            engine.llm().config().base_url,
            "http://localhost:9999/v1",
            "LLM provider 应使用 DB 侧 backend_config"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 用例入口可达：空库上的读用例返回结构完整的结果，写用例按边界显式报错。
    #[tokio::test]
    async fn use_cases_are_reachable_on_empty_db() {
        let dir = temp_dir("usecases");
        let db_path = dir.join("assistant.db");
        let engine = Engine::open(db_path).await.expect("引擎装配应成功");

        // 召回：空库 → 空结果（不报错），且进入概览模式（无 query / messages）
        let result = engine
            .recall(RecallRequest::default())
            .await
            .expect("空库召回应成功");
        assert!(result.items.is_empty());
        assert_eq!(result.stats.mode, crate::types::RecallMode::Overview);

        // 写入：空 messages 是边界错误（显式 Validation，不静默成功）
        let err = engine
            .ingest(IngestRequest {
                messages: Vec::new(),
                persona: None,
                conversation_id: None,
                channel: crate::types::CHANNEL_MCP.to_string(),
                finalize: false,
            })
            .await
            .expect_err("空 messages 应报错");
        assert_eq!(err.category(), "validation");

        // 封存：不存在的会话 → 未抢到（幂等语义，不报错）
        let outcome = engine.seal(Uuid::nil()).await.expect("封存应成功返回");
        assert!(!outcome.sealed);
        assert_eq!(outcome.l1_count, 0);

        // 空闲检查：无活跃会话 → 0
        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);

        // 历史：无 session_id / persona → 空结构
        let history = engine
            .history(HistoryRequest::default())
            .await
            .expect("历史读取应成功");
        assert!(history.session_id.is_none());
        assert!(history.messages.is_empty());

        // 人格列表：空库 → 空列表
        assert!(
            engine
                .persona_list()
                .await
                .expect("人格列表应成功")
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重建冷却窗口：间隔为 0（默认）恒允许；配置间隔后按"最近构建完成时间"判定。
    #[tokio::test]
    async fn index_rebuild_cooldown_follows_config_interval() {
        let dir = temp_dir("cooldown");
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("初始化测试库应成功");
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));
        let keychain = Arc::new(Keychain::new());
        let llm = build_llm_provider(&BackendConfig::lm_studio_default(), &keychain, None)
            .expect("构建本地 provider 应成功");

        // 间隔 0（默认）: 不节流 —— 跨进程写入即时可见
        let engine = Engine::from_parts(
            Arc::clone(&storage),
            Arc::clone(&llm),
            None,
            TestConfig::default(),
        );
        assert_eq!(engine.config().index.refresh_interval_seconds, 0);
        engine.record_index_build_time(now_ms());
        assert!(
            engine.index_rebuild_cooldown_elapsed(),
            "间隔为 0 时应恒允许重建"
        );

        // 间隔 60 秒: 从未构建 / 窗口内 / 超过窗口 三种判定
        let mut config = TestConfig::default();
        config.index.refresh_interval_seconds = 60;
        let engine = Engine::from_parts(storage, llm, None, config);
        assert!(
            engine.index_rebuild_cooldown_elapsed(),
            "从未构建过索引 → 允许（首次加载不受节流约束）"
        );
        engine.record_index_build_time(now_ms());
        assert!(
            !engine.index_rebuild_cooldown_elapsed(),
            "刚构建完成 → 冷却窗口内不允许重建"
        );
        engine.record_index_build_time(now_ms() - 61_000);
        assert!(
            engine.index_rebuild_cooldown_elapsed(),
            "距上次构建超过间隔 → 允许重建"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `from_parts` 注入构造：不触碰数据库文件与配置文件。
    #[tokio::test]
    async fn from_parts_constructs_without_io() {
        let dir = temp_dir("parts");
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("初始化测试库应成功");
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));

        // mock LLM：仅实现 trait 必需方法的最小子集成本较高，此处用真实本地 provider
        // （不发起网络调用，仅构造）验证注入路径；embedding 显式注入 None 走降级。
        let keychain = Arc::new(Keychain::new());
        let llm = build_llm_provider(&BackendConfig::lm_studio_default(), &keychain, None)
            .expect("构建本地 provider 应成功");
        let engine = Engine::from_parts(storage, llm, None, TestConfig::default());

        assert!(!engine.is_embedding_available());
        assert!(!engine.is_retriever_loaded());
        assert!(
            engine.db_path().as_os_str().is_empty(),
            "from_parts 不携带库路径"
        );
        engine
            .storage()
            .list_personas()
            .await
            .expect("注入的存储应可查询");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
