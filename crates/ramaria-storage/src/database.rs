//! rust/crates/ramaria-storage/src/database.rs - 数据库连接池与 migration 管理
//!
//! 设计特点:
//! - 封装 sqlx::SqlitePool 的初始化和迁移执行
//! - 支持默认路径（开发模式）、环境变量覆盖
//! - migration runner 在空库上自动创建完整 schema
//! - 错误统一转换为 RamariaError::Storage

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::PathBuf;
use tracing::info;

/// 开发模式默认数据库路径。
const DEV_DB_PATH: &str = ".ramaria-dev/assistant.db";

/// 环境变量名：覆盖数据库文件路径。
const ENV_DB_PATH: &str = "RAMARIA_DB_PATH";

// =========================================================
// Pool 初始化
// =========================================================

/// 初始化 SQLite 连接池并执行迁移。
///
/// 参数:
/// - `db_path`: 可选的数据库文件路径。`None` 时按以下优先级确定：
///   1. 环境变量 `RAMARIA_DB_PATH`
///   2. 开发模式默认 `rust/.ramaria-dev/assistant.db`
///
/// 返回:
/// - 已执行过 migration 的连接池。
///
/// 说明:
/// - 自动创建数据库文件所在目录。
/// - 连接池最大连接数为 2（SQLite 单写场景足够）。
/// - WAL 模式默认启用。
pub async fn init_pool(db_path: Option<PathBuf>) -> RamariaResult<SqlitePool> {
    let path = resolve_db_path(db_path);

    // 确保父目录存在
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            RamariaError::storage_with_source(
                format!("无法创建数据库目录: {}", parent.display()),
                e,
            )
        })?;
    }

    let path_str = path.to_string_lossy().to_string();
    info!("初始化 SQLite 数据库: {}", path_str);

    let options = SqliteConnectOptions::new()
        .filename(&path_str)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .map_err(|e| {
            RamariaError::storage_with_source(format!("SQLite 连接失败: {path_str}"), e)
        })?;

    // 运行迁移
    run_migrations(&pool).await?;

    info!("SQLite 数据库初始化完成");
    Ok(pool)
}

/// 为测试创建内存数据库连接池（已执行 migration）。
///
/// 用法:
/// - 单元测试中使用 `test_pool().await` 获取独立的内存数据库。
///
/// 返回:
/// - 指向 `sqlite::memory:` 的连接池，schema 已就绪。
///
/// 说明:
/// - 内存数据库为单连接模式，连接断开即丢失数据。
/// - 每个测试应使用独立的内存数据库，避免并行测试相互干扰。
#[cfg(test)]
pub(crate) async fn test_pool() -> RamariaResult<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .map_err(|e| RamariaError::storage_with_source("内存 SQLite 连接失败", e))?;

    run_migrations(&pool).await?;
    Ok(pool)
}

// =========================================================
// Migration Runner
// =========================================================

/// 执行 sqlx migrate 机制。
///
/// 说明:
/// - 调用 `sqlx::migrate!("./migrations")` 执行所有未执行迁移。
/// - 空库第一次调用将创建完整 schema。
/// - 迁移失败时返回带 source 的 Storage 错误。
async fn run_migrations(pool: &SqlitePool) -> RamariaResult<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("数据库 migration 执行失败", e))?;
    Ok(())
}

// =========================================================
// 路径解析
// =========================================================

/// 解析数据库文件路径。
///
/// 优先级:
/// 1. 显式传入的 `db_path`
/// 2. 环境变量 `RAMARIA_DB_PATH`
/// 3. 默认开发路径 `<workspace root>/rust/.ramaria-dev/assistant.db`
fn resolve_db_path(db_path: Option<PathBuf>) -> PathBuf {
    if let Some(p) = db_path {
        return p;
    }

    if let Ok(env_path) = std::env::var(ENV_DB_PATH)
        && !env_path.is_empty()
    {
        return PathBuf::from(env_path);
    }

    // 开发模式默认路径：相对 Cargo workspace 根
    PathBuf::from(DEV_DB_PATH)
}
