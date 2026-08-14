//! crates/ramaria-storage/src/database.rs - 数据库连接池与 migration 管理
//!
//! 设计特点:
//! - 封装 SqlitePool 初始化，支持默认路径、开发路径、环境变量覆盖
//! - migration runner：空库自动执行全部 migration 文件
//! - WAL 模式默认启用，连接池最大 2 连接（本地应用场景）
//! - 测试模式支持 `sqlite::memory:` 内存数据库
//! - 开发模式默认路径 `rust/.ramaria-dev/assistant.db`

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::PathBuf;

/// 默认开发数据库路径（相对于 workspace 根 `rust/`）。
const DEV_DB_RELATIVE_PATH: &str = ".ramaria-dev/assistant.db";

/// 初始化数据库连接池并执行 migration。
///
/// 参数:
/// - `db_path`: 可选显式数据库路径。为 None 时按优先级查找：
/// 1. `RAMARIA_DATA_DIR` 环境变量 + `/assistant.db`
/// 2. 开发模式默认路径 `rust/.ramaria-dev/assistant.db`
///
/// 返回:
/// - 成功时返回已连接且已执行 migration 的连接池。
/// - 失败时返回 Storage 错误。
///
/// 说明:
/// - 连接启用 WAL 模式和 foreign_keys。
/// - 首次启动时自动创建数据库文件并执行所有 migration。
pub async fn init_pool(db_path: Option<PathBuf>) -> RamariaResult<SqlitePool> {
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
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
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
