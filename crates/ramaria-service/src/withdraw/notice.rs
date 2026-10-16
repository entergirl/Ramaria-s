//! crates/ramaria-service/src/withdraw/notice.rs - Ramaria 通道撤回通知处理
//!
//! 设计特点:
//! - 处理链：绑定门禁 → 会话定位 → 消息定位 → 幂等标记 → 触发资格（本机撤回 /
//!   助手消息不触发）→ 反应窗口 → 连撤在途去重 → 回应生成与投递
//! - 标记先于触发：无论是否触发回应，撤回标记都先落库（幂等重入以标记行数为准）
//! - 状态记账与调度同口径：人格状态与全局日计数在每个返回分支回写
//! - 隐私：日志只记会话 id / 人格 / 触发结论 / 原因，不记消息内容

use tracing::{info, warn};

use ramaria_core::error::RamariaResult;
use ramaria_core::privacy::mask_id;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, MessageRole, Session};

use crate::engine::Engine;
use crate::proactive::state::{local_date_str, local_hour};
use crate::proactive::{
    DeliveryOutcome, GateOutcome, JudgeCandidate, JudgeOutcome, JudgeSignals, ProactiveDirective,
    ProactiveGlobalState, ProactiveState, evaluate_recall_gates, judge_decide, load_global_state,
    load_state, proactive_deliver, record_proactive_delivery, save_global_state, save_state,
};
use crate::types::{RecallNoticeRequest, RecallOutcome};

use super::RECALL_REACTION_WINDOW_MS;

// =========================================================
// 用例入口
// =========================================================

/// 处理一条通道消息撤回通知。
///
/// 流程:
/// 1. 绑定门禁（不存在 / 未启用即忽略）；
/// 2. 会话定位（通道内活跃会话）；
/// 3. 消息定位（平台消息 ID）；
/// 4. 幂等标记（标记行数为 0 = 重复通知）；
/// 5. 触发资格（本机撤回 / 助手消息仅标记不触发）；
/// 6. 反应窗口（撤回过久不触发）；
/// 7. 连撤在途去重（同会话已有在途处理则不触发）；
/// 8. 触发主链（状态 / 闸门 / 判据 / 生成 / 投递）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `notice`: 撤回通知（通道层解析产物）。
/// - `now`: 处理时刻（Unix 毫秒）。
///
/// 返回:
/// - 处理结果三态（已标记 / 忽略 / 重复通知）。
pub(crate) async fn handle_notice(
    engine: &Engine,
    notice: RecallNoticeRequest,
    now: i64,
) -> RamariaResult<RecallOutcome> {
    let storage = engine.storage_ref().as_ref();

    // ---- 1. 绑定门禁 ----
    let binding = match engine
        .channel_find_binding(&notice.channel, &notice.external_ref)
        .await?
    {
        Some(binding) => binding,
        None => return Ok(ignored("binding_missing")),
    };
    if !binding.enabled {
        return Ok(ignored("binding_disabled"));
    }
    let persona_uid = engine.channel_effective_persona_uid(&binding);

    // ---- 2. 会话定位 ----
    let Some(session) = storage
        .find_active_session_by_channel(&notice.channel, Some(&notice.external_ref))
        .await?
    else {
        return Ok(ignored("session_missing"));
    };

    // ---- 3. 消息定位 ----
    let Some(message) = storage
        .find_message_by_platform_id(session.id, &notice.platform_message_id)
        .await?
    else {
        return Ok(ignored("message_missing"));
    };

    // ---- 4. 幂等标记 ----
    let rows = storage
        .mark_message_recalled(session.id, &notice.platform_message_id, now)
        .await?;
    if rows == 0 {
        return Ok(RecallOutcome::AlreadyMarked);
    }

    // ---- 5. 触发资格：本机撤回或助手消息不触发 ----
    if is_own_recall(&notice) || message.role == MessageRole::Assistant {
        info!(
            session_id = %session.id,
            channel = notice.channel,
            external_ref = %mask_id(&notice.external_ref),
            "通道撤回通知：本机撤回或助手消息，仅标记不触发"
        );
        return Ok(marked_false("own_message"));
    }

    // ---- 6. 反应窗口 ----
    if now.saturating_sub(notice.occurred_at) > RECALL_REACTION_WINDOW_MS {
        info!(
            session_id = %session.id,
            "通道撤回通知：超出反应窗口，仅标记不触发"
        );
        return Ok(marked_false("window_expired"));
    }

    // ---- 7. 连撤在途去重 ----
    let inflight_key = session.id.to_string();
    if !engine.recall_inflight_begin(&inflight_key) {
        info!(
            session_id = %session.id,
            "通道撤回通知：同会话已有在途处理，仅标记不触发"
        );
        return Ok(marked_false("inflight_dedup"));
    }

    // ---- 8. 触发主链（返回后释出在途标记）----
    let outcome = trigger_reaction(engine, &session, &message, &persona_uid, now).await;
    engine.recall_inflight_end(&inflight_key);

    if let RecallOutcome::Marked { triggered, reason } = &outcome {
        info!(
            session_id = %session.id,
            persona = %mask_id(&persona_uid),
            channel = notice.channel,
            external_ref = %mask_id(&notice.external_ref),
            triggered = *triggered,
            reason = reason.as_deref().unwrap_or(""),
            "通道撤回通知处理完成"
        );
    }
    Ok(outcome)
}

