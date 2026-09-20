//! crates/ramaria-storage/src/retry.rs - SQLite 写锁忙等重试模块
//!
//! 设计特点:
//! - 只针对 SQLITE_BUSY / SQLITE_LOCKED 类错误重试，其余错误原样透传，不改变失败语义
//! - 有限次尝试（含首次最多 3 次）：避免持锁进程长时间不释放时拖垮调用方
//! - 线性退避（基数 100ms * 第 n 次重试）：多进程写锁冲突多为瞬时交接，短退避即可化解
//! - 放在存储层：忙判定依赖 SQLite 错误码与驱动行为，属存储实现细节；上层只消费最终结果
//! - 与连接池 busy_timeout 互补：busy_timeout 在单条 SQL 内等待，
//!   重试覆盖“等满超时仍冲突”的边界，两者叠加降低多进程共库的失败率

use std::future::Future;
use std::time::Duration;

/// 写锁忙时的最大尝试次数（含首次尝试，即最多再重试 2 次）。
pub const BUSY_RETRY_MAX_ATTEMPTS: u32 = 3;

/// 退避基数（毫秒）：第 n 次重试等待 `基数 * n` 毫秒。
pub const BUSY_RETRY_BASE_DELAY_MS: u64 = 100;

// =========================================================
// 忙错误判定
// =========================================================

/// 判断 sqlx 错误是否为 SQLite 写锁忙类错误（SQLITE_BUSY / SQLITE_LOCKED）。
///
/// 判定:
/// - 首选错误码：数据库错误码解析为 u32 后 `% 256` 命中主码 5（BUSY）/ 6（LOCKED）。
///   sqlx 返回的是扩展错误码（如 SQLITE_BUSY_RECOVERY = 261），`% 256` 归一化回主码。
/// - 回退文案匹配：错误码缺失或非数字时，按 SQLite 官方文案
///   （`database is locked` / `database table is locked` / `database is busy`）判定。
///
/// 返回:
/// - `true`: 可退避重试的写锁忙类错误。
/// - `false`: 其他错误（应原样透传，不做重试）。
pub fn is_busy_error(err: &sqlx::Error) -> bool {
    let Some(db_err) = err.as_database_error() else {
        return false;
    };

    if let Some(code) = db_err.code() {
        if let Ok(extended) = code.parse::<u32>() {
            let primary = extended % 256;
            if primary == 5 || primary == 6 {
                return true;
            }
        }
    }

    let message = db_err.message().to_lowercase();
    message.contains("database is locked")
        || message.contains("database table is locked")
        || message.contains("database is busy")
}

// =========================================================
// 有限次退避重试
// =========================================================

/// 执行写操作，并在 SQLite 写锁忙时做有限次线性退避重试。
///
/// 用法:
/// ```ignore
/// with_busy_retry("保存消息", || async {
///     sqlx::query("...").bind(...).execute(pool).await
/// })
/// .await
/// .storage_err("保存消息失败")?;
/// ```
///
/// 参数:
/// - `operation`: 操作名（仅用于日志，中文描述）。
/// - `op`: 返回新 future 的闭包；每次尝试重新执行，busy 时等待后重试。
///
/// 返回:
/// - 非 busy 错误立即透传；busy 且重试额度用尽时返回最后一次错误。
pub async fn with_busy_retry<T, F, Fut>(operation: &str, mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt: u32 = 1;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !is_busy_error(&err) {
                    return Err(err);
                }
                if attempt >= BUSY_RETRY_MAX_ATTEMPTS {
                    tracing::error!(
                        operation,
                        attempt,
                        error = %err,
                        "写锁忙重试额度用尽，放弃本次写操作"
                    );
                    return Err(err);
                }
                let delay_ms = BUSY_RETRY_BASE_DELAY_MS * u64::from(attempt);
                tracing::warn!(operation, attempt, delay_ms, "写锁忙，退避后重试");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                attempt += 1;
            }
        }
    }
}

