//! crates/ramaria-testkit/src/db.rs - Ramaria 测试数据库模块
//!
//! 设计特点:
//! - 临时文件库：每次测试独占目录，用例结束删除（Windows 删除失败仅忽略）
//! - 迁移驱动：统一调用 storage 的 init_pool 执行真实 migration，不写任何 DDL
//! - 内存库：单连接 + 同一 migration 文件，供需要直接持有连接池的测试使用
//! - 目录名含进程号、纳秒与自增序号，并行测试互不冲突

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// 临时目录序号（同一进程内并行测试唯一）。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 测试专用临时数据库。
///
/// 职责:
/// - 持有独立的临时目录与迁移完成的连接池；
/// - 提供连接池与库文件路径；
/// - 用例结束调用 [`TempDb::close`] 或依赖 Drop 清理目录。
///
/// 字段约定:
/// - `dir`: 本次测试独占的临时目录；
/// - `db_path`: 目录内的库文件路径；
/// - `pool`: 迁移完成的连接池（close 后为 None）。
pub struct TempDb {
    dir: PathBuf,
    db_path: PathBuf,
    pool: Option<SqlitePool>,
}

impl TempDb {
    /// 获取连接池（已执行全部 migration）。
    pub fn pool(&self) -> &SqlitePool {
        self.pool
            .as_ref()
            .expect("TempDb 连接池在 close 之前应始终可用")
    }

    /// 获取库文件路径。
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// 关闭连接池并删除临时目录。
    ///
    /// 说明:
    /// - 先关闭连接池释放文件句柄，再删除目录；
    /// - Windows 上句柄释放可能滞后于 `close`，删除按短间隔重试；
    /// - 全部重试失败则保留目录（不 panic），由调用方或系统清理。
    pub async fn close(mut self) {
        if let Some(pool) = self.pool.take() {
            pool.close().await;
        }
        // 让 sqlx 的后台关闭任务完成，降低 Windows 文件句柄滞后概率
        tokio::task::yield_now().await;
        remove_dir_with_retry(&self.dir, true).await;
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        drop(self.pool.take());
        // Drop 不能 await：同步短重试，失败保留目录
        for attempt in 0..5 {
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < 4 => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => return,
            }
        }
    }
}

/// 异步删除目录：短间隔重试，`allow_retry=false` 时只尝试一次。
async fn remove_dir_with_retry(dir: &Path, allow_retry: bool) {
    let attempts = if allow_retry { 5 } else { 1 };
    for attempt in 0..attempts {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempt + 1 < attempts => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(_) => return,
        }
    }
}

/// 创建迁移完成的临时文件库。
///
/// 返回:
/// - 独占临时目录与连接池；用例结束应调用 [`TempDb::close`]。
pub async fn db() -> TempDb {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(&dir).expect("创建测试临时目录应成功");
    let db_path = dir.join("ramaria.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .unwrap_or_else(|e| panic!("初始化测试数据库失败: {e}"));
    TempDb {
        dir,
        db_path,
        pool: Some(pool),
    }
}

/// 创建迁移完成的内存库（单连接）。
///
/// 职责:
/// - 供需要直接持有连接池的测试使用（如导入写入层）；
/// - 与 storage 的测试库口径一致：单连接、启用外键、执行同一 migration 文件。
///
/// 返回:
/// - 已迁移的内存连接池；调用方负责关闭。
pub async fn memory_db() -> RamariaResult<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| RamariaError::storage_with_source("无法创建测试内存数据库", e))?;
    sqlx::migrate!("../ramaria-storage/migrations")
        .run(&pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("测试 migration 失败", e))?;
    Ok(pool)
}

/// 生成唯一临时目录路径（不创建目录）。
fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ramaria-testkit-{}-{nanos}-{seq}",
        std::process::id()
    ))
}
