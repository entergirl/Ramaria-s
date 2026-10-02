//! crates/ramaria-service/src/import/deep.rs - Ramaria 导入深度处理触发模块
//!
//! 设计特点:
//! - 触发序列与宿主事件序列一致：L2 阶段进度 → 全 persona L2 检查 → L3 阶段进度
//! - L2 / L3 预计总量在线估算（事件提取与性格推断均按 persona 批处理，导入双人上界 = 2）
//! - 级联是否真正产生由引擎内部阈值决定，本模块只负责阶段进度与触发
//! - 内部失败只记日志，不阻塞导入主流程

use std::time::Instant;

use ramaria_core::error::RamariaResult;

use crate::engine::Engine;
use crate::eta::{EtaEstimator, PhaseKind};

use super::l1::{ImportL1Progress, ImportProgressSink};

// =========================================================
// 用例入口
// =========================================================

/// 触发导入后的深度处理（L2 事件提取 → L3 性格画像级联）。
///
/// 流程（与宿主事件序列一致）:
/// 1. 更新 L2 阶段 EMA 并发送 `l2` 阶段进度（总量按双方 persona 在线估算 = 2）；
/// 2. 触发全 persona L2 检查（内部失败只记日志，不阻塞）；
/// 3. 回填 L2 进度、发送 `l3` 阶段进度（总量 = 2）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `l1_total`: L1 阶段的调用总数（来自 [`ImportL1Outcome`]；`None` 时进度事件不带 L1 总量）；
/// - `progress`: 可选进度回调（L2 / L3 各一条阶段进度）。
///
/// 说明:
/// - 由调用方在深度模式且至少一条 L1 生成成功时调用；
/// - 级联是否真正产生由引擎内部阈值决定，本用例只负责阶段进度与触发。
pub(crate) async fn trigger_deep(
    engine: &Engine,
    l1_total: Option<usize>,
    progress: Option<&dyn ImportProgressSink>,
) -> RamariaResult<()> {
    let started_at = Instant::now();
    let mut eta = EtaEstimator::new();
    // L2/L3 预计总量在线估算：事件提取与性格推断均按 persona 批处理，导入双人场景上界为 2
    let l2_expected = 2usize;
    let l3_expected = 2usize;

    // ---- L2 阶段开始 ----
    eta.update(
        PhaseKind::L2,
        0,
        l2_expected,
        started_at.elapsed().as_secs_f64(),
    );
    if let Some(sink) = progress {
        sink.on_l1_progress(&ImportL1Progress {
            phase: "l2",
            current: 0,
            total: l2_expected,
            message: "正在提取 L2 事件（双方 persona）...".to_string(),
            eta_seconds: eta.remaining_seconds().map(|s| s.round() as u64),
            l1_total,
            l2_total: Some(l2_expected),
            l3_total: None,
        });
    }

    engine.trigger_l2_check().await;

    // ---- L3 阶段开始（L2 已完成，回填 L2 样本） ----
    eta.update(
        PhaseKind::L2,
        l2_expected,
        l2_expected,
        started_at.elapsed().as_secs_f64(),
    );
    eta.update(
        PhaseKind::L3,
        0,
        l3_expected,
        started_at.elapsed().as_secs_f64(),
    );
    if let Some(sink) = progress {
        sink.on_l1_progress(&ImportL1Progress {
            phase: "l3",
            current: 0,
            total: l3_expected,
            message: "正在推断 L3 性格画像（双方 persona）...".to_string(),
            eta_seconds: eta.remaining_seconds().map(|s| s.round() as u64),
            l1_total,
            l2_total: Some(l2_expected),
            l3_total: Some(l3_expected),
        });
    }

    Ok(())
}
