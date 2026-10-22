//! crates/ramaria-service/src/lifecycle/l2_l3/all.rs - Ramaria 全量 L2/L3 手动触发
//!
//! 设计特点:
//! - 手动补跑口径：先做一轮 L2 触发检查，再对全部 persona 独立检查 L3（即使 L1 已全部吸收）
//! - 与后台调度共用同一份触发实现，本层只做遍历与日志
//! - 内部失败只记日志，不向调用方抛错（宿主手动触发不阻塞）

use crate::engine::Engine;

// =========================================================
// 全量触发
// =========================================================

/// 手动触发 L2 事件提取检查（全 persona 扫描 + 独立 L3 级联）。
///
/// 流程:
/// 1. `check_l2_trigger`：未吸收 L1 达阈值的 persona 进入事件提取（成功后内部级联 L3）；
/// 2. 再遍历全部 persona 独立检查 L3：覆盖「L1 已吸收但事件未吸收」的场景。
///
/// 参数:
/// - `engine`: 服务层引擎。
pub(crate) async fn check_all_personas(engine: &Engine) {
    let storage = engine.storage_ref().as_ref();

    tracing::info!("全量 L2/L3 检查：开始遍历 persona...");

    // L1 → L2（仅检查未吸收 L1）
    super::check_l2_trigger(engine, None).await;

    // L2 → L3（独立检查未吸收事件，即使 L1 已全部吸收）
    let personas = match storage.list_personas().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "全量 L2/L3 检查：查询 persona 列表失败，跳过 L3");
            return;
        }
    };

    for persona in &personas {
        let unabsorbed_events = match storage.list_unabsorbed_events(&persona.uid).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(persona_uid = %persona.uid, error = %e, "查询未吸收事件失败");
                continue;
            }
        };

        tracing::info!(
            persona_uid = %persona.uid,
            persona_name = %persona.name,
            unabsorbed_event_count = unabsorbed_events.len(),
            "检查 L3 触发条件"
        );

        super::check_l3_trigger(engine, None, &persona.uid).await;
    }
}
