//! crates/ramaria-service/src/proactive/schedule/deliver.rs - Ramaria 主动消息投放与记账
//!
//! 设计特点:
//! - 按负载通道路由：本地接收端与社交通道分流
//! - 通道消息先过四层门禁，任一不过即丢弃；平台投递失败不记账
//! - 本地路径生成成功即记投递时间与当日计数（消息已落库应用内可见）

use tracing::{debug, warn};

use ramaria_core::types::{CHANNEL_QQ, CHANNEL_TELEGRAM};

use crate::engine::Engine;

use super::super::sink::ProactiveMessage;
use super::super::state::{self, ProactiveState, RecentTopic};
use super::super::topic::ProactiveOutcome;
use super::types::DeliveryOutcome;
use super::{RECENT_TOPICS_CAP, VALENCE_SIGN_EPSILON};

/// 返回:
/// - 四态 [`DeliveryOutcome`]，驱动差异化记账。
pub(crate) async fn deliver(
    engine: &Engine,
    outcome: &ProactiveOutcome,
    now: i64,
) -> DeliveryOutcome {
    let is_channel = outcome.channel == CHANNEL_QQ || outcome.channel == CHANNEL_TELEGRAM;
    let message = ProactiveMessage {
        message_id: outcome.message_id,
        content: outcome.content.clone(),
        session_id: outcome.session_id,
        persona: outcome.persona.clone(),
        source: outcome.source.clone(),
        created_at: now,
        channel: outcome.channel.clone(),
        external_ref: outcome.external_ref.clone(),
    };

    if !is_channel {
        // 本地路径：维持既有口径（未注册静默丢弃；报错不记选题冷却）
        return match engine.proactive_sink() {
            None => {
                debug!(
                    persona = %outcome.persona,
                    "主动消息投递：接收端未注册，静默丢弃"
                );
                DeliveryOutcome::LocalFailed
            }
            Some(sink) => match sink.deliver(&message).await {
                Ok(()) => DeliveryOutcome::Delivered,
                Err(e) => {
                    warn!(
                        persona = %outcome.persona,
                        error = %e,
                        "主动消息投递失败"
                    );
                    DeliveryOutcome::LocalFailed
                }
            },
        };
    }

    // 通道路径：逐层门禁（任一不过 → 丢弃，零记账）
    if !engine.config().channel.proactive_delivery_enabled {
        debug!(channel = %outcome.channel, "主动消息通道投递：通道总开关关闭，丢弃");
        return DeliveryOutcome::Dropped;
    }
    let Some(external_ref) = message
        .external_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        debug!(channel = %outcome.channel, "主动消息通道投递：缺外部会话标识，丢弃");
        return DeliveryOutcome::Dropped;
    };
    match engine
        .channel_find_binding(&outcome.channel, &external_ref)
        .await
    {
        Ok(Some(binding)) => {
            if !binding.enabled || !binding.allow_proactive {
                debug!(
                    channel = %outcome.channel,
                    enabled = binding.enabled,
                    allow_proactive = binding.allow_proactive,
                    "主动消息通道投递：绑定未启用或未允许主动，丢弃"
                );
                return DeliveryOutcome::Dropped;
            }
        }
        Ok(None) => {
            debug!(channel = %outcome.channel, "主动消息通道投递：绑定不存在，丢弃");
            return DeliveryOutcome::Dropped;
        }
        Err(e) => {
            warn!(
                channel = %outcome.channel,
                error = %e,
                "主动消息通道投递：绑定查询失败（不记账）"
            );
            return DeliveryOutcome::PlatformFailed;
        }
    }
    let Some(sink) = engine.channel_proactive_sink() else {
        debug!(channel = %outcome.channel, "主动消息通道投递：通道接收端未装配，丢弃");
        return DeliveryOutcome::Dropped;
    };
    match sink.deliver(&message).await {
        Ok(()) => DeliveryOutcome::Delivered,
        Err(e) => {
            warn!(
                channel = %outcome.channel,
                error = %e,
                "主动消息通道投递失败（不记账）"
            );
            DeliveryOutcome::PlatformFailed
        }
    }
}

/// 效价符号：正（大于阈值）→ `1`；负（小于负阈值）→ `-1`；否则 `0`（中性或未知）。
fn valence_sign(valence: f64) -> i8 {
    if valence > VALENCE_SIGN_EPSILON {
        1
    } else if valence < -VALENCE_SIGN_EPSILON {
        -1
    } else {
        0
    }
}

/// 记录一次生成成功的投放记账。
///
/// 口径:
/// - `Delivered` / `LocalFailed`：记 `last_sent_at` / 当日计数 / 连续未回应次数 /
///   候选效价符号——消息已落库应用内可见（本地失败亦如此），避免下一窗口对同一
///   人格重复生成；退避计数以此累计；
/// - `Dropped` / `PlatformFailed`：全部不记（消息未送达不消耗额度，允许下一窗口重试）；
/// - 近期选题仅在投递成功且选题键非空时记录（投递失败不计冷却，允许下窗重试）。
pub(crate) fn record_delivery(
    st: &mut ProactiveState,
    now: i64,
    source: &str,
    topic_key: Option<&str>,
    delivery: &DeliveryOutcome,
    valence: f64,
) {
    // 丢弃 / 平台失败：不记任何状态（仅调用方日志留痕）
    if !matches!(
        delivery,
        DeliveryOutcome::Delivered | DeliveryOutcome::LocalFailed
    ) {
        debug!(source, "主动调度：投递未送达或被丢弃，本轮不记账");
        return;
    }

    st.last_sent_at = Some(now);
    st.daily_count = st.daily_count.saturating_add(1);
    st.daily_date = state::local_date_str(now);
    st.silence_streak = st.silence_streak.saturating_add(1);
    st.last_valence_sign = valence_sign(valence);

    if matches!(delivery, DeliveryOutcome::Delivered) {
        if let Some(key) = topic_key.map(str::trim).filter(|key| !key.is_empty()) {
            st.recent_topics.push(RecentTopic {
                source: source.to_string(),
                key: key.to_string(),
                sent_at: now,
            });
            if st.recent_topics.len() > RECENT_TOPICS_CAP {
                let overflow = st.recent_topics.len() - RECENT_TOPICS_CAP;
                st.recent_topics.drain(..overflow);
            }
        }
    }
}
