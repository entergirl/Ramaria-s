//! crates/ramaria-service/tests/parity/support/app_env.rs - 应用装配侧的隔离对照环境
//!
//! 设计特点:
//! - 与 `ParityEnv` 对称：同一套"临时 SQLite + 脚本化 mock LLM + 预计算向量 mock 嵌入"
//!   的隔离口径，供应用装配与服务装配在同一 fixture 上并跑逐字对照
//! - 真实存储：每个环境一个临时 SQLite 文件（执行全量 migration），用例在真实存储语义上
//!   执行；不使用内存 mock 顶替（与"行为等价"验证目标一致）
//! - 依赖注入：应用装配注入脚本化 LLM / 预计算向量嵌入与独立 keychain 句柄，
//!   不触网、不加载模型
//! - 就绪装配：`setup_ready` 对齐应用装配的既有就绪序列（写入后端配置 + 索引版本 +
//!   状态刷新），使对话路径在可控状态下执行
//! - 清理双保险：显式 `cleanup()` 先释放应用实例与存储引用、再优雅关闭连接池并删除目录；
//!   未显式清理时 `Drop` 兜底删除（失败仅记录，不影响测试结论）

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_app::App;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig};
use ramaria_llm::keychain::Keychain;
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;

use super::env::{DEFAULT_ASSISTANT_REPLY, unique_temp_dir};
use super::error::{ParityError, ParityResult};
use super::log;
use super::mocks::{DeterministicEmbedding, ScriptedLlm};

/// 应用装配侧一次对照场景的隔离环境。
///
/// 职责:
/// - 持有临时目录、库文件路径、连接池、已装配的应用实例与嵌入 mock；
/// - 提供应用 / 存储 / 脚本 LLM 的访问器，供 fixture 造数、场景执行与快照断言；
/// - 生命周期结束时释放资源（`cleanup`）或由 `Drop` 兜底。
///
/// 字段约定:
/// - `app` / `storage` 为 `Option`：仅用于 `cleanup` 时显式释放，访问器保证其存在；
/// - `pool` 为独立 clone：`cleanup` 用它优雅关闭连接并释放文件句柄（Windows 下很重要）。
pub struct AppEnv {
    dir: PathBuf,
    db_path: PathBuf,
    pool: SqlitePool,
    storage: Option<Arc<SqliteStorage>>,
    app: Option<App>,
    llm: Arc<ScriptedLlm>,
    embedding: Arc<DeterministicEmbedding>,
}

impl AppEnv {
    /// 构造环境：注入"固定回复"的脚本化 LLM（默认配置）。
    ///
    /// 参数:
    /// - `tag`: 环境标签（进入临时目录名，便于排查残留目录）。
    pub async fn new(tag: &str) -> ParityResult<Self> {
        Self::with_llm(tag, Arc::new(ScriptedLlm::reply(DEFAULT_ASSISTANT_REPLY))).await
    }

    /// 构造环境：注入指定脚本化 LLM（默认配置）。
    ///
    /// 用法:
    /// - 生成 / 封存路径注入按序回复的脚本；
    /// - 调用方经 `llm()` 访问器读取本轮 `ChatRequest` 与调用计数。
    pub async fn with_llm(tag: &str, llm: Arc<ScriptedLlm>) -> ParityResult<Self> {
        log::init_parity_log();

        let dir = unique_temp_dir(tag);
        let db_path = dir.join("parity.db");

        // 连接池初始化即执行全量 migration（空库建到当前基线 schema）
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
            .await
            .map_err(|e| ParityError::env(format!("初始化测试库 {}", db_path.display()), e))?;

        let storage = Arc::new(SqliteStorage::new(pool.clone()));
        let embedding = Arc::new(DeterministicEmbedding::new());
        let app = App::new(
            Arc::clone(&storage) as Arc<dyn StorageBackend>,
            Arc::clone(&llm) as Arc<dyn LlmProvider>,
            Some(Arc::clone(&embedding) as Arc<dyn EmbeddingProvider>),
            RamariaConfig::default(),
            Arc::new(Keychain::new()),
        );

        Ok(Self {
            dir,
            db_path,
            pool,
            storage: Some(storage),
            app: Some(app),
            llm,
            embedding,
        })
    }

    /// 应用实例引用（场景执行入口）。
    pub fn app(&self) -> &App {
        self.app.as_ref().expect("环境未清理前应用实例应始终可用")
    }

    /// 存储引用（fixture 造数与结果断言）。
    pub fn storage(&self) -> &Arc<SqliteStorage> {
        self.storage.as_ref().expect("环境未清理前存储应始终可用")
    }

    /// 脚本 LLM 引用（读取本轮请求与调用计数）。
    pub fn llm(&self) -> &Arc<ScriptedLlm> {
        &self.llm
    }

    /// 嵌入 mock 引用（断言向量通道是否被使用）。
    pub fn embedding(&self) -> &Arc<DeterministicEmbedding> {
        &self.embedding
    }

    /// 库文件路径（供需要独立连接池的造数场景）。
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 把应用推进到对话可用的就绪状态。
    ///
    /// 序列（对齐应用装配的既有就绪口径）:
    /// 1. 写入本地后端配置（`lm_studio` 缺省）；
    /// 2. 写入索引版本（已构建）；
    /// 3. 刷新设置状态，并校验结果处于对话可用态（`Ready` / `Degraded`）。
    pub async fn setup_ready(&self) -> ParityResult<()> {
        self.storage()
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .map_err(|e| ParityError::env("写入对照后端配置", e))?;
        self.storage()
            .set_index_version(1)
            .await
            .map_err(|e| ParityError::env("写入对照索引版本", e))?;
        let state = self
            .app()
            .refresh_setup_state()
            .await
            .map_err(|e| ParityError::env("刷新应用状态", e))?;
        match state {
            AppState::Ready | AppState::Degraded => Ok(()),
            other => Err(ParityError::env(
                "刷新应用状态",
                format!("状态应为对话可用（Ready / Degraded），实际 {other}"),
            )),
        }
    }

    /// 显式清理：释放应用实例与存储引用 → 优雅关闭连接池 → 删除临时目录。
    ///
    /// 说明:
    /// - 关闭池失败只记录日志，不使测试失败（临时目录残留由 `Drop` 兜底与系统清理）；
    /// - 删除目录失败仅记录 debug（Windows 下偶发句柄延迟释放）。
    pub async fn cleanup(mut self) {
        self.app = None;
        self.storage = None;

        // 记录库文件位置后退场：便于排查跨连接场景下的残留句柄
        tracing::debug!(db = %self.db_path().display(), "清理对照测试环境");
        // sqlx 的 `close` 为幂等收敛操作：已关闭或并发关闭均视为成功
        self.pool.close().await;
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            tracing::debug!(dir = %self.dir.display(), error = %e, "删除对照测试临时目录失败（忽略）");
        }
        // 标记已清理，避免 Drop 重复删除
        self.dir = PathBuf::new();
    }
}

impl Drop for AppEnv {
    fn drop(&mut self) {
        if self.dir.as_os_str().is_empty() {
            return;
        }
        // 兜底清理：未调用 cleanup 的路径（如测试提前 panic）尽力删除目录。
        // 连接池此时可能仍持有句柄，删除失败属预期，不打印噪声。
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