// =========================================================
// 触发主链
// =========================================================

/// 执行撤回国应生成主链（状态加载 → 闸门 → 判据 → 生成 → 投递）。
///
/// 记账口径:
/// - 每个返回分支在本函数内完成人格状态回写（全局状态启用时一并回写）；
/// - 判据 Speak 时记 `judge_yes_count` 但不写 `last_judge_at`
///   （即时事件豁免判据节流）；
/// - 投递结果驱动记账与调度同口径：Delivered / LocalFailed 计入日计数与全局日计数；
/// - 投递失败不改变结果（生成已完成）。
///
/// 返回:
/// - `Marked { triggered: true }`: 生成并投递完成；
/// - `Marked { triggered: false, reason }`: 链上任一环节未生成。
async fn trigger_reaction(
    engine: &Engine,
    session: &Session,
    message: &Message,
    persona_uid: &str,
    now: i64,
) -> RecallOutcome {
    let storage = engine.storage_ref().as_ref();
    let config = engine.config();

    // ---- a. 人格行 ----
    let persona = match storage.get_persona_by_uid(persona_uid).await {
        Ok(Some(persona)) => persona,
        Ok(None) => {
            warn!(persona = %mask_id(persona_uid), "撤回触发：人格不存在，放弃回应");
            return marked_false("persona_missing");
        }
        Err(error) => {
            warn!(
                persona = %mask_id(persona_uid),
                error = %error,
                "撤回触发：人格查询失败，放弃回应"
            );
            return marked_false("persona_missing");
        }
    };

    // ---- b. 运行时状态（人格 + 可选全局）----
    let mut st = match load_state(storage, persona_uid).await {
        Ok(st) => st,
        Err(error) => {
            warn!(error = %error, "撤回触发：状态读取失败，放弃回应");
            return marked_false("state_failed");
        }
    };
    let mut global = if config.proactive.daily_total_limit > 0 {
        match load_global_state(storage).await {
            Ok(mut global) => {
                let today = local_date_str(now);
                if global.daily_date != today {
                    global.daily_count = 0;
                    global.daily_date = today;
                }
                Some(global)
            }
            Err(error) => {
                warn!(error = %error, "撤回触发：全局状态读取失败，放弃回应");
                return marked_false("state_failed");
            }
        }
    } else {
        None
    };

    // ---- c. 打扰控制闸门 ----
    match evaluate_recall_gates(
        engine,
        &persona,
        now,
        &config.proactive,
        global.as_ref(),
        &mut st,
    )
    .await
    {
        Ok(GateOutcome::Skip(reason)) => {
            persist_state(storage, persona_uid, &st, global.as_ref()).await;
            return marked_false(reason.as_str());
        }
        Ok(GateOutcome::Pass) => {}
        Err(error) => {
            warn!(error = %error, "撤回触发：闸门判定失败，放弃回应");
            persist_state(storage, persona_uid, &st, global.as_ref()).await;
            return marked_false("gate_failed");
        }
    }

    // ---- d. AI 判据（即时事件：时机权重固定 1.0，不参与时机加权）----
    let hours_since_last_chat = match storage.last_message_time_by_persona(persona_uid).await {
        Ok(Some(t)) => Some(now.saturating_sub(t) as f64 / 3_600_000.0),
        Ok(None) => None,
        Err(error) => {
            warn!(error = %error, "撤回触发：最近对话时间读取失败，判据按无历史处理");
            None
        }
    };
    let signals = JudgeSignals {
        hour: local_hour(now),
        activity_weight: 1.0,
        hours_since_last_chat,
        silence_streak: st.silence_streak,
        daily_count: st.daily_count,
    };
    let candidates = [JudgeCandidate {
        id: "c0".to_string(),
        source: "recall".to_string(),
        anchor: Some("对方刚撤回了一条消息".to_string()),
        valence: 0.0,
    }];
    let decision = match judge_decide(engine, &candidates, &signals).await {
        JudgeOutcome::Speak(decision) => {
            st.judge_yes_count = st.judge_yes_count.saturating_add(1);
            decision
        }
        JudgeOutcome::Silent => {
            st.judge_no_count = st.judge_no_count.saturating_add(1);
            persist_state(storage, persona_uid, &st, global.as_ref()).await;
            return marked_false("judge_silent");
        }
        JudgeOutcome::Failed => {
            persist_state(storage, persona_uid, &st, global.as_ref()).await;
            return marked_false("judge_failed");
        }
    };

    // ---- e. 生成 ----
    let topic_key = format!("recall:{}", message.id);
    let directive = ProactiveDirective {
        persona: persona_uid.to_string(),
        session_id: Some(session.id),
        source: "recall".to_string(),
        topic_key: Some(topic_key.clone()),
        anchor: Some("对方刚撤回了一条消息".to_string()),
        angle: decision.angle,
        tone: decision.tone,
        valence: 0.0,
    };
    let outcome = match engine.chat_proactive(directive).await {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            persist_state(storage, persona_uid, &st, global.as_ref()).await;
            return marked_false("generation_skipped");
        }
        Err(error) => {
            warn!(
                persona = %mask_id(persona_uid),
                error = %error,
                "撤回触发：生成失败"
            );
            persist_state(storage, persona_uid, &st, global.as_ref()).await;
            return marked_false("generation_failed");
        }
    };

    // ---- f. 投递与记账（与调度同口径）----
    let delivery = proactive_deliver(engine, &outcome, now).await;
    record_proactive_delivery(
        &mut st,
        now,
        "recall",
        Some(topic_key.as_str()),
        &delivery,
        outcome.valence,
    );
    let billable = matches!(
        delivery,
        DeliveryOutcome::Delivered | DeliveryOutcome::LocalFailed
    );
    if billable {
        if let Some(global) = global.as_mut() {
            global.daily_count = global.daily_count.saturating_add(1);
        }
    }
    persist_state(storage, persona_uid, &st, global.as_ref()).await;

    info!(
        session_id = %session.id,
        persona = %mask_id(persona_uid),
        delivered = matches!(delivery, DeliveryOutcome::Delivered),
        billable,
        "撤回触发：已生成回应"
    );
    RecallOutcome::Marked {
        triggered: true,
        reason: None,
    }
}