// =========================================================
// 单元测试（真实文件库 + 独立连接池，验证写锁冲突路径）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{PoolTuning, init_pool_with};
    use crate::repo::{messages, sessions};
    use ramaria_core::error::RamariaResult;
    use ramaria_core::types::{Message, MessageRole, MessageSource};
    use sqlx::SqlitePool;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::path::{Path, PathBuf};

    /// 创建唯一临时数据库文件路径（测试尾部尽力清理）。
    fn temp_db_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应可读")
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("ramaria-storage-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
        dir.join("retry-test.db")
    }

    /// 打开独立连接池：写锁冲突立即报错（busy_timeout = 0），用于构造锁竞争。
    async fn open_raw_pool(path: &Path, max_connections: u32) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::ZERO)
            .journal_mode(SqliteJournalMode::Wal);
        SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .expect("测试连接池创建应成功")
    }

    /// 创建写锁探针表。
    async fn create_probe_table(pool: &SqlitePool) {
        sqlx::query("CREATE TABLE IF NOT EXISTS busy_probe (id INTEGER PRIMARY KEY)")
            .execute(pool)
            .await
            .expect("建表成功");
    }

    /// 在事务内写入一行并持有写锁（不提交）。
    async fn hold_write_lock(pool: &SqlitePool) -> sqlx::Transaction<'static, sqlx::Sqlite> {
        let mut txn = pool.begin().await.expect("开启事务成功");
        sqlx::query("INSERT INTO busy_probe (id) VALUES (1)")
            .execute(&mut *txn)
            .await
            .expect("持锁写入成功");
        txn
    }

    /// 写锁忙识别：持锁写被拒命中 busy 判定；UNIQUE 冲突不误报。
    #[tokio::test]
    async fn is_busy_error_detects_locked_write() {
        let db_path = temp_db_path("busy-detect");
        let pool_a = open_raw_pool(&db_path, 1).await;
        let pool_b = open_raw_pool(&db_path, 1).await;
        create_probe_table(&pool_a).await;

        // A 持写锁（事务内 INSERT 不提交），B 的写入应被立即拒绝
        let txn = hold_write_lock(&pool_a).await;
        let err = sqlx::query("INSERT INTO busy_probe (id) VALUES (2)")
            .execute(&pool_b)
            .await
            .expect_err("B 写入应因写锁忙被拒");
        assert!(is_busy_error(&err), "持锁冲突应命中 busy 判定: {err}");

        // 释放写锁后，重复主键写入是约束错误，不应误报为 busy
        txn.commit().await.expect("提交成功");
        let err = sqlx::query("INSERT INTO busy_probe (id) VALUES (1)")
            .execute(&pool_b)
            .await
            .expect_err("重复主键应被拒绝");
        assert!(!is_busy_error(&err), "约束冲突不应误报 busy: {err}");

        drop((pool_a, pool_b));
        let _ = std::fs::remove_dir_all(db_path.parent().expect("临时路径应有父目录"));
    }

    /// 写锁释放后重试成功：退避等待覆盖持锁时长窗口。
    #[tokio::test]
    async fn busy_retry_succeeds_after_lock_release() {
        let db_path = temp_db_path("busy-release");
        let pool_a = open_raw_pool(&db_path, 1).await;
        let pool_b = open_raw_pool(&db_path, 1).await;
        create_probe_table(&pool_a).await;

        let txn = hold_write_lock(&pool_a).await;

        // 另起 task 约 50ms 后提交释放写锁（短于第一次退避 100ms）
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            txn.commit().await.expect("延迟提交成功");
        });

        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            with_busy_retry("测试写入", || async {
                sqlx::query("INSERT INTO busy_probe (id) VALUES (2)")
                    .execute(&pool_b)
                    .await
            })
            .await
        })
        .await
        .expect("重试不应挂死");
        outcome.expect("写锁释放后重试应成功");

        release.await.expect("释放任务不应 panic");
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM busy_probe")
            .fetch_one(&pool_b)
            .await
            .expect("计数成功");
        assert_eq!(count.0, 2, "A 与 B 的写入都应落库");

        drop((pool_a, pool_b));
        let _ = std::fs::remove_dir_all(db_path.parent().expect("临时路径应有父目录"));
    }

    /// 额度用尽：持锁不释放时返回最后一次 busy 错误，不无限等待。
    #[tokio::test]
    async fn busy_retry_gives_up_after_max_attempts() {
        let db_path = temp_db_path("busy-giveup");
        let pool_a = open_raw_pool(&db_path, 1).await;
        let pool_b = open_raw_pool(&db_path, 1).await;
        create_probe_table(&pool_a).await;

        // A 一直持锁（测试结束前不回滚）
        let txn = hold_write_lock(&pool_a).await;

        let result = tokio::time::timeout(Duration::from_secs(10), async {
            with_busy_retry("测试写入", || async {
                sqlx::query("INSERT INTO busy_probe (id) VALUES (2)")
                    .execute(&pool_b)
                    .await
            })
            .await
        })
        .await
        .expect("额度用尽应立即返回，不应挂死");

        let err = result.expect_err("持锁不释放时重试应最终失败");
        assert!(is_busy_error(&err), "最终错误应仍为写锁忙: {err}");

        txn.rollback().await.expect("回滚成功");
        drop((pool_a, pool_b));
        let _ = std::fs::remove_dir_all(db_path.parent().expect("临时路径应有父目录"));
    }

    /// 两个独立池并发交替写会话与消息：全部成功（多进程共库写场景模拟）。
    #[tokio::test]
    async fn concurrent_writes_from_two_pools_do_not_fail() {
        let db_path = temp_db_path("concurrent");
        let tuning = PoolTuning {
            busy_timeout: Duration::from_secs(2),
            max_connections: 2,
        };
        let pool_a = init_pool_with(Some(db_path.clone()), tuning)
            .await
            .expect("池 A 初始化成功");
        let pool_b = init_pool_with(Some(db_path.clone()), tuning)
            .await
            .expect("池 B 初始化成功");

        // 两池并发各写 20 条（10 组「建会话 + 写消息」交替）
        let (result_a, result_b) = tokio::join!(
            write_sessions_and_messages(&pool_a, "client-a"),
            write_sessions_and_messages(&pool_b, "client-b")
        );
        result_a.expect("池 A 的 20 条写入应全部成功");
        result_b.expect("池 B 的 20 条写入应全部成功");

        // 全部写入落库
        let session_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool_a)
            .await
            .expect("统计会话数成功");
        assert_eq!(session_count.0, 20, "两池各建 10 个会话都应落库");
        let message_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool_a)
            .await
            .expect("统计消息数成功");
        assert_eq!(message_count.0, 20, "两池各写 10 条消息都应落库");

        drop((pool_a, pool_b));
        let _ = std::fs::remove_dir_all(db_path.parent().expect("临时路径应有父目录"));
    }

    /// 单个池上的交替写入：10 组「建会话 + 写消息」共 20 次写操作。
    async fn write_sessions_and_messages(pool: &SqlitePool, tag: &str) -> RamariaResult<()> {
        for i in 0..10 {
            let external_ref = format!("{tag}-{i}");
            let session =
                sessions::create_in_channel(pool, None, "mcp", Some(&external_ref)).await?;
            let msg = Message::new(
                session.id,
                MessageRole::User,
                format!("{tag} 第 {i} 条"),
                MessageSource::Local,
            );
            messages::save(pool, &msg).await?;
        }
        Ok(())
    }
}
