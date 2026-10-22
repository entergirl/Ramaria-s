//! crates/ramaria-service/src/proactive/schedule/round.rs - Ramaria 主动调度单轮
//!
//! 设计特点:
//! - 单轮判定链：硬闸门 → 活跃时段门与时机合成 → 判据节流 → 选题 → 生成 → 投放
//! - 全局日上限启用时整轮加载并回写全局状态；不限时不读不写
//! - 状态按人格隔离，处理尾部无条件回写；单人格失败不阻塞其余

use tracing::{debug, info, warn};

use ramaria_core::error::RamariaResult;
use ramaria_core::types::Persona;

use crate::engine::Engine;

use ramaria_core::config::ProactiveConfig;

use super::super::activity::{self, ActivityGate};
use super::super::state::{self, ProactiveGlobalState, ProactiveState};
use super::deliver::{deliver, record_delivery};
use super::gates::{GateOutcome, GateSkip, evaluate_gates, hours_to_ms};
use super::types::{DeliveryOutcome, PersonaTick, ProactiveTickSummary, TopicPicker};

// =========================================================
// 单轮调度
// =========================================================

/// 执行单轮主动调度检查。
///
/// 流程:
/// 1. 总开关判定（关闭直接返回空摘要，选题等一律不触达）；
/// 2. 列出全部人格（失败上抛，循环层下轮重试）；
/// 3. 全局日上限启用时加载全局状态并按本地日期跨日重置（失败上抛，下轮重试）；
/// 4. 逐人格：加载状态 → 判定链（闸门 / 活跃时段 / 判据节流 / 选题 / 生成 / 投放）
///    → 无条件回写状态；单人格失败记 warn 不阻塞其余；
/// 5. 全局状态无条件回写（跨日重置与生成计数均落盘）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `now`: 本轮时间（Unix 毫秒）。
/// - `picker`: 选题提供者。
///
/// 返回:
/// - 本轮各计数摘要；列出人格或读取全局状态失败时上抛。
pub(crate) async fn run_tick(
    engine: &Engine,
    now: i64,
    picker: &dyn TopicPicker,
) -> RamariaResult<ProactiveTickSummary> {
    let config = engine.config();
    if !config.proactive.enabled {
        debug!("主动调度：总开关关闭，本轮跳过");
        return Ok(ProactiveTickSummary::default());
    }

    let storage = engine.storage_ref().as_ref();
    let personas = storage.list_personas().await?;
    let mut summary = ProactiveTickSummary::default();

    // 全局日上限：不限时不读不写全局状态
    let mut global = if config.proactive.daily_total_limit > 0 {
        let mut g = state::load_global_state(storage).await?;
        let today = state::local_date_str(now);
        if g.daily_date != today {
            g.daily_count = 0;
            g.daily_date = today;
        }
        Some(g)
    } else {
        None
    };

    for persona in &personas {
        summary.personas += 1;
        let mut st = match state::load_state(storage, &persona.uid).await {
            Ok(st) => st,
            Err(e) => {
                warn!(persona = %persona.uid, error = %e, "主动调度：状态读取失败，跳过该人格");
                continue;
            }
        };

        let result = process_persona(
            engine,
            persona,
            now,
            picker,
            &config.proactive,
            global.as_ref(),
            &mut st,
        )
        .await;

        // 状态统一回写：跳过路径的跨日重置 / 宽限基准 / 退避归零等变更同样落盘
        if let Err(e) = state::save_state(storage, &persona.uid, &st).await {
            warn!(persona = %persona.uid, error = %e, "主动调度：状态保存失败");
        }

        match result {
            Ok(PersonaTick::Skipped(reason)) => {
                debug!(
                    persona = %persona.uid,
                    reason = reason.as_str(),
                    "主动调度：闸门未通过，跳过"
                );
            }
            Ok(PersonaTick::NoTopic) => {
                summary.attempts += 1;
                debug!(persona = %persona.uid, "主动调度：本轮无候选选题");
            }
            Ok(PersonaTick::Generated {
                delivered,
                billable,
            }) => {
                summary.attempts += 1;
                summary.generated += 1;
                if delivered {
                    summary.delivered += 1;
                }
                // 全局计数与人格级同口径：仅计费结果（本地失败仍计；丢弃 / 平台失败不计）
                if billable {
                    if let Some(g) = global.as_mut() {
                        g.daily_count = g.daily_count.saturating_add(1);
                    }
                }
            }
            Ok(PersonaTick::GenerationSkipped) => {
                summary.attempts += 1;
                debug!(persona = %persona.uid, "主动调度：生成层门禁跳过");
            }
            Ok(PersonaTick::GenerationFailed) => summary.attempts += 1,
            Err(e) => {
                warn!(
                    persona = %persona.uid,
                    error = %e,
                    "主动调度：单人格处理失败，跳过该人格"
                );
            }
        }
    }

    // 全局状态无条件回写：跨日重置与生成计数均落盘
    if let Some(g) = &global {
        if let Err(e) = state::save_global_state(storage, g).await {
            warn!(error = %e, "主动调度：全局状态保存失败");
        }
    }

    Ok(summary)
}

