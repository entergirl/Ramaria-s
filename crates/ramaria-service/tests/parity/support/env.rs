//! crates/ramaria-service/tests/parity/support/env.rs - 对照测试环境（隔离库 + 引擎装配）
//!
//! 设计特点:
//! - 真实存储：每个环境一个临时 SQLite 文件（执行全量 migration），用例在真实存储语义上
//!   执行；不使用内存 mock 顶替（与"行为等价"验证目标一致）
//! - 依赖注入：`Engine::from_parts` 注入脚本化 LLM / 预计算向量嵌入 / 保守召回策略，
//!   不触网、不加载模型
//! - 隔离性：目录名含纳秒时间戳与进程内自增序号，多测试并行互不干扰
//! - 清理双保险：显式 `cleanup()` 先释放引擎与存储引用、再优雅关闭连接池并删除目录；
//!   未显式清理时 `Drop` 兜底删除（失败仅记录，不影响测试结论）
//! - 多进程等价物：`db_path()` 暴露库文件路径，供"第二个连接池写入"模拟跨进程场景

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;

use ramaria_service::{Engine, RecallPolicy};

use super::error::{ParityError, ParityResult};
use super::log;
use super::mocks::{DeterministicEmbedding, ScriptedLlm};

/// 默认脚本回复：chat 类路径未显式指定脚本时使用（短句，便于字符数断言）。
pub const DEFAULT_ASSISTANT_REPLY: &str = "嗯，我在听。";

/// 进程内唯一序号（与时间戳组合，避免同纳秒内多个环境目录冲突）。
static ENV_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 一次对照场景的隔离环境。
///
/// 职责:
/// - 持有临时目录、库文件路径、连接池与已装配的引擎；
/// - 提供存储 / 引擎 / 嵌入 mock 的访问器，供 fixture 造数与快照断言；
/// - 生命周期结束时释放资源（`cleanup`）或由 `Drop` 兜底。
///
/// 字段约定:
/// - `engine` / `storage` 为 `Option`：仅用于 `cleanup` 时显式释放，访问器保证其存在；
/// - `pool` 为独立 clone：`cleanup` 用它优雅关闭连接并释放文件句柄（Windows 下很重要）。
pub struct ParityEnv {
    dir: PathBuf,
    db_path: PathBuf,
    pool: SqlitePool,
    storage: Option<Arc<SqliteStorage>>,
    engine: Option<Arc<Engine>>,
    embedding: Arc<DeterministicEmbedding>,
}

impl ParityEnv {
    /// 构造环境：注入"固定回复"的脚本化 LLM（默认配置）。
    ///
    /// 参数:
    /// - `tag`: 环境标签（进入临时目录名，便于排查残留目录）。
    pub async fn new(tag: &str) -> ParityResult<Self> {
        let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::reply(DEFAULT_ASSISTANT_REPLY));
        Self::with_llm(tag, llm).await
    }

    /// 构造环境：注入指定 LLM（默认配置）。
    ///
    /// 用法:
    /// - 封存路径注入返回 L1 摘要 JSON 的脚本；
    /// - chat 路径注入脚本后保留 `Arc<ScriptedLlm>` 引用以断言请求结构；
    /// - 降级路径注入恒失败脚本。
    pub async fn with_llm(tag: &str, llm: Arc<dyn LlmProvider>) -> ParityResult<Self> {
        Self::with_llm_and_config(tag, llm, RamariaConfig::default()).await
    }

    /// 构造环境：注入指定 LLM 与生效配置（配置项覆盖场景使用）。
    ///
    /// 说明:
    /// - 初始化顺序：临时目录 → 连接池（含 migration）→ 存储句柄 → 嵌入 mock → 引擎注入；
    /// - 引擎走 `from_parts`：不读配置文件、不写回任何配置，行为只由注入依赖与 `config` 决定。
    pub async fn with_llm_and_config(
        tag: &str,
        llm: Arc<dyn LlmProvider>,
        config: RamariaConfig,
    ) -> ParityResult<Self> {
        log::init_parity_log();

        let dir = unique_temp_dir(tag);
        let db_path = dir.join("parity.db");

        // 连接池初始化即执行全量 migration（空库建到当前基线 schema）
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
            .await
            .map_err(|e| ParityError::env(format!("初始化测试库 {}", db_path.display()), e))?;

        let storage = Arc::new(SqliteStorage::new(pool.clone()));
        let embedding = Arc::new(DeterministicEmbedding::new());
        // 具体类型 → trait 对象的隐式上转（`Arc<DeterministicEmbedding>` → `Arc<dyn EmbeddingProvider>`）
        let embedding_provider: Arc<dyn EmbeddingProvider> = embedding.clone();
        let engine = Engine::from_parts(
            Arc::clone(&storage) as Arc<dyn StorageBackend>,
            llm,
            Some(embedding_provider),
            config,
        );
        // 对照环境对齐 MCP 宿主装配口径：装配层显式注入保守策略（原文不出端、全部人格可见），
        // 不依赖服务层的配置映射缺省——对照基线按冻结口径比对。
        engine.set_recall_policy(RecallPolicy::default());
        let engine = Arc::new(engine);

        Ok(Self {
            dir,
            db_path,
            pool,
            storage: Some(storage),
            engine: Some(engine),
            embedding,
        })
    }

    /// 引擎引用（用例执行入口）。
    pub fn engine(&self) -> &Arc<Engine> {
        self.engine.as_ref().expect("环境未清理前引擎应始终可用")
    }

    /// 存储引用（fixture 造数与结果断言）。
    pub fn storage(&self) -> &Arc<SqliteStorage> {
        self.storage.as_ref().expect("环境未清理前存储应始终可用")
    }

    /// 嵌入 mock 引用（断言向量通道是否被使用）。
    pub fn embedding(&self) -> &Arc<DeterministicEmbedding> {
        &self.embedding
    }

    /// 库文件路径（模拟"第二个连接池"的跨进程写入场景）。
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 显式清理：释放引擎与存储引用 → 优雅关闭连接池 → 删除临时目录。
    ///
    /// 说明:
    /// - 关闭池失败只记录日志，不使测试失败（临时目录残留由 `Drop` 兜底与系统清理）；
    /// - 删除目录失败仅记录 debug（Windows 下偶发句柄延迟释放）。
    pub async fn cleanup(mut self) {
        self.engine = None;
        self.storage = None;

        // sqlx 的 `close` 为幂等收敛操作：已关闭或并发关闭均视为成功
        self.pool.close().await;
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            tracing::debug!(dir = %self.dir.display(), error = %e, "删除对照测试临时目录失败（忽略）");
        }
        // 标记已清理，避免 Drop 重复删除
        self.dir = PathBuf::new();
    }
}

impl Drop for ParityEnv {
    fn drop(&mut self) {
        if self.dir.as_os_str().is_empty() {
            return;
        }
        // 兜底清理：未调用 cleanup 的路径（如测试提前 panic）尽力删除目录。
        // 连接池此时可能仍持有句柄，删除失败属预期，不打印噪声。
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// =========================================================
// 目录工具
// =========================================================

/// 生成唯一临时目录路径（不创建；由连接池初始化创建父目录）。
///
/// 说明:
/// - 目录名含时间戳与进程内自增序号，多个对照环境（含应用装配侧）共用同一序号源，
///   并行建立时互不冲突。
pub(super) fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let sequence = ENV_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ramaria-parity-{tag}-{nanos}-{sequence}"))
}
