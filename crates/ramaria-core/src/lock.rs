//! crates/ramaria-core/src/lock.rs - 锁中毒统一恢复辅助
//!
//! 设计特点:
//! - 统一全仓锁中毒（持锁线程 panic 导致锁被标记为 poison）处理口径：
//!   记录 warn 日志后取回内部数据继续执行。
//! - 覆盖 `std::sync::Mutex`、`RwLock` 的读锁与写锁三类入口。
//! - 替代此前并存的三种分叉行为：静默 `into_inner()`、`error!` + `into_inner()`、
//!   `warn!` + 提前放弃；避免同一污染事件在不同调用点表现不一致。
//! - 纯同步工具，零 I/O，符合 ramaria-core 零 I/O 约束。
//!
//! 使用约定:
//! - 中毒意味着"此前有线程在持锁期间 panic"，锁内数据可能不完整；
//!   继续执行的代价是可能读到中间态数据，收益是不让一次 panic 永久中断主流程
//!   （对话、检索、索引维护均为可降级路径）。
//! - `context` 为调用点标识（如 `"app.llm"`），仅用于日志定位，
//!   不得传入任何原文、密钥或用户隐私内容。

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// 获取 Mutex 锁；锁中毒时记录 warn 日志并恢复内部数据继续。
///
/// 参数:
/// - `mutex`: 目标互斥锁。
/// - `context`: 调用点标识（日志字段，不含隐私内容）。
///
/// 返回:
/// - 可用的 [`MutexGuard`]；中毒时通过 `PoisonError::into_inner()` 取回。
pub fn lock_recover<'a, T>(mutex: &'a Mutex<T>, context: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                target: "ramaria_core::lock",
                context,
                "Mutex 锁中毒（持锁线程此前 panic），已恢复内部数据继续执行"
            );
            poisoned.into_inner()
        }
    }
}

/// 获取 RwLock 读锁；锁中毒时记录 warn 日志并恢复内部数据继续。
///
/// 参数:
/// - `lock`: 目标读写锁。
/// - `context`: 调用点标识（日志字段，不含隐私内容）。
///
/// 返回:
/// - 可用的 [`RwLockReadGuard`]；中毒时通过 `PoisonError::into_inner()` 取回。
pub fn read_recover<'a, T>(lock: &'a RwLock<T>, context: &str) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                target: "ramaria_core::lock",
                context,
                "RwLock 读锁中毒（持锁线程此前 panic），已恢复内部数据继续执行"
            );
            poisoned.into_inner()
        }
    }
}

/// 获取 RwLock 写锁；锁中毒时记录 warn 日志并恢复内部数据继续。
///
/// 参数:
/// - `lock`: 目标读写锁。
/// - `context`: 调用点标识（日志字段，不含隐私内容）。
///
/// 返回:
/// - 可用的 [`RwLockWriteGuard`]；中毒时通过 `PoisonError::into_inner()` 取回。
pub fn write_recover<'a, T>(lock: &'a RwLock<T>, context: &str) -> RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                target: "ramaria_core::lock",
                context,
                "RwLock 写锁中毒（持锁线程此前 panic），已恢复内部数据继续执行"
            );
            poisoned.into_inner()
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    /// 制造一个已被污染的 Mutex（持锁期间 panic）。
    fn poisoned_mutex() -> Mutex<i32> {
        let m = Mutex::new(7);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = m.lock().unwrap();
            panic!("intentional panic to poison mutex");
        }));
        assert!(result.is_err(), "闭包应 panic 并污染锁");
        m
    }

    /// 制造一个已被污染的 RwLock（持写锁期间 panic）。
    fn poisoned_rwlock() -> RwLock<i32> {
        let lock = RwLock::new(11);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = lock.write().unwrap();
            panic!("intentional panic to poison rwlock");
        }));
        assert!(result.is_err(), "闭包应 panic 并污染锁");
        lock
    }

    #[test]
    fn lock_recover_healthy_mutex() {
        let m = Mutex::new(1);
        let mut guard = lock_recover(&m, "test.healthy");
        *guard += 1;
        drop(guard);
        assert_eq!(*lock_recover(&m, "test.healthy"), 2);
    }

    #[test]
    fn lock_recover_poisoned_mutex_returns_data() {
        let m = poisoned_mutex();
        assert!(m.is_poisoned(), "前置条件：锁已中毒");
        let guard = lock_recover(&m, "test.poisoned");
        assert_eq!(*guard, 7, "中毒后仍应取回内部数据并继续");
    }

    #[test]
    fn read_recover_poisoned_rwlock_returns_data() {
        let lock = poisoned_rwlock();
        assert!(lock.is_poisoned(), "前置条件：锁已中毒");
        let guard = read_recover(&lock, "test.poisoned.read");
        assert_eq!(*guard, 11);
    }

    #[test]
    fn write_recover_poisoned_rwlock_can_write() {
        let lock = poisoned_rwlock();
        {
            let mut guard = write_recover(&lock, "test.poisoned.write");
            *guard = 13;
        }
        assert_eq!(*read_recover(&lock, "test.poisoned.read"), 13);
    }
}