/// 处理单个人格的一轮调度。
///
/// 流程:
/// 1. 硬闸门（未通过返回跳过原因）；
/// 2. 活跃时段门（样本不足退化放行；低活跃度本轮不启动）；
/// 3. 时机权重合成（人格节律为软信号：样本不足中性，不构成硬否决）；
/// 4. 判据节流（仅判据开启时生效）；
/// 5. 选题（进入选题即视为一次判据尝试，先记账后调用）；
/// 6. 生成（`Ok(None)` = 生成层门禁静默跳过；`Err` 吸收为生成失败，不记投递状态）；
/// 7. 投放与记账（本地 / 通道分流；本地路径生成成功即记投递时间、当日计数与
///    候选效价符号，投递成功才记选题冷却；通道门禁丢弃 / 平台失败不记账）。
async fn process_persona(
    engine: &Engine,
    persona: &Persona,
    now: i64,
    picker: &dyn TopicPicker,
    config: &ProactiveConfig,
    global: Option<&ProactiveGlobalState>,
    st: &mut ProactiveState,
) -> RamariaResult<PersonaTick> {
    // ---- 1. 硬闸门 ----
    match evaluate_gates(engine, persona, now, config, global, st).await? {
        GateOutcome::Skip(reason) => return Ok(PersonaTick::Skipped(reason)),
        GateOutcome::Pass => {}
    }

    // ---- 2. 活跃时段门：软加权，样本不足退化放行 ----
    let hour = state::local_hour(now);
    let model = activity::load_model(
        engine.storage_ref().as_ref(),
        st,
        &persona.uid,
        now,
        config.active_hours_window_days,
        config.active_hours_min_samples,
    )
    .await?;
    let activity_weight =
        match activity::evaluate_gate(model.as_ref(), hour, config.active_hours_weight) {
            ActivityGate::LowWeight { norm } => {
                debug!(
                    persona = %persona.uid,
                    norm,
                    "主动调度跳过：当前时段活跃度过低"
                );
                return Ok(PersonaTick::Skipped(GateSkip::ActivityLow));
            }
            ActivityGate::Pass { weight } => {
                debug!(persona = %persona.uid, hour, weight, "主动调度：活跃时段门通过");
                weight
            }
            ActivityGate::NotModeled => {
                debug!(persona = %persona.uid, "主动调度：活跃时段样本不足，门放行");
                activity::weight_floor(config.active_hours_weight)
            }
        };

    // ---- 2.5 人格节律：与用户活跃合成时机权重（软信号，不构成硬否决）----
    let rhythm_model = activity::load_persona_model(
        engine.storage_ref().as_ref(),
        st,
        &persona.uid,
        now,
        config.rhythm_window_days,
        config.rhythm_min_samples,
    )
    .await?;
    let rhythm_norm = rhythm_model.map(|model| activity::hour_norm(&model.histogram, hour));
    let combined_weight =
        activity::combine_weight(activity_weight, rhythm_norm, config.rhythm_weight);
    debug!(
        persona = %persona.uid,
        rhythm_norm = ?rhythm_norm,
        combined_weight,
        "主动调度：时机权重合成"
    );

    // ---- 3. 判据节流（仅判据开启时生效）----
    if config.judge_enabled {
        if let Some(last) = st.last_judge_at {
            if now.saturating_sub(last) < hours_to_ms(config.judge_interval_hours) {
                return Ok(PersonaTick::Skipped(GateSkip::JudgeThrottled));
            }
        }
    }

    // ---- 4. 选题：进入选题即视为一次判据尝试，先记账后调用 ----
    st.last_judge_at = Some(now);
    let Some(directive) = picker
        .pick(engine, &persona.uid, now, st, combined_weight)
        .await
    else {
        return Ok(PersonaTick::NoTopic);
    };
    let source = directive.source.clone();
    let topic_key = directive.topic_key.clone();

    // ---- 5. 生成与投放 ----
    match engine.chat_proactive(directive).await {
        Ok(Some(outcome)) => {
            let delivery = deliver(engine, &outcome, now).await;
            record_delivery(
                st,
                now,
                &source,
                topic_key.as_deref(),
                &delivery,
                outcome.valence,
            );
            let delivered = matches!(delivery, DeliveryOutcome::Delivered);
            let billable = matches!(
                delivery,
                DeliveryOutcome::Delivered | DeliveryOutcome::LocalFailed
            );
            info!(
                persona = %persona.uid,
                source = %source,
                channel = %outcome.channel,
                delivered,
                billable,
                session_id = %outcome.session_id,
                "主动调度：已生成主动消息"
            );
            Ok(PersonaTick::Generated {
                delivered,
                billable,
            })
        }
        Ok(None) => {
            debug!(persona = %persona.uid, "主动调度：生成层门禁静默跳过");
            Ok(PersonaTick::GenerationSkipped)
        }
        // 生成失败（LLM / 存储）：不记投递状态，允许下一窗口重试（节流已记账）
        Err(e) => {
            warn!(persona = %persona.uid, error = %e, "主动调度：生成失败，本轮不记投递状态");
            Ok(PersonaTick::GenerationFailed)
        }
    }
}
