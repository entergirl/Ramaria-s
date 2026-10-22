//! crates/ramaria-service/src/engine/assemble/open.rs - Ramaria 引擎装配入口
//!
//! 设计特点:
//! - 装配顺序固定：连接池 + migration → 配置只读加载 → 后端配置 → provider 装配 → 检索器占位
//! - 注入构造不创建连接池与迁移、不加载配置文件，依赖生命周期由调用方负责
//! - 缺省策略与钩子按配置闸门映射，入口层可整体覆盖

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::{AppState, BackendConfig};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::job::JobManagerConfig;
use ramaria_memory::keyword::KeywordService;
use ramaria_storage::SqliteStorage;

use crate::engine::{Engine, EngineOptions};
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;

use super::providers;
use super::{default_config_path, load_config_readonly, path_log_label};

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
        let job_manager_config = options.job_manager_config;

        // ---- 1. 数据库连接池 + migration ----
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone())).await?;
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool.clone()));
        // 连接池句柄随引擎保留（缓存装配会消费一份克隆，先留出引擎侧句柄）
        let engine_pool = pool.clone();

        // ---- 2. 配置加载（只读，不双写）----
        let config_path = options
            .config_path
            .unwrap_or_else(|| default_config_path(&db_path));
        let (config, config_warning) = load_config_readonly(&config_path, &db_path);

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
        let llm = providers::build_llm_provider(&backend_config, &keychain, cache.clone())?;

        // ---- 5. 嵌入 provider（可选，缺失降级）----
        let embedding = providers::restore_embedding(&backend_config, config.embedding.device);

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
            config_warning: RwLock::new(config_warning),
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
            index_status_state: Arc::new(RwLock::new(crate::index::IndexStatusState::default())),
            index_progress_hook: Arc::new(RwLock::new(None)),
            index_build_inflight: Arc::new(AtomicBool::new(false)),
            index_build_lock: Arc::new(tokio::sync::Mutex::new(())),
            recall_policy: Arc::new(RwLock::new(recall_policy)),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
            seal_allowed: AtomicBool::new(true),
            proactive_sink: Arc::new(RwLock::new(None)),
            channel_proactive_sink: Arc::new(RwLock::new(None)),
            recall_inflight: Arc::new(Mutex::new(std::collections::HashSet::new())),
            vision_probe: tokio::sync::Mutex::new(None),
            job_manager_config: RwLock::new(job_manager_config),
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
            config_warning: RwLock::new(None),
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
            index_status_state: Arc::new(RwLock::new(crate::index::IndexStatusState::default())),
            index_progress_hook: Arc::new(RwLock::new(None)),
            index_build_inflight: Arc::new(AtomicBool::new(false)),
            index_build_lock: Arc::new(tokio::sync::Mutex::new(())),
            recall_policy: Arc::new(RwLock::new(recall_policy)),
            seal_hooks: Arc::new(RwLock::new(SealHooks::default())),
            seal_allowed: AtomicBool::new(true),
            proactive_sink: Arc::new(RwLock::new(None)),
            channel_proactive_sink: Arc::new(RwLock::new(None)),
            recall_inflight: Arc::new(Mutex::new(std::collections::HashSet::new())),
            vision_probe: tokio::sync::Mutex::new(None),
            job_manager_config: RwLock::new(JobManagerConfig::default()),
            state: Mutex::new(AppState::NeedsSetup),
        }
    }

    /// 设置数据库文件路径（builder 风格；注入装配的引擎补全路径派生能力）。
    ///
    /// 参数:
    /// - `db_path`: 数据库文件路径。
    ///
    /// 返回:
    /// - 设置路径后的引擎。
    pub fn with_db_path(mut self, db_path: impl Into<PathBuf>) -> Self {
        self.db_path = db_path.into();
        self
    }
}
