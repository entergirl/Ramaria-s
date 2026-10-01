//! crates/ramaria-service/src/engine.rs - 服务层引擎装配与用例挂载点
//!
//! 设计特点:
//! - `Engine` 自持依赖装配：storage（连接池 + migration）→ 配置（快照）→ LLM → 嵌入（可选）→ 检索占位
//! - 与传输无关：不依赖 app / cli / desktop / tauri，不持有界面或协议概念
//! - 配置纪律：config.toml 为配置权威源（双写以文件为准）；装配路径只读，
//!   写入只经配置用例（双写 / 热重载，见 [`Engine::save_config`]）
//! - 热更新：LLM / 嵌入 provider 与配置以 `RwLock` 持有快照，变更经用例整体替换；
//!   读取路径取克隆后在锁外使用，异步代码不跨 `.await` 持锁
//! - 降级链：嵌入模型缺失 → 向量通道不可用（BM25 + 关键词镜像继续工作），不阻塞装配
//! - 懒加载：检索索引在首次召回时构建，本层仅持有占位槽（避免进程启动即加载大库）；
//!   占位槽未加载期间产生的 L1 增量会置脏标记，保证下次加载重建不漏（见 `index_dirty`）
//! - 显式重建：`rebuild_index` 强制全量重建（跳过早退与冷却窗口），构建失败保留旧索引
//!   并置"重建失败"告警位与失败原因记录（`is_index_rebuild_failed` / `index_build_failure`，
//!   供诊断展示与宿主告警）
//! - 重建节流：跨进程代次变化触发的重建受 `[index].refresh_interval_seconds` 约束
//!   （0 = 不节流，见 `index_rebuild_cooldown_elapsed`）
//! - 宿主后台任务：进程内空闲检查循环由入口层拉起（`spawn_idle_loop`），退出时优雅关停
//! - 用例挂载点：recall / ingest / seal / tick_idle / history / persona / 模型管理均由用例实现接入
//! - 底层连接池：装配时保留 `SqlitePool` 句柄，供导入等以 `&SqlitePool` 为入口的用例使用
//!   （注入构造可经 `attach_sqlite_pool` 附着）

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::behavior::BehaviorRule;
use ramaria_core::config::{EmbeddingDevice, RamariaConfig};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{lock_recover, read_recover, write_recover};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::{
    AppState, BackendConfig, LlmProvider as LlmProviderKind, MemoryL1, Session, now_ms,
};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::keyword::KeywordService;
use ramaria_memory::retriever::Retriever;
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::behavior::{BehaviorLearnOutcome, RuleEvidenceItem};
use crate::config::{ConfigWriter, SyncOutcome, SyncWriteResult};
use crate::diagnostics::{DiagnosticsReport, DiagnosticsRequest};
use crate::export::{ExportData, ExportDataRequest};
use crate::idle::{IdleLoop, IdleLoopOptions};
use crate::index::{IndexBuildFailure, IndexStamp};
use crate::lifecycle::{Lifecycle, LifecycleOptions};
use crate::persona::{PersonaLoadMode, PersonaRegenerateOutcome};
use crate::privacy::PrivacyStatus;
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;
use crate::stream_event::ChatStreamHandle;
use crate::style::StyleStatsView;
use crate::types::{
    AliasResolveOutcome, AliasResolveRequest, ChannelOverviewView, ChatSendOutcome,
    ChatSendRequest, ChatStreamRequest, DegradedReason, EmbeddingModelView, EmbeddingValidation,
    FactBrowsePage, FactBrowseRequest, FactDetailView, GroupedFactsView, HistoryRequest,
    HistoryResult, IngestOutcome, IngestRequest, KeywordPoolView, KeywordSeedOutcome,
    KeywordSuggestionOutcome, L1BrowsePage, L1BrowseRequest, L1MemoryView, L2BrowsePage,
    L2BrowseRequest, L3TraitView, PendingAliasView, PersonaCardRequest, PersonaCardView,
    PersonaFileOutcome, PersonaFullView, PersonaSummaryView, PersonaUpdateRequest,
    PersonalityProfileView, ProfileStatusView, RecallRequest, RecallResult, SealOutcome,
    SessionBrowsePage, SessionBrowseRequest, SessionDetailView, SessionMessagesRequest,
    SessionMessagesView, SetupRequest, SetupStatus, TraitEvidenceRequest, TraitEvidenceView,
};
use crate::utt::UttRebuildOutcome;

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
/// - 生效配置同为 `RwLock` 快照：配置用例（双写 / 热重载）整体替换，读取方取克隆后在锁外使用。
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
    /// 生效配置快照（config.toml 为权威源；经配置用例（双写 / 热重载）整体替换 Arc）。
    ///
    /// 快照语义与 LLM / 嵌入 provider 一致：读取方在锁内克隆内层 Arc 后释放锁，
    /// 异步路径不持有锁跨 `.await`；写入方整体替换内层 Arc。
    config: RwLock<Arc<RamariaConfig>>,
    /// 实际使用的配置文件路径（`from_parts` 构造时为空路径）。
    config_path: PathBuf,
    /// 数据库文件路径（诊断与客户端配置片段展示用）。
    db_path: PathBuf,
    /// 底层 SQLite 连接池句柄（导入等以 `&SqlitePool` 为入口的用例使用；未附着时为 None）。
    ///
    /// 语义:
    /// - `open_with` 装配时自动附着；注入构造（`from_parts`）默认未附着，
    ///   由宿主 / 测试经 [`Engine::attach_sqlite_pool`] 附着；
    /// - 句柄为连接池的克隆（内部共享），读取方取克隆后在锁外使用。
    pool: RwLock<Option<SqlitePool>>,
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
    /// 检索索引"重建失败"告警位：最近一次构建尝试失败且旧索引未刷新。
    ///
    /// 语义:
    /// - `true` = 最近一次构建失败，共享检索器保留的是旧索引（仍可检索，但未刷新）；
    /// - 构建成功后复位；供诊断展示与宿主告警（记忆注入可能不完整）。
    index_rebuild_failed: Arc<AtomicBool>,
    /// 检索索引最近一次构建失败的原因记录（脱敏原因文本 + 记录时间）。
    ///
    /// 语义:
    /// - `Some(..)` = 最近一次构建失败；构建成功后清除；
    /// - 供诊断导出与宿主提示携带可诊断原因；读取方取克隆后在锁外使用
    ///   （异步路径不持锁跨 `.await`）。
    index_build_failure: Arc<RwLock<Option<IndexBuildFailure>>>,
    /// 行为层待定池（跨会话内存态：行为增量编排的归簇状态）。
    behavior_pending: Arc<Mutex<PendingPool>>,
    /// 召回隐私与边界策略（装配时按配置闸门映射缺省；入口层可按需注入覆盖）。
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

    // =========================================================
    // 策略与钩子（入口层注入）
    // =========================================================

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

    /// 流式生成用例：以指定人格回复一条消息，返回增量事件流句柄（交互入口消费）。
    ///
    /// 职责:
    /// - 与非流式生成同源：参数校验 / 状态与隐私门禁 / 会话定位 / 历史窗口 / 记忆召回 /
    ///   Prompt 装配 / Token 预算为同一份实现；
    /// - 生成侧差异：用户消息先落库，增量按事件流转发，助手回复仅在无错且非空时落库；
    ///   流打不开时不落库并返回只含一个 Error 事件的流。
    ///
    /// 返回:
    /// - 成功时返回 `ChatStreamHandle`（会话定位 + 事件流）；前置编排失败返回对应错误。
    pub async fn chat_stream(
        self: &Arc<Self>,
        req: ChatStreamRequest,
    ) -> RamariaResult<ChatStreamHandle> {
        crate::chat::stream(self, req).await
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

    /// 会话创建用例：新建空白会话（可绑定人格）。
    ///
    /// 返回:
    /// - 新会话核心记录；宿主自行映射为各自既有响应结构。
    pub async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        crate::session::create(self, persona_uid).await
    }

    /// 会话删除用例：仅删除会话行本身（关联数据由外键级联规则清理）。
    ///
    /// 说明:
    /// - 会话不存在时幂等成功（与存储层删除语义一致）。
    pub async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        crate::session::delete(self, session_id).await
    }

    /// 会话级联删除用例：事务内按依赖顺序清理全部关联数据后删除会话行。
    ///
    /// 说明:
    /// - 供一次性合成会话（如探针）用完即删的场景使用：不触发封存 / 学习管线；
    /// - 宿主若持有生命周期容器，需在删除后自行清理活跃指针与活跃时间缓存。
    pub async fn delete_session_cascade(&self, session_id: Uuid) -> RamariaResult<()> {
        crate::session::delete_cascade(self, session_id).await
    }

    /// 解析发送目标会话（会话预检与自动重建）。
    ///
    /// 语义:
    /// - 指定会话存在且未关闭 → 原样返回；
    /// - 指定会话不存在或已关闭 → 新建会话（绑定人格）并返回其 id；
    /// - 存储查询失败 → 保守返回原会话（由生成路径做最终校验）；
    /// - 未指定会话（`None`）→ 新建会话（绑定人格）并返回。
    ///
    /// 用途:
    /// - 交互入口在进入生成前调用，避免前端竞态窗口把已关闭 / 已删除的会话 id
    ///   传入后收到"会话已关闭"错误。
    pub async fn resolve_send_session(
        &self,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
    ) -> RamariaResult<Uuid> {
        crate::session::resolve_send_session(self, persona_uid, session_id).await
    }

    // =========================================================
    // 记忆与会话浏览用例
    // =========================================================

    /// L1 记忆浏览用例：按会话收集摘要（桌面口径）或按 persona 取未吸收摘要（CLI 口径）。
    ///
    /// 返回:
    /// - `items`（分页后的摘要视图）与 `total`（排序后、分页前的条数）。
    pub async fn memory_l1(&self, req: L1BrowseRequest) -> RamariaResult<L1BrowsePage> {
        crate::browse::l1(self, req).await
    }

    /// L1 摘要按会话读取用例（封存结果的核对口径）。
    ///
    /// 返回:
    /// - 目标会话的全部摘要视图；会话不存在或无摘要均返回空列表（不报错）。
    pub async fn memory_l1_by_session(&self, session_id: Uuid) -> RamariaResult<Vec<L1MemoryView>> {
        crate::browse::l1_by_session(self, session_id).await
    }

    /// L2 事件浏览用例：persona 过滤分页（分页前总数）或全人格合并后统一排序截断。
    ///
    /// 返回:
    /// - `items` 与 `total`（分页前的条数）。
    pub async fn memory_l2(&self, req: L2BrowseRequest) -> RamariaResult<L2BrowsePage> {
        crate::browse::l2(self, req).await
    }

    /// L3 性格标签浏览用例（扁平列表；persona 缺省时合并全部人格）。
    pub async fn memory_l3(&self, persona: Option<&str>) -> RamariaResult<Vec<L3TraitView>> {
        crate::browse::l3(self, persona).await
    }

    /// L3 三层性格画像用例（base / primary / accent 分组，仅生效标签；人格不存在报错）。
    pub async fn personality_profile(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<PersonalityProfileView> {
        crate::browse::personality_profile(self, persona_uid).await
    }

    /// 画像数据状态用例（有效样本量与可信度区间：insufficient / preliminary / trusted）。
    pub async fn profile_status(&self, persona_uid: &str) -> RamariaResult<ProfileStatusView> {
        crate::browse::profile_status(self, persona_uid).await
    }

    /// 性格标签证据链用例：trait → 证据记录 → 事件 → L1 溯源 → 证据片段。
    ///
    /// 返回:
    /// - 单元素列表（证据链视图）；无证据记录时返回单条空链（非错误）。
    pub async fn memory_trait_evidence(
        &self,
        req: TraitEvidenceRequest,
    ) -> RamariaResult<Vec<TraitEvidenceView>> {
        crate::browse::trait_evidence(self, req).await
    }

    /// 知识事实浏览用例：活跃事实，可选按字段过滤后分页。
    ///
    /// 返回:
    /// - `items`（全字段视图）与 `total`（分页前的条数）。
    pub async fn memory_facts(&self, req: FactBrowseRequest) -> RamariaResult<FactBrowsePage> {
        crate::browse::facts(self, req).await
    }

    /// 单条事实详情用例：含完整版本链（链头最早在前）；不存在时返回 `None`。
    pub async fn memory_fact_detail(&self, id: i64) -> RamariaResult<Option<FactDetailView>> {
        crate::browse::fact_detail(self, id).await
    }

    /// 知识事实分组用例：按字段分组 + 多版本事实的版本链折叠数据。
    pub async fn memory_facts_grouped(&self, persona: &str) -> RamariaResult<GroupedFactsView> {
        crate::browse::facts_grouped(self, persona).await
    }

    /// 会话列表浏览用例：开始时间倒序 + 消息计数聚合 + 分页（`limit` 缺省返回全部）。
    pub async fn session_list(
        &self,
        req: SessionBrowseRequest,
    ) -> RamariaResult<SessionBrowsePage> {
        crate::browse::sessions(self, req).await
    }

    /// 会话消息浏览用例：全量正序（`limit` 为 None）或最新在前分页后翻正。
    pub async fn session_messages(
        &self,
        req: SessionMessagesRequest,
    ) -> RamariaResult<SessionMessagesView> {
        crate::browse::session_messages(self, req).await
    }

    /// 会话详情用例：会话元数据 + 消息页（全量或分页后翻正）。
    ///
    /// 返回:
    /// - 会话不存在时返回 `Validation` 错误。
    pub async fn session_detail(
        &self,
        session_id: Uuid,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> RamariaResult<SessionDetailView> {
        crate::browse::session_detail(self, session_id, limit, offset).await
    }

    /// 会话消息计数用例（诊断用；查询失败按 0 处理，不阻塞主流程）。
    pub async fn count_session_messages(&self, session_id: Uuid) -> usize {
        crate::browse::count_session_messages(self, session_id).await
    }

    /// 通道会话概览用例：该通道的活跃会话数与最近活动时间（只读聚合）。
    pub async fn channel_overview(&self, channel: &str) -> RamariaResult<ChannelOverviewView> {
        crate::browse::channel_overview(self, channel).await
    }

    // =========================================================
    // 导出与 utt 重建用例
    // =========================================================

    /// 会话导出数据装配用例（会话集合 + 消息 + 人格 L1 摘要段）。
    ///
    /// 说明:
    /// - 只做数据装配；JSON / Markdown 文本生成与文件写出属入口能力；
    /// - `total_sessions` 为过滤前全部会话数，`sessions` 为过滤并分页后的装配结果。
    pub async fn export_sessions(&self, req: ExportDataRequest) -> RamariaResult<ExportData> {
        crate::export::collect(self, req).await
    }

    /// utt 话语块重建用例（可选 `--force` 全量重切，完成后刷新检索索引）。
    ///
    /// 说明:
    /// - 以当前生效配置的 `[utt]` 组为切分参数；配置未启用时 `rebuilt = false`；
    /// - `force = true` 先清空全部旧块再全量重建（切分参数变更后必须使用）。
    pub async fn rebuild_utt_blocks(&self, force: bool) -> RamariaResult<UttRebuildOutcome> {
        crate::utt::rebuild(self, force).await
    }

    /// 关键词池列表用例（三态计数 + 全量词条）。
    pub async fn keyword_list(&self) -> RamariaResult<KeywordPoolView> {
        crate::keyword::list(self).await
    }

    /// 待确认别名列表用例（pending，别名 → 建议规范词）。
    pub async fn keyword_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasView>> {
        crate::keyword::pending_aliases(self).await
    }

    /// 别名裁决用例：确认合并（pending → alias）/ 驳回晋升（pending → canonical）。
    ///
    /// 说明:
    /// - confirm 且已是 alias 时按 `already_applied_ok` 选择幂等成功或报错
    ///   （调用入口各自口径）。
    pub async fn keyword_resolve_alias(
        &self,
        req: AliasResolveRequest,
    ) -> RamariaResult<AliasResolveOutcome> {
        crate::keyword::resolve_alias(self, req).await
    }

    /// 关键词 seed 用例：幂等手工注入规范词（已存在保持现状）。
    ///
    /// 说明:
    /// - 整体校验（任一非法即报错、不部分写入）后去重，保留首次出现顺序；
    ///   新词条 use_count 从 0 起，已存在词条不递增 use_count、不改别名状态。
    pub async fn keyword_seed(&self, keywords: &[String]) -> RamariaResult<KeywordSeedOutcome> {
        crate::keyword::seed(self, keywords).await
    }

    /// 关键词别名建议用例：扫描词池与内存镜像使用量，把相似词对登记为待确认别名。
    ///
    /// 说明:
    /// - `min_use` 为 None 时取服务层默认阈值（过滤仅出现 1-2 次的偶然用词）；
    /// - 单次运行最多登记固定条数，超出部分本轮不写（结果中携带截断计数）；
    /// - 调用入口按 best-effort 处理错误（建议生成不阻塞列表 / 主流程）。
    pub async fn keyword_suggest_pending_aliases(
        &self,
        min_use: Option<u32>,
    ) -> RamariaResult<KeywordSuggestionOutcome> {
        crate::keyword::suggest_pending_aliases(self, min_use).await
    }

    /// 人格列表用例：列出全部人格摘要（uid / 名称 / 类型 / 来源 / 启用状态）。
    pub async fn persona_list(&self) -> RamariaResult<Vec<PersonaSummaryView>> {
        crate::persona::list(self).await
    }

    /// 人格卡片用例：性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度。
    pub async fn persona_card(&self, req: PersonaCardRequest) -> RamariaResult<PersonaCardView> {
        crate::persona::card(self, req).await
    }

    /// 人格全字段列表用例（含 ref_id / avatar / config / description / 更新时间）。
    pub async fn persona_list_full(&self) -> RamariaResult<Vec<PersonaFullView>> {
        crate::persona::list_full(self).await
    }

    /// 人格信息更新用例（名称 / 头像 / 描述；配置内容由文件导入通道管理）。
    ///
    /// 返回:
    /// - 更新后回读的完整视图；uid 为空 / 人格不存在返回 `Validation` 错误。
    pub async fn persona_update_info(
        &self,
        uid: &str,
        req: PersonaUpdateRequest,
    ) -> RamariaResult<PersonaFullView> {
        crate::persona::update_info(self, uid, req).await
    }

    /// 人格文件导入用例：从目录扫描 `.toml` 文件（文件名 = uid）创建或同步记录。
    ///
    /// 参数:
    /// - `dir`: 人格文件目录（目录解析由调用方负责；目录不可读返回 `Io` 错误）。
    /// - `uid_filter`: 只处理指定 uid（文件名 stem 精确匹配）；`None` 表示全部。
    /// - `mode`: 记录已存在时的处置模式（创建或更新 / 仅创建缺失跳过）。
    ///
    /// 返回:
    /// - 每个文件的处理结果（新建 / 更新 / 跳过 / 失败），单文件失败不中断其余文件。
    pub async fn persona_load_from_dir(
        &self,
        dir: &Path,
        uid_filter: Option<&str>,
        mode: PersonaLoadMode,
    ) -> RamariaResult<Vec<PersonaFileOutcome>> {
        crate::persona::load_from_dir(self, dir, uid_filter, mode).await
    }

    /// 人格文件导入用例（单个文件；旧单文件布局兼容路径）。
    ///
    /// 参数:
    /// - `path`: 人格文件路径（旧布局的文件名不携带 uid，uid 由调用方显式给出）。
    /// - `uid`: 目标人格 uid（旧单文件布局使用固定的 `rama-0001`）。
    /// - `fallback_name`: 文件缺少 `assistant_name` 时的名称兜底。
    /// - `mode`: 记录已存在时的处置模式（创建或更新 / 仅创建缺失跳过）。
    ///
    /// 返回:
    /// - 本文件的处理结果（新建 / 更新 / 跳过 / 失败）；失败转为结果条目（不上抛）。
    pub async fn persona_load_file(
        &self,
        path: &Path,
        uid: &str,
        fallback_name: &str,
        mode: PersonaLoadMode,
    ) -> PersonaFileOutcome {
        crate::persona::load_file(self, path, uid, fallback_name, mode).await
    }

    /// 确保系统用户人格（user-0001）存在（幂等）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次创建；`Ok(false)`: 已存在（未做任何写入）。
    pub async fn persona_ensure_user(&self) -> RamariaResult<bool> {
        crate::persona::ensure_user(self).await
    }

    /// 重生成某人格在导入会话中的 L1 摘要（不含 L2/L3 级联，宿主按需触发）。
    ///
    /// 返回:
    /// - 逐会话重生成计数与提示文案；级联（L2/L3）由宿主在拿到结果后自行触发。
    pub async fn regenerate_persona_l1(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<PersonaRegenerateOutcome> {
        crate::persona::regenerate_import_l1(self, persona_uid).await
    }

    // =========================================================
    // 行为规则与表达风格用例
    // =========================================================

    /// 行为规则列表用例：按 persona 列出全部规则（含禁用项）。
    ///
    /// 返回:
    /// - 全量规则列表（存储层稳定排序）。
    pub async fn behavior_list_rules(&self, persona_uid: &str) -> RamariaResult<Vec<BehaviorRule>> {
        crate::behavior::list_rules(self, persona_uid).await
    }

    /// 行为规则详情用例：按 id 查询单条规则。
    ///
    /// 返回:
    /// - `Ok(Some(rule))`: 规则存在；`Ok(None)`: 规则不存在（空态，非错误）。
    pub async fn behavior_get_rule(&self, id: i64) -> RamariaResult<Option<BehaviorRule>> {
        crate::behavior::get_rule(self, id).await
    }

    /// 行为规则启停用例：禁用写 S1 反馈日志（启用不写，非干预信号）。
    ///
    /// 参数:
    /// - `id`: 规则 id。
    /// - `enabled`: true = 启用，false = 禁用。
    /// - `session_id`: 干预发生的会话（可选，审计关联）。
    pub async fn behavior_set_rule_enabled(
        &self,
        id: i64,
        enabled: bool,
        session_id: Option<&str>,
    ) -> RamariaResult<()> {
        crate::behavior::set_rule_enabled(self, id, enabled, session_id).await
    }

    /// 行为规则编辑用例：全量覆盖 + 转 Manual 强锚点 + 写编辑前后快照反馈。
    ///
    /// 参数:
    /// - `rule`: 编辑后的完整规则（id 定位）。
    /// - `session_id`: 干预发生的会话（可选，审计关联）。
    pub async fn behavior_edit_rule(
        &self,
        rule: &mut BehaviorRule,
        session_id: Option<&str>,
    ) -> RamariaResult<()> {
        crate::behavior::edit_rule(self, rule, session_id).await
    }

    /// 行为规则删除用例（破坏性操作，调用方负责确认）。
    pub async fn behavior_delete_rule(&self, id: i64) -> RamariaResult<()> {
        crate::behavior::delete_rule(self, id).await
    }

    /// 行为规则导入用例：宽松 JSON 校验（非法拒绝），导入规则 source=Manual。
    ///
    /// 参数:
    /// - `persona_uid`: 规则所属人格。
    /// - `json`: 规则 JSON（含 situation / reaction / params / avoid 字段）。
    ///
    /// 返回:
    /// - 新规则 id（Manual，自动生效）。
    pub async fn behavior_import_rule(&self, persona_uid: &str, json: &str) -> RamariaResult<i64> {
        crate::behavior::import_rule(self, persona_uid, json).await
    }

    /// 行为规则证据链用例：规则 → 事件 → 脱敏视图（权重降序，脏引用跳过）。
    ///
    /// 返回:
    /// - 证据项列表；规则不存在时返回业务校验错误。
    pub async fn behavior_rule_evidence(&self, id: i64) -> RamariaResult<Vec<RuleEvidenceItem>> {
        crate::behavior::rule_evidence(self, id).await
    }

    /// 行为规则全量学习用例：事件 → 聚类（含 Manual 锚点）→ 规则生成 → 替换旧 Auto。
    ///
    /// 返回:
    /// - 学习统计；`[behavior].enabled=false` 时返回空统计。
    pub async fn behavior_learn(&self, persona_uid: &str) -> RamariaResult<BehaviorLearnOutcome> {
        crate::behavior::learn(self, persona_uid).await
    }

    /// 行为规则增量更新用例（封存钩子核心，供宿主手动触发）。
    ///
    /// 说明:
    /// - 处理 persona 未吸收事件：归簇 / 待定池推进 / 证据衰减 / 漂移检测并落库；
    /// - `[behavior].enabled=false` 时直接返回。
    pub async fn behavior_incremental_update(&self, persona_uid: &str) -> RamariaResult<()> {
        crate::behavior::incremental_update(self, persona_uid).await
    }

    /// 风格统计增量更新用例（封存钩子核心，供宿主手动补跑）。
    ///
    /// 说明:
    /// - 全量消息 → 五维统计 → 基线显著性 → 规则文本生成 / 替换落库（幂等）；
    /// - LLM 不可用 / 失败由核心静默降级为模板生成；开关由调用方判断。
    pub async fn style_incremental_update(&self, persona_uid: &str) -> RamariaResult<()> {
        crate::style::incremental_update(self, persona_uid).await
    }

    /// 自动风格规则读取用例（注入侧）：仅 Ready 状态返回非空规则文本。
    ///
    /// 返回:
    /// - `Ok(Some(rule))`: 可注入的规则文本；
    /// - `Ok(None)`: 数据不足 / 无显著项 / 未统计（静默跳过）。
    pub async fn style_load_rule(&self, persona_uid: &str) -> RamariaResult<Option<String>> {
        crate::style::load_style_rule(self.storage_ref().as_ref(), persona_uid).await
    }

    /// 说话风格统计读取用例：单行统计视图。
    ///
    /// 返回:
    /// - `Ok(Some(view))`: 样本量 / 状态与标签 / 规则来源与标签 / 规则文本 / 统计 JSON / 更新时间；
    /// - `Ok(None)`: 该人格未统计过（空态，非错误）。
    pub async fn style_stats(&self, persona_uid: &str) -> RamariaResult<Option<StyleStatsView>> {
        crate::style::stats(self, persona_uid).await
    }

    /// 确保检索索引已加载（懒加载：首次召回前构建一次，重复调用为空操作）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次调用完成了构建。
    /// - `Ok(false)`: 索引此前已加载（或无需构建）。
    ///
    /// 说明:
    /// - 构建失败时置"重建失败"告警位并上抛错误（旧索引保持可用，
    ///   见 [`Engine::is_index_rebuild_failed`]）；
    /// - 构建完成后写回索引版本（供首次配置状态机判定"索引已构建"）。
    pub async fn ensure_index_loaded(&self) -> RamariaResult<bool> {
        crate::index::ensure_loaded(self).await
    }

    /// 强制全量重建内存检索索引（跳过懒加载早退与冷却窗口）。
    ///
    /// 职责:
    /// - 供宿主显式刷新（批量导入完成 / 设置变更 / 诊断修复等场景）调用；
    /// - 与懒加载路径共用同一构建实现，重建后立即生效
    ///   （不受 `[index].refresh_interval_seconds` 约束）。
    ///
    /// 返回:
    /// - `Ok(total)`: 重建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
    /// - `Err(..)`: 构建失败——旧索引保持可用、告警位置位并上抛错误。
    pub async fn rebuild_index(&self) -> RamariaResult<usize> {
        crate::index::rebuild(self).await
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
        let options = IdleLoopOptions::from_config(self.config().as_ref());
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
    // 设置与元信息用例
    // =========================================================

    /// 设置列表用例：读取全部设置项（`settings` 表键值对）。
    ///
    /// 返回:
    /// - 空库返回空列表（非错误）；返回键集合与过滤口径保持现状。
    pub async fn settings_list(&self) -> RamariaResult<Vec<(String, String)>> {
        crate::settings::list(self).await
    }

    /// 设置读取用例：读取单个设置项（缺失键返回 None，不报错）。
    pub async fn setting_get(&self, key: &str) -> RamariaResult<Option<String>> {
        crate::settings::get(self, key).await
    }

    /// 设置写入用例：写入单个设置项（已存在键覆盖写）。
    ///
    /// 返回:
    /// - 空键返回 `Validation` 错误（文案与桌面现状一致）。
    pub async fn setting_set(&self, key: &str, value: &str) -> RamariaResult<()> {
        crate::settings::set(self, key, value).await
    }

    /// 读取 DB 侧后端配置（`backend_config` 表）。
    ///
    /// 返回:
    /// - `Ok(None)`: 无记录（回退口径由调用方按各自现状决定）。
    pub async fn backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        crate::settings::backend_config(self).await
    }

    /// 读取数据库 schema 版本（`schema_meta` 表；键缺失按 1，非法值报错）。
    pub async fn schema_version(&self) -> RamariaResult<i32> {
        crate::settings::schema_version(self).await
    }

    // =========================================================
    // 隐私确认用例
    // =========================================================

    /// 检查当前后端的隐私确认状态。
    ///
    /// 说明:
    /// - 判定输入（provider / base_url）取 DB 侧后端配置，与桌面 / CLI 现状同源；
    /// - 无后端配置记录时按本地 provider 默认值判定（无需确认）。
    pub async fn check_privacy(&self) -> RamariaResult<PrivacyStatus> {
        crate::privacy::check(self).await
    }

    /// 记录当前后端的隐私确认。
    ///
    /// 参数:
    /// - `persistent`: 是否跨重启持久化（勾选"下次不再提醒"）。
    pub async fn confirm_privacy(&self, persistent: bool) -> RamariaResult<()> {
        crate::privacy::confirm(self, persistent).await
    }

    // =========================================================
    // 配置用例（双写同步与热重载）
    // =========================================================

    /// 只读加载完整配置（config.toml 与 DB 侧合并，无写副作用）。
    ///
    /// 说明:
    /// - 等价 `ConfigWriter::load_config_only`：文件缺失 / 解析失败时以 DB 侧为准；
    /// - 不更新内存快照、不写任何一侧（设置页回显等只读场景）。
    pub async fn load_full_config(&self) -> RamariaResult<RamariaConfig> {
        let writer = self.config_writer()?;
        writer.load_config_only().await
    }

    /// 重新加载配置：一致性校验（文件为准）→ 回写 DB → 热重载内存快照。
    ///
    /// 说明:
    /// - 校验规则见 `ConfigWriter::load`（文件缺失 / 损坏路径以 DB 为准且不回写 DB）；
    /// - 成功后以合并结果整体替换内存快照（后续用例读取生效）；
    /// - 热重载范围：仅配置快照；后台循环阈值（如空闲分钟数）由宿主持有的
    ///   生命周期容器热更新，本用例不联动；行为待定池（`PendingPool`）保持既有内存态。
    pub async fn reload_config(&self) -> RamariaResult<SyncOutcome> {
        let writer = self.config_writer()?;
        let mut outcome = writer.load().await?;
        // 路径字段由装配持有（config.toml 的 paths 组只表达展示性空值）：热重载保留现快照值，
        // 避免日志目录 / 配置目录随重载丢失（诊断导出等功能依赖这些路径）
        outcome.config.paths = self.config().paths.clone();
        self.replace_config_snapshot(outcome.config.clone());
        Ok(outcome)
    }

    /// 保存完整配置：文件与 DB 双写（settings / backend_config 表），成功后热重载内存快照。
    ///
    /// 说明:
    /// - 单侧写失败降级不阻塞：结果经 `SyncWriteResult` 回传（调用方展示提示）；
    /// - 双侧全部成功时替换内存快照（后续用例读取生效），失败时保持原快照；
    /// - API key 不经本用例：密钥始终由 OS keychain 管理，配置结构本身不含密钥。
    pub async fn save_config(&self, cfg: &RamariaConfig) -> RamariaResult<SyncWriteResult> {
        let writer = self.config_writer()?;
        let result = writer.save_config(cfg).await;
        if result.is_ok() {
            // 路径字段由装配持有（保存的配置不含本机路径）：热重载时保留现快照值
            let mut next = cfg.clone();
            next.paths = self.config().paths.clone();
            self.replace_config_snapshot(next);
        }
        Ok(result)
    }

    /// 同步后端配置到文件侧 `[backend]` 组（保留文件侧其它字段与未知键）。
    ///
    /// 说明:
    /// - 仅文件侧：DB 侧由调用方（后端配置用例）先行写入，保持表 / 文件一致；
    /// - 文件损坏时拒绝覆盖（保留现场，返回失败明细）。
    pub async fn sync_backend_config(
        &self,
        backend: &BackendConfig,
    ) -> RamariaResult<SyncWriteResult> {
        let writer = self.config_writer()?;
        Ok(writer.sync_backend_config(backend).await)
    }

    /// 构造配置用例句柄（`config_path` 为空时返回显式错误）。
    fn config_writer(&self) -> RamariaResult<ConfigWriter> {
        if self.config_path.as_os_str().is_empty() {
            return Err(RamariaError::config(
                "引擎未设置 config_path，无法执行配置读写用例",
            ));
        }
        Ok(ConfigWriter::new(
            Arc::clone(&self.storage),
            self.config_path.clone(),
        ))
    }

    /// 整体替换内存配置快照（仅由配置用例调用；不重建行为待定池等既有内存态）。
    fn replace_config_snapshot(&self, cfg: RamariaConfig) {
        let mut guard = write_recover(&self.config, "engine.config");
        *guard = Arc::new(cfg);
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

    /// 更新 LLM 后端配置并热加载 provider（密钥入 keychain → 配置落库 → provider 热替换
    /// → 文件侧 `[backend]` 组同步）。
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
        crate::model::validate_embedding_model(path, self.config().embedding.device).await
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
    // 诊断导出用例
    // =========================================================

    /// 导出诊断信息为 .zip（日志 / 配置 / 系统信息；敏感内容先脱敏再打包）。
    ///
    /// 说明:
    /// - 配置快照在本方法内读取，收集与打包在锁外进行；
    /// - 收集阶段错误不阻塞导出；写入经同目录临时文件原子替换。
    pub async fn export_diagnostics(
        &self,
        req: DiagnosticsRequest,
    ) -> RamariaResult<DiagnosticsReport> {
        crate::diagnostics::export(self, req).await
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

    /// 检索索引最近一次重建是否失败（失败时共享检索器保留旧索引，仍可检索）。
    ///
    /// 用途:
    /// - 诊断展示与宿主告警；不改变任何降级行为（旧索引照常检索）。
    pub fn is_index_rebuild_failed(&self) -> bool {
        self.index_rebuild_failed.load(Ordering::Acquire)
    }

    /// 设置检索索引"重建失败"告警位（构建成功复位 / 失败置位，由索引构建路径调用）。
    pub(crate) fn set_index_rebuild_failed(&self, failed: bool) {
        self.index_rebuild_failed.store(failed, Ordering::Release);
    }

    /// 最近一次索引构建失败记录（脱敏原因 + 时间戳；未失败 / 已恢复为 `None`）。
    ///
    /// 用途:
    /// - 诊断导出携带可诊断原因；不改变任何降级行为（旧索引照常检索）。
    pub fn index_build_failure(&self) -> Option<IndexBuildFailure> {
        read_recover(&self.index_build_failure, "engine.index_build_failure").clone()
    }

    /// 记录索引构建失败原因（由索引构建路径在失败分支调用）。
    ///
    /// 参数:
    /// - `reason`: 脱敏后的原因文本（路径只留文件名、消息类字段只留字符数，不含用户原文）。
    pub(crate) fn record_index_build_failure(&self, reason: String) {
        let mut guard = write_recover(&self.index_build_failure, "engine.index_build_failure");
        *guard = Some(IndexBuildFailure {
            reason,
            at_ms: now_ms(),
        });
    }

    /// 清除索引构建失败记录（构建成功后复位）。
    pub(crate) fn clear_index_build_failure(&self) {
        let mut guard = write_recover(&self.index_build_failure, "engine.index_build_failure");
        *guard = None;
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
        let interval_seconds = self.config().index.refresh_interval_seconds;
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

    /// 探针只读访问器：装配后检索器槽未加载（None）、关键词镜像为空。
    #[tokio::test]
    async fn probe_handles_expose_empty_state_after_assembly() {
        let dir = temp_dir("probe-handles");
        let db_path = dir.join("assistant.db");
        let engine = Engine::open(db_path).await.expect("引擎装配应成功");

        let retriever = engine.retriever_slot();
        assert!(
            read_recover(&retriever, "engine.probe.retriever").is_none(),
            "装配阶段检索器槽应为空（懒加载占位）"
        );

        let mirror = engine.keyword_mirror();
        let guard = read_recover(&mirror, "engine.probe.keyword_mirror");
        assert_eq!(guard.doc_count(), 0, "装配阶段关键词镜像应为空");
        assert_eq!(guard.pool_len(), 0);

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

    /// 配置双写用例：带 config_path 装配后，save_config 双侧落盘并热重载内存快照。
    #[tokio::test]
    async fn save_config_updates_in_memory_snapshot() {
        let dir = temp_dir("config-save");
        let db_path = dir.join("assistant.db");
        let config_path = dir.join("config.toml");
        let engine =
            Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
                .await
                .expect("引擎装配应成功");

        // 装配期只读：未生成配置文件，快照为默认值
        assert!(!config_path.exists(), "装配不得写回 config.toml");
        assert_eq!(engine.config().session.l1_idle_minutes, 10);

        let mut cfg = engine.config().as_ref().clone();
        cfg.session.l1_idle_minutes = 42;
        let result = engine.save_config(&cfg).await.expect("保存配置应成功");
        assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

        // 内存快照已热重载（后续用例读取生效）
        assert_eq!(
            engine.config().session.l1_idle_minutes,
            42,
            "save_config 后快照应更新"
        );

        // 文件侧与 DB 侧同步落盘
        let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
        let file_cfg: TestConfig = toml::from_str(&text).expect("文件应为合法 TOML");
        assert_eq!(file_cfg.session.l1_idle_minutes, 42);
        let stored = engine
            .storage()
            .get_setting("config.session.l1_idle_minutes")
            .await
            .expect("读取 settings 应成功");
        assert_eq!(stored.as_deref(), Some("42"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置重载用例：文件改值后 reload → 快照与 DB 对齐（以文件为准回写）。
    #[tokio::test]
    async fn reload_config_reads_file_and_writes_db() {
        let dir = temp_dir("config-reload");
        let db_path = dir.join("assistant.db");
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "[utt]\ntheta_gap_minutes = 30\n").expect("写入配置应成功");

        let engine =
            Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
                .await
                .expect("引擎装配应成功");
        assert_eq!(engine.config().utt.theta_gap_minutes, 30);

        // 外部直写 DB 制造不一致残值
        engine
            .storage()
            .set_setting("config.utt.theta_gap_minutes", "60")
            .await
            .expect("写入 settings 应成功");
        // 文件改值 → reload：一致性校验以文件为准回写 DB，并热重载快照
        std::fs::write(&config_path, "[utt]\ntheta_gap_minutes = 25\n").expect("写入配置应成功");
        let outcome = engine.reload_config().await.expect("重载应成功");

        assert_eq!(outcome.config.utt.theta_gap_minutes, 25);
        assert!(
            outcome
                .mismatches
                .iter()
                .any(|m| m.key == "config.utt.theta_gap_minutes"),
            "不一致应记入 mismatch: {:?}",
            outcome.mismatches
        );
        assert_eq!(
            engine.config().utt.theta_gap_minutes,
            25,
            "reload 后快照应与文件一致"
        );
        let stored = engine
            .storage()
            .get_setting("config.utt.theta_gap_minutes")
            .await
            .expect("读取 settings 应成功");
        assert_eq!(stored.as_deref(), Some("25"), "DB 应以文件为准回写");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置用例边界：注入构造（空 config_path）调用配置用例返回显式错误，不 panic。
    #[tokio::test]
    async fn config_writer_requires_config_path() {
        let (engine, _storage, dir) = crate::test_support::engine_with_db("config-no-path").await;
        assert!(
            engine.config_path().as_os_str().is_empty(),
            "注入构造不携带配置路径"
        );

        let err = engine
            .save_config(&TestConfig::default())
            .await
            .expect_err("空 config_path 应报错");
        assert_eq!(err.category(), "config");

        let err = engine
            .reload_config()
            .await
            .expect_err("空 config_path 应报错");
        assert_eq!(err.category(), "config");

        let err = engine
            .load_full_config()
            .await
            .expect_err("空 config_path 应报错");
        assert_eq!(err.category(), "config");

        let err = engine
            .sync_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect_err("空 config_path 应报错");
        assert_eq!(err.category(), "config");

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

    /// 连接池门面：注入构造默认未附着；附着后读取返回共享句柄；装配路径自动携带。
    #[tokio::test]
    async fn attach_sqlite_pool_roundtrip() {
        let dir = temp_dir("pool-attach");
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
            .await
            .expect("初始化测试库应成功");
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool.clone()));
        let keychain = Arc::new(Keychain::new());
        let llm = build_llm_provider(&BackendConfig::lm_studio_default(), &keychain, None)
            .expect("构建本地 provider 应成功");

        let engine = Engine::from_parts(storage, llm, None, TestConfig::default());
        assert!(engine.sqlite_pool().is_none(), "注入构造默认不携带连接池");
        engine.attach_sqlite_pool(pool);
        assert!(engine.sqlite_pool().is_some(), "附着后应可读取连接池句柄");

        // 装配路径（open_with）自动携带连接池句柄
        let engine = Engine::open(db_path).await.expect("引擎装配应成功");
        assert!(
            engine.sqlite_pool().is_some(),
            "装配路径应自动携带连接池句柄"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
