//! crates/ramaria-service/src/engine.rs - 服务层引擎装配与用例挂载点
//!
//! 设计特点:
//! - `Engine` 自持依赖装配：storage（连接池 + migration）→ 配置（只读）→ LLM → 嵌入（可选）→ 检索占位
//! - 与传输无关：不依赖 app / cli / desktop / tauri，不持有界面或协议概念
//! - 配置纪律：config.toml 为配置权威源（桌面 / CLI 双写同步以文件为准），本层只读不写回
//! - 降级链：嵌入模型缺失 → 向量通道不可用（BM25 + 关键词镜像继续工作），不阻塞装配
//! - 懒加载：检索索引在首次召回时构建，本层仅持有占位槽（避免进程启动即加载大库）；
//!   占位槽未加载期间产生的 L1 增量会置脏标记，保证下次加载重建不漏（见 `index_dirty`）
//! - 用例挂载点：recall / ingest / seal / tick_idle / history / persona 均由用例实现接入

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::config::{EmbeddingDevice, RamariaConfig};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::{BackendConfig, LlmProvider as LlmProviderKind};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::keyword::KeywordService;
use ramaria_memory::retriever::Retriever;
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

use crate::index::IndexStamp;
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;
use crate::types::{
    ChatSendOutcome, ChatSendRequest, HistoryRequest, HistoryResult, IngestOutcome, IngestRequest,
    PersonaCardRequest, PersonaCardView, PersonaSummaryView, RecallRequest, RecallResult,
    SealOutcome,
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
pub struct Engine {
    /// 存储后端（业务 CRUD + 基础设施）。
    storage: Arc<dyn StorageBackend>,
    /// 当前 LLM provider（按 DB 侧 backend_config 装配）。
    llm: Arc<dyn LlmProvider>,
    /// 嵌入模型 provider（None = 向量通道降级，BM25 + 关键词镜像继续可用）。
    embedding: Option<Arc<dyn EmbeddingProvider>>,
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
    /// 行为层待定池（跨会话内存态：行为增量编排的归簇状态，与 app 侧同一机制）。
    behavior_pending: Arc<Mutex<PendingPool>>,
    /// 召回隐私与边界策略（默认保守；入口层按 `[mcp]` 配置注入）。
    recall_policy: Arc<RwLock<RecallPolicy>>,
    /// 封存钩子（行为 / 风格 / L2 触发；未注册则跳过，见 [`SealHooks`]）。
    seal_hooks: Arc<RwLock<SealHooks>>,
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
    /// 4. LLM provider（按 `[cache]` 配置注入精确缓存）；
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
        let llm = build_llm_provider(&backend_config, &keychain, cache)?;

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
            llm,
            embedding,
            // 行为层待定池按生效配置初始化（与 app 侧同一机制）
            behavior_pending: Arc::new(Mutex::new(PendingPool::new(&config.behavior))),
            config,
            db_path,
            // ---- 6. 检索器占位：首次召回时构建（懒加载）----
            retriever: Arc::new(RwLock::new(None)),
            // ---- 7. 关键词镜像与策略 / 钩子：空镜像 + 默认保守策略 ----
            keyword_mirror: Arc::new(RwLock::new(KeywordService::new())),
            index_dirty: Arc::new(AtomicBool::new(false)),
            index_stamp: Arc::new(RwLock::new(None)),
            recall_policy: Arc::new(RwLock::new(RecallPolicy::default())),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
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
    pub fn from_parts(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        config: RamariaConfig,
    ) -> Self {
        Self {
            storage,
            llm,
            embedding,
            behavior_pending: Arc::new(Mutex::new(PendingPool::new(&config.behavior))),
            config,
            db_path: PathBuf::new(),
            retriever: Arc::new(RwLock::new(None)),
            keyword_mirror: Arc::new(RwLock::new(KeywordService::new())),
            index_dirty: Arc::new(AtomicBool::new(false)),
            index_stamp: Arc::new(RwLock::new(None)),
            recall_policy: Arc::new(RwLock::new(RecallPolicy::default())),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
        }
    }

    // =========================================================
    // 依赖访问器
    // =========================================================

    /// 存储后端引用（用例实现与诊断使用）。
    pub fn storage(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// 当前 LLM provider（按值返回 Arc，便于移入异步任务）。
    pub fn llm(&self) -> Arc<dyn LlmProvider> {
        Arc::clone(&self.llm)
    }

    /// 嵌入 provider（None = 向量通道降级）。
    pub fn embedding(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedding.clone()
    }

    /// 向量通道是否可用（嵌入模型已加载）。
    pub fn is_embedding_available(&self) -> bool {
        self.embedding.is_some()
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
    // 内部依赖访问器（crate 内用例实现使用）
    // =========================================================

    /// 存储后端（crate 内用例实现使用）。
    pub(crate) fn storage_ref(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// LLM provider（crate 内用例实现使用）。
    pub(crate) fn llm_ref(&self) -> &Arc<dyn LlmProvider> {
        &self.llm
    }

    /// 嵌入 provider（crate 内用例实现使用）。
    pub(crate) fn embedding_ref(&self) -> Option<&Arc<dyn EmbeddingProvider>> {
        self.embedding.as_ref()
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
fn build_llm_provider(
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
