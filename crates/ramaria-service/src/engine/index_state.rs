//! crates/ramaria-service/src/engine/index_state.rs - Ramaria 索引状态机字段读写
//!
//! 设计特点:
//! - 脏标记语义：增量镜像时检索器尚未加载 → 置脏，下次加载（懒加载或重建）必然重建
//! - 代次快照记录"构建前"读取的库内快照：构建窗口内的新写入使下次比对不等 → 再刷新一次
//! - 失败告警位与失败原因记录分离：告警位供宿主快速判定，原因记录供诊断与提示
//! - 构建时间用于重建节流（`[index].refresh_interval_seconds` 冷却窗口），
//!   写入密集期抑制整库重建风暴
//! - 索引状态槽：构建路径写状态与进度，宿主读视图；进度回调在锁外派发
//! - 后台构建以 CAS 在途标记互斥，构建任务结束复位
//! - 纯内存原子 / 锁槽读写，不做 I/O 与日志，可高频调用

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::types::{IndexStatus, now_ms};

use crate::index::{IndexBuildFailure, IndexStamp};
use crate::types::{IndexProgress, IndexStatusView};

use super::Engine;

// =========================================================
// 索引脏标记（懒加载与增量镜像的协同）
// =========================================================

impl Engine {
    /// 标记索引需要重建（增量镜像时检索器尚未加载 → 该批 L1 未进内存索引）。
    pub(crate) fn mark_index_dirty(&self) {
        self.index_dirty.store(true, Ordering::Release);
    }

    /// 查询索引是否需要重建。
    pub(crate) fn index_dirty(&self) -> bool {
        self.index_dirty.load(Ordering::Acquire)
    }

    /// 清除索引脏标记（重建开始前调用：构建期间新产生的增量会重新置脏）。
    pub(crate) fn clear_index_dirty(&self) {
        self.index_dirty.store(false, Ordering::Release);
    }

    /// 检索索引最近一次重建是否失败（失败时共享检索器保留旧索引，仍可检索）。
    ///
    /// 用途:
    /// - 诊断展示与宿主告警；不改变任何降级行为（旧索引照常检索）。
    pub fn is_index_rebuild_failed(&self) -> bool {
        self.index_rebuild_failed.load(Ordering::Acquire)
    }

    /// 设置检索索引"重建失败"告警位（构建成功复位 / 失败置位，由索引构建路径调用）。
    pub(crate) fn set_index_rebuild_failed(&self, failed: bool) {
        self.index_rebuild_failed.store(failed, Ordering::Release);
    }

    /// 最近一次索引构建失败记录（脱敏原因 + 时间戳；未失败 / 已恢复为 `None`）。
    ///
    /// 用途:
    /// - 诊断导出携带可诊断原因；不改变任何降级行为（旧索引照常检索）。
    pub fn index_build_failure(&self) -> Option<IndexBuildFailure> {
        read_recover(&self.index_build_failure, "engine.index_build_failure").clone()
    }

    /// 记录索引构建失败原因（由索引构建路径在失败分支调用）。
    ///
    /// 参数:
    /// - `reason`: 脱敏后的原因文本（路径只留文件名、消息类字段只留字符数，不含用户原文）。
    pub(crate) fn record_index_build_failure(&self, reason: String) {
        let mut guard = write_recover(&self.index_build_failure, "engine.index_build_failure");
        *guard = Some(IndexBuildFailure {
            reason,
            at_ms: now_ms(),
        });
    }

    /// 清除索引构建失败记录（构建成功后复位）。
    pub(crate) fn clear_index_build_failure(&self) {
        let mut guard = write_recover(&self.index_build_failure, "engine.index_build_failure");
        *guard = None;
    }

    /// 记录索引代次快照（索引构建完成后调用）。
    ///
    /// 说明:
    /// - 记录的是**构建前**读取的库内快照：构建窗口内其他进程新写入的内容
    ///   会使下次比对不等 → 再刷新一次（收敛，不漏新记忆）。
    pub(crate) fn record_index_stamp(&self, stamp: IndexStamp) {
        let mut guard = write_recover(&self.index_stamp, "engine.index_stamp");
        *guard = Some(stamp);
    }

    /// 当前已记录的索引代次快照（索引未构建过时为 None）。
    pub(crate) fn index_stamp(&self) -> Option<IndexStamp> {
        *read_recover(&self.index_stamp, "engine.index_stamp")
    }

    /// 记录内存索引最近一次构建完成时间（索引构建成功后调用）。
    pub(crate) fn record_index_build_time(&self, at_ms: i64) {
        self.last_index_build_ms.store(at_ms, Ordering::Release);
    }

    /// 内存索引最近一次构建完成时间（Unix 毫秒；0 = 尚未构建）。
    pub(crate) fn last_index_build_time(&self) -> i64 {
        self.last_index_build_ms.load(Ordering::Acquire)
    }

    /// 判断当前是否允许重建内存索引（`[index].refresh_interval_seconds` 冷却窗口）。
    ///
    /// 语义:
    /// - 允许重建：间隔配置为 0（不节流）、从未构建过、或距上次构建已完成超过间隔；
    /// - 不允许：冷却窗口内——本次沿用现有索引，窗口过后的下一次召回补上重建
    ///   （用于写入密集期抑制整库重建风暴；不适用于首次加载与同进程脏标记路径）。
    pub(crate) fn index_rebuild_cooldown_elapsed(&self) -> bool {
        let interval_seconds = self.config().index.refresh_interval_seconds;
        if interval_seconds == 0 {
            return true;
        }
        let last_build_ms = self.last_index_build_time();
        if last_build_ms == 0 {
            return true;
        }
        now_ms().saturating_sub(last_build_ms) >= interval_seconds as i64 * 1_000
    }
}

