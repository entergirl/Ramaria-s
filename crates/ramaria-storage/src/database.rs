//! crates/ramaria-storage/src/database.rs - 数据库连接池与 migration 管理
//!
//! 设计特点:
//! - 封装 SqlitePool 初始化，支持默认路径、开发路径、环境变量覆盖
//! - migration runner：空库自动执行全部 migration 文件
//! - WAL 模式默认启用，连接池最大 2 连接（本地应用场景）
//! - busy_timeout 默认 10 秒：多进程共用库时写锁串行，“等待”优于“失败”
//! - 测试模式支持 `sqlite::memory:` 内存数据库
//! - 开发模式默认路径 `main/.ramaria-dev/assistant.db`（相对进程工作目录）

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::PathBuf;
use std::time::Duration;

/// 默认开发数据库路径（相对进程工作目录；仓库内运行时即 workspace 根 `main/`）。
const DEV_DB_RELATIVE_PATH: &str = ".ramaria-dev/assistant.db";

/// 默认写锁等待上限（秒）。
///
/// 说明:
/// - 多进程并发写同一个库时，WAL 下写锁串行；等待写锁释放（而非直接失败）
///   更符合本地应用的使用预期。
pub const DEFAULT_BUSY_TIMEOUT_SECONDS: u64 = 10;

/// 连接池初始化调优参数。
///
/// 职责:
/// - 承载连接池初始化时可调的写锁等待与并发参数；默认值对应正式装配，
///   多池并发测试等场景可按需覆盖。
///
/// 字段约定:
/// - `busy_timeout`: 单条 SQL 等待写锁释放的上限（SQLite busy handler 超时）。
/// - `max_connections`: 连接池最大连接数。
#[derive(Debug, Clone, Copy)]
pub struct PoolTuning {
    /// 写锁等待上限。
    pub busy_timeout: Duration,
    /// 连接池最大连接数。
    pub max_connections: u32,
}

impl Default for PoolTuning {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(DEFAULT_BUSY_TIMEOUT_SECONDS),
            max_connections: 2,
        }
    }
}

/// 初始化数据库连接池并执行 migration（默认调优参数）。
///
/// 参数:
/// - `db_path`: 可选显式数据库路径。为 None 时按优先级查找：
/// 1. `RAMARIA_DATA_DIR` 环境变量 + `/assistant.db`
/// 2. 开发模式默认路径 `main/.ramaria-dev/assistant.db`（相对进程工作目录）
///
/// 返回:
/// - 成功时返回已连接且已执行 migration 的连接池。
/// - 失败时返回 Storage 错误。
///
/// 说明:
/// - 连接启用 WAL 模式和 foreign_keys。
/// - 写锁等待上限取 [`PoolTuning::default`]（[`DEFAULT_BUSY_TIMEOUT_SECONDS`] 秒），
///   多进程共库时等待写锁释放而非直接失败。
/// - 首次启动时自动创建数据库文件并执行所有 migration。
pub async fn init_pool(db_path: Option<PathBuf>) -> RamariaResult<SqlitePool> {
    init_pool_with(db_path, PoolTuning::default()).await
}

/// 初始化数据库连接池并执行 migration（自定义调优参数）。
///
/// 参数:
/// - `db_path`: 数据库路径解析规则同 [`init_pool`]。
/// - `tuning`: 连接池调优参数（写锁等待上限与最大连接数）。
///
/// 返回:
/// - 成功时返回已连接且已执行 migration 的连接池。
/// - 失败时返回 Storage 错误。
///
/// 说明:
/// - 连接启用 WAL 模式和 foreign_keys，写锁忙时最多等待 `tuning.busy_timeout`。
/// - 首次启动时自动创建数据库文件并执行所有 migration。
pub async fn init_pool_with(
    db_path: Option<PathBuf>,
    tuning: PoolTuning,
) -> RamariaResult<SqlitePool> {
    let path = db_path.unwrap_or_else(|| {
        std::env::var("RAMARIA_DATA_DIR")
            .map(|d| PathBuf::from(d).join("assistant.db"))
            .unwrap_or_else(|_| PathBuf::from(DEV_DB_RELATIVE_PATH))
    });

    // 确保父目录存在
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RamariaError::storage_with_source("无法创建数据库目录", e))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(tuning.busy_timeout)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(tuning.max_connections)
        .connect_with(options)
        .await
        .map_err(|e| {
            RamariaError::storage_with_source(format!("无法连接数据库: {}", path.display()), e)
        })?;

    // 执行 migration
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("数据库 migration 失败", e))?;

    Ok(pool)
}

/// 创建测试用内存数据库连接池。
///
/// 返回:
/// - 内存数据库连接池，已执行全部 migration，测试结束后自动销毁。
#[cfg(test)]
pub async fn init_test_pool() -> RamariaResult<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| RamariaError::storage_with_source("无法创建测试数据库", e))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("测试 migration 失败", e))?;

    Ok(pool)
}
