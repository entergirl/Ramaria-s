//! crates/ramaria-service/src/proactive/schedule/types.rs - Ramaria 主动调度提供者与结果形态
//!
//! 设计特点:
//! - 选题提供者只依赖运行时状态与时机权重，不感知调度流程
//! - 结果形态区分单轮计数、投递四态与单人格处理结果，供记账差异化

use async_trait::async_trait;

use crate::engine::Engine;

use super::super::state::ProactiveState;
use super::super::topic::ProactiveDirective;
use super::gates::GateSkip;

// =========================================================
// 选题提供者
// =========================================================

/// 选题提供者：为指定人格产出候选指令（None = 本轮无题可提）。
///
/// 说明:
/// - `state` 为调度已加载的运行时状态（可变）：供选题器读取去重冷却记录、
///   写入判据 yes/no 计数；
/// - `activity_weight` 为时机权重（0.0~1.0：用户活跃与人格节律的合成软加权）：
///   供判据输入信号；样本不足未建模时取权重下限；
/// - 判据节流由调度层在调用前判定：本方法被调用即视为一次判据尝试（节流窗口消费）。
#[async_trait]
pub(crate) trait TopicPicker: Send + Sync {
    /// 产出本轮候选指令。
    ///
    /// 参数:
    /// - `engine`: 服务层引擎（供选题读取事件 / 规则等素材）。
    /// - `persona`: 目标人格 uid。
    /// - `now`: 本轮时间（Unix 毫秒）。
    /// - `state`: 调度已加载的运行时状态（可变；选题器写入判据计数）。
    /// - `activity_weight`: 时机权重（0.0~1.0；用户活跃与人格节律的合成）。
    ///
    /// 返回:
    /// - `Some(directive)`: 候选指令；
    /// - `None`: 本轮无题可提。
    async fn pick(
        &self,
        engine: &Engine,
        persona: &str,
        now: i64,
        state: &mut ProactiveState,
        activity_weight: f64,
    ) -> Option<ProactiveDirective>;
}

// =========================================================
// 单轮结果形态
// =========================================================

/// 单轮调度摘要（计数口径）。
///
/// 字段约定:
/// - `personas`: 本轮扫描的人格数；
/// - `attempts`: 进入选题（含判据尝试）的人格次数；
/// - `generated`: 生成成功条数；
/// - `delivered`: 实际投递条数。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProactiveTickSummary {
    pub personas: usize,
    pub attempts: usize,
    pub generated: usize,
    pub delivered: usize,
}

/// 投递结果（驱动差异化记账）。
pub(crate) enum DeliveryOutcome {
    /// 投递成功（本地或通道）。
    Delivered,
    /// 本地路径失败（未注册 / 本地 sink 报错）——维持既有记账口径。
    LocalFailed,
    /// 通道消息被门禁丢弃（总开关 / 绑定未启用 / 未允许主动 / 通道 sink 未装配 / 缺外部标识）。
    Dropped,
    /// 通道平台投递失败（sink 报错 / 绑定查询失败）。
    PlatformFailed,
}

/// 单人格处理结果。
pub(super) enum PersonaTick {
    /// 硬闸门或活跃时段门未通过。
    Skipped(GateSkip),
    /// 判据尝试完成但无候选选题。
    NoTopic,
    /// 生成成功（`delivered` = 投递是否成功；`billable` = 是否计入配额）。
    Generated { delivered: bool, billable: bool },
    /// 生成层门禁静默跳过（不落库不投递）。
    GenerationSkipped,
    /// 生成失败（已进入选题，不记投递状态）。
    GenerationFailed,
}