// =========================================================
// 索引状态与进度（宿主展示 / 事件派发）
// =========================================================

/// 一次索引状态写入的完整快照（避免状态写入函数参数过多）。
struct IndexStatusUpdate {
    status: IndexStatus,
    loaded: usize,
    total: usize,
    failed: usize,
    model: Option<String>,
    last_error: Option<String>,
    elapsed_ms: u64,
}

impl Engine {
    /// 当前索引状态视图（计算百分比；读锁内克隆后返回）。
    pub fn index_status(&self) -> IndexStatusView {
        let state = read_recover(&self.index_status_state, "engine.index_status_state").clone();
        IndexStatusView {
            status: state.status,
            loaded: state.loaded,
            total: state.total,
            failed: state.failed,
            percent: crate::index::percent(state.status, state.loaded, state.total),
            model: state.model,
            last_error: state.last_error,
            updated_at: state.updated_at,
        }
    }

    /// 写入索引状态并派发一次进度事件（elapsed_ms 按 0 处理）。
    pub(crate) fn set_index_status(
        &self,
        status: IndexStatus,
        loaded: usize,
        total: usize,
        failed: usize,
        model: Option<String>,
        last_error: Option<String>,
    ) {
        self.apply_index_status(IndexStatusUpdate {
            status,
            loaded,
            total,
            failed,
            model,
            last_error,
            elapsed_ms: 0,
        });
    }

    /// 写入索引状态并派发带耗时的进度事件（构建期使用）。
    pub(crate) fn set_index_progress(
        &self,
        status: IndexStatus,
        loaded: usize,
        total: usize,
        failed: usize,
        model: Option<String>,
        elapsed_ms: u64,
    ) {
        self.apply_index_status(IndexStatusUpdate {
            status,
            loaded,
            total,
            failed,
            model,
            last_error: None,
            elapsed_ms,
        });
    }

    /// 状态槽写入 + 锁外派发进度事件。
    fn apply_index_status(&self, update: IndexStatusUpdate) {
        {
            let mut guard = write_recover(&self.index_status_state, "engine.index_status_state");
            guard.status = update.status;
            guard.loaded = update.loaded;
            guard.total = update.total;
            guard.failed = update.failed;
            guard.model = update.model.clone();
            guard.last_error = update.last_error;
            guard.updated_at = now_ms();
        }
        let hook = read_recover(&self.index_progress_hook, "engine.index_progress_hook").clone();
        if let Some(hook) = hook {
            hook(&IndexProgress {
                status: update.status,
                loaded: update.loaded,
                total: update.total,
                failed: update.failed,
                percent: crate::index::percent(update.status, update.loaded, update.total),
                elapsed_ms: update.elapsed_ms,
                model: update.model,
            });
        }
    }

    /// 清除最近一次构建失败原因（保留其余状态字段）。
    pub(crate) fn clear_index_status_error(&self) {
        let mut guard = write_recover(&self.index_status_state, "engine.index_status_state");
        guard.last_error = None;
    }

    /// 注册索引进度回调（宿主启动时注入；未注册时只更新状态）。
    pub fn set_index_progress_hook(&self, hook: super::IndexProgressHook) {
        let mut guard = write_recover(&self.index_progress_hook, "engine.index_progress_hook");
        *guard = Some(hook);
    }

    /// 当前是否有索引构建任务在途。
    pub(crate) fn is_index_build_inflight(&self) -> bool {
        self.index_build_inflight.load(Ordering::Acquire)
    }

    /// 索引构建串行锁（后台构建与同步重建共用）。
    pub(crate) fn index_build_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.index_build_lock
    }

    /// 启动后台索引构建（CAS 互斥）。
    ///
    /// 返回:
    /// - `true`: 已启动构建任务（在途期间状态由构建路径推进）。
    /// - `false`: 已有构建在途，本次调用被忽略。
    pub fn start_index_build(self: &Arc<Self>) -> bool {
        if self
            .index_build_inflight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::debug!("索引构建已在途，忽略本次触发");
            return false;
        }
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            // 在途标记由 guard 复位：构建任务 panic 时同样执行 Drop，
            // 避免标记永久为 true 导致后续构建全部被忽略。
            let _guard = IndexBuildInflightGuard(Arc::clone(&engine));
            let result = crate::index::rebuild(engine.as_ref()).await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "后台索引构建失败，保留旧索引");
            }
        });
        true
    }
}

/// 后台索引构建在途标记的复位守卫。
///
/// 说明:
/// - 正常结束与 panic 展开都会执行 `Drop`，保证在途标记不泄漏；
/// - panic 路径下状态仍停在 Building 时，改为 Degraded（保留当前计数与模型）
///   并置可读失败原因，避免宿主长期显示“构建中”。
struct IndexBuildInflightGuard(Arc<Engine>);

impl Drop for IndexBuildInflightGuard {
    fn drop(&mut self) {
        let engine = &self.0;
        engine.index_build_inflight.store(false, Ordering::Release);
        if engine.index_status().status == IndexStatus::Building {
            let snapshot = engine.index_status();
            engine.set_index_status(
                IndexStatus::Degraded,
                snapshot.loaded,
                snapshot.total,
                snapshot.failed,
                snapshot.model,
                Some("索引构建任务异常终止".to_string()),
            );
            tracing::warn!("索引构建任务异常终止，在途标记已复位");
        }
    }
}
