//! crates/ramaria-storage/src/database.rs - 数据库连接池与 migration 管理
//!
//! 设计特点:
//! - 封装 SqlitePool 初始化，支持默认路径、开发路径、环境变量覆盖
//! - migration runner：空库自动执行全部 migration 文件
//! - 启动预检：既有库只读核对 schema_version，不兼容直接拒绝（不迁移、不删除、不改名）
//! - WAL 模式默认启用，连接池最大 2 连接（本地应用场景）
//! - busy_timeout 默认 10 秒：多进程共用库时写锁串行，“等待”优于“失败”
//! - 测试模式支持 `sqlite::memory:` 内存数据库
//! - 开发模式默认路径 `main/.ramaria-dev/ramaria.db`（相对进程工作目录）

use crate::repo::schema_meta::CURRENT_SCHEMA_VERSION;
use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 默认开发数据库路径（相对进程工作目录；仓库内运行时即 workspace 根 `main/`）。
const DEV_DB_RELATIVE_PATH: &str = ".ramaria-dev/ramaria.db";

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
/// 1. `RAMARIA_DATA_DIR` 环境变量 + `/ramaria.db`
/// 2. 开发模式默认路径 `main/.ramaria-dev/ramaria.db`（相对进程工作目录）
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
        // 缺省路径仅用于开发 / 测试：数据目录环境变量优先，
        // 否则落在当前工作目录下的开发库（解析为绝对路径，避免相对路径随 CWD 漂移）。
        std::env::var("RAMARIA_DATA_DIR")
            .map(|d| PathBuf::from(d).join("ramaria.db"))
            .unwrap_or_else(|_| {
                std::env::current_dir()
                    .map(|dir| dir.join(DEV_DB_RELATIVE_PATH))
                    .unwrap_or_else(|_| PathBuf::from(DEV_DB_RELATIVE_PATH))
            })
    });

    // 既有库先核对版本：不兼容时给出可读错误，绝不改动原库
    ensure_compatible_schema(&path).await?;

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

/// 启动预检：既有数据库文件必须与当前程序 schema 版本一致。
///
/// 职责:
/// - 只读打开既有库，核对 `schema_meta` 表中的 `schema_version`；不兼容时返回可读错误。
///
/// 参数:
/// - `path`: 数据库文件路径（父目录可能尚不存在）。
///
/// 返回:
/// - `Ok(())`: 路径不存在、零字节文件（交给 migration 建库）或版本匹配。
/// - `Err(_)`: 版本不匹配、缺少 `schema_meta`、缺少版本键，或文件不是 SQLite 数据库。
///
/// 说明:
/// - 单连接只读打开，检查结束即关闭；拒绝路径不写原库、不自动迁移、不删除、不改名。
async fn ensure_compatible_schema(path: &Path) -> RamariaResult<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > 0 => {}
        // 零字节文件等同新库：migration 会写入完整 schema
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(RamariaError::storage_with_source(
                format!("无法读取数据库文件: {}", path.display()),
                e,
            ));
        }
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| {
            RamariaError::storage_with_source(
                format!(
                    "无法以只读方式打开数据库文件（可能不是 SQLite 数据库）: {}",
                    path.display()
                ),
                e,
            )
        })?;

    let verdict = inspect_schema_version(&pool, path).await;
    // 拒绝路径不得残留连接句柄
    pool.close().await;
    verdict
}

/// 读取 `schema_meta.schema_version` 并与当前程序版本比对。
///
/// 参数:
/// - `pool`: 只读连接池（由 `ensure_compatible_schema` 建立）。
/// - `path`: 数据库文件路径（错误信息用）。
///
/// 返回:
/// - `Ok(())`: 版本等于 `CURRENT_SCHEMA_VERSION`。
/// - `Err(_)`: 缺表 / 缺键 / 值非法 / 版本不匹配，错误文本含实际与当前版本及备份提示。
async fn inspect_schema_version(pool: &SqlitePool, path: &Path) -> RamariaResult<()> {
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        RamariaError::storage_with_source(
            format!(
                "无法读取数据库结构（可能不是 SQLite 数据库）: {}",
                path.display()
            ),
            e,
        )
    })?;
    if table.is_none() {
        return Err(RamariaError::storage(format!(
            "无法识别的数据库文件（缺少 schema_meta）: {}。请备份后删除或改名（不会自动迁移、删除或改名）",
            path.display()
        )));
    }

    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                RamariaError::storage_with_source(
                    format!("无法读取数据库 schema 版本: {}", path.display()),
                    e,
                )
            })?;
    let actual = raw
        .as_deref()
        .map(str::parse::<i32>)
        .transpose()
        .map_err(|_| {
            RamariaError::storage(format!(
                "数据库 schema_version 值非法（文件: {}）。请备份后删除或改名（不会自动迁移、删除或改名）",
                path.display()
            ))
        })?;

    match actual {
        Some(version) if version == CURRENT_SCHEMA_VERSION => Ok(()),
        Some(version) => Err(RamariaError::storage(format!(
            "数据库 schema 版本不兼容: 实际版本 {version}，当前版本 {CURRENT_SCHEMA_VERSION}（文件: {}）。请备份后删除或改名（不会自动迁移、删除或改名）",
            path.display()
        ))),
        None => Err(RamariaError::storage(format!(
            "数据库缺少 schema_version 键，无法识别版本（文件: {}）。请备份后删除或改名（不会自动迁移、删除或改名）",
            path.display()
        ))),
    }
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