// =========================================================
// 辅助
// =========================================================

/// 撤回者是否为本机（`self_id` 与 `operator_id` 均非空且相等）。
fn is_own_recall(notice: &RecallNoticeRequest) -> bool {
    match (notice.self_id.as_deref(), notice.operator_id.as_deref()) {
        (Some(self_id), Some(operator)) => {
            let self_id = self_id.trim();
            !self_id.is_empty() && self_id == operator.trim()
        }
        _ => false,
    }
}

/// 状态回写出口：人格状态与（启用时）全局状态一并保存；失败只记 warn。
async fn persist_state(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    st: &ProactiveState,
    global: Option<&ProactiveGlobalState>,
) {
    if let Err(error) = save_state(storage, persona_uid, st).await {
        warn!(
            persona = %mask_id(persona_uid),
            error = %error,
            "撤回触发：人格状态保存失败"
        );
    }
    if let Some(global) = global {
        if let Err(error) = save_global_state(storage, global).await {
            warn!(error = %error, "撤回触发：全局状态保存失败");
        }
    }
}

/// 未触发结果（带回原因标识）。
fn marked_false(reason: &str) -> RecallOutcome {
    RecallOutcome::Marked {
        triggered: false,
        reason: Some(reason.to_string()),
    }
}

/// 忽略结果（会话或消息未定位）。
fn ignored(reason: &str) -> RecallOutcome {
    RecallOutcome::Ignored {
        reason: reason.to_string(),
    }
}
