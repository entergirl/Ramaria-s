//! crates/ramaria-eval/src/run/control.rs - Ramaria 评测运行控制与进度事件
//!
//! 设计特点:
//! - 取消是单调开关：置位后不再派新题，已在飞的题照常完成并写出
//! - 事件只带题号、条件与计数：不含问题、回复与密钥
//! - 控制对象按值克隆：运行器、条件管线与写出点各持一份，共享同一开关与通道
//! - 订阅者消失时发送失败静默忽略：界面关闭不应中断运行
//! - 无订阅者等价于默认对象：CLI 调用不建通道，行为与未接入事件时一致

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc::UnboundedSender;

// =========================================================
// 事件
// =========================================================

/// 运行进度事件。
///
/// 字段约定:
/// - `Started` / `ConditionStarted` / `ItemDone` / `ConditionDone` 按运行时序发出；
/// - `Warning` 只对应条件级降级与跳过，与 `run.json` 的告警同源。
pub(crate) enum RunEvent {
    /// 运行开始：口径与总计划
    Started(RunStarted),
    /// 一个条件开始派题
    ConditionStarted {
        /// 条件在本次矩阵内的序号（从 0 起）
        index: usize,
        /// 本次运行的条件总数
        total: usize,
        /// 条件标识
        id: String,
        /// 本条件本次待跑题数（续跑后可能小于计划数）
        items: usize,
    },
    /// 一道题写出结果
    ItemDone(ItemEvent),
    /// 一个条件结束（含条件级失败）
    ConditionDone {
        /// 条件标识
        id: String,
        /// 该条件已完成题数
        completed: usize,
        /// 该条件已失败题数
        failed: usize,
    },
    /// 条件级降级与跳过提示
    Warning {
        /// 相关条件；与具体条件无关时为 `None`
        condition: Option<String>,
        /// 已写入 `run.json` 的提示原文
        message: String,
    },
}

/// 运行开始事件。
///
/// 字段约定:
/// - `total_items`: 计划题目数（题集去重后）；
/// - `total_executions`: 计划条目数（题 × 匹配条件）；
/// - `conditions`: 每个条件的计划题数与续跑初始计数，按矩阵展开顺序。
pub(crate) struct RunStarted {
    pub(crate) run_id: String,
    pub(crate) root: PathBuf,
    pub(crate) total_conditions: usize,
    pub(crate) total_items: usize,
    pub(crate) total_executions: usize,
    pub(crate) conditions: Vec<ConditionPlan>,
}

/// 一个条件的计划题数与续跑初始计数。
///
/// 字段约定:
/// - `total`: 该条件计划题数；
/// - `completed` / `failed`: 续跑时的初始计数（`items.jsonl` 扫描并 `recount` 之后）。
pub(crate) struct ConditionPlan {
    pub(crate) id: String,
    pub(crate) total: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
}

/// 单题写出事件。
///
/// 字段约定:
/// - `completed` / `failed`: 该条件**当前**的计数（同一题重试成功后失败数会回退）；
/// - `total`: 该条件的计划题数。
pub(crate) struct ItemEvent {
    pub(crate) condition_id: String,
    pub(crate) question_id: String,
    pub(crate) success: bool,
    pub(crate) elapsed_ms: u64,
    pub(crate) attempts: u32,
    pub(crate) error_kind: Option<String>,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) total: usize,
}

// =========================================================
// 控制
// =========================================================

/// 运行的取消开关与事件出口。
///
/// 状态:
/// - `cancelled`: 单调开关，置位后不再派新题；
/// - `events`: 可选事件通道；`None` 表示无人订阅（CLI 路径）。
#[derive(Clone, Default)]
pub(crate) struct RunControl {
    cancelled: Arc<AtomicBool>,
    events: Option<UnboundedSender<RunEvent>>,
}

impl RunControl {
    /// 带事件出口的控制对象。
    pub(crate) fn new(events: UnboundedSender<RunEvent>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            events: Some(events),
        }
    }

    /// 请求中断：之后不再派新题。
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// 是否已请求中断。
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 发送一条进度事件；无人订阅或接收端已关闭时静默忽略。
    pub(crate) fn emit(&self, event: RunEvent) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }
}
