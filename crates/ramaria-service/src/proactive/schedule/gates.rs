//! crates/ramaria-service/src/proactive/schedule/gates.rs - Ramaria 主动调度硬闸门判定
//!
//! 设计特点:
//! - 单次判定链：人格可见性 / 人格主动开关 / 应用状态 / 首次宽限 / 免打扰 /
//!   每日上限 / 全局日上限 / 冷却 / 退避 / 最小空闲 / 线上隐私确认，
//!   首个未通过即短路返回
//! - 按"资格 → 就绪 → 打扰控制 → 隐私"分层判定：资格类闸门先判，
//!   存储查询只在需要时发起
//! - 宽限基准与跨日重置在判定途中惰性写入运行时状态，由调用方统一回写
//! - 跳过只返回原因标识（日志字段），日志由调用方负责

use tracing::debug;

use ramaria_core::config::ProactiveConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{AppState, Persona};

use crate::engine::Engine;
use crate::proactive::switch::{self, ProactivePersonaMode};

use super::quiet::parse_quiet_hours;
use super::state::{self, ProactiveGlobalState, ProactiveState};

// =========================================================
// 跳过原因与判定结果
// =========================================================

/// 闸门跳过原因（日志标识）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateSkip {
    PersonaNotAllowed,
    PersonaExcluded,
    PersonaDisabled,
    PersonaCold,
    StateNotReady,
    StartupGrace,
    QuietHours,
    DailyLimit,
    DailyTotalLimit,
    Cooldown,
    SilenceBackoff,
    MinIdle,
    PrivacyUnconfirmed,
    ActivityLow,
    JudgeThrottled,
}

impl GateSkip {
    /// 稳定英文标识（日志字段）。
    pub(super) fn as_str(self) -> &'static str {
        match self {
            GateSkip::PersonaNotAllowed => "persona_not_allowed",
            GateSkip::PersonaExcluded => "persona_excluded",
            GateSkip::PersonaDisabled => "persona_disabled",
            GateSkip::PersonaCold => "persona_cold",
            GateSkip::StateNotReady => "state_not_ready",
            GateSkip::StartupGrace => "startup_grace",
            GateSkip::QuietHours => "quiet_hours",
            GateSkip::DailyLimit => "daily_limit",
            GateSkip::DailyTotalLimit => "daily_total_limit",
            GateSkip::Cooldown => "cooldown",
            GateSkip::SilenceBackoff => "silence_backoff",
            GateSkip::MinIdle => "min_idle",
            GateSkip::PrivacyUnconfirmed => "privacy_unconfirmed",
            GateSkip::ActivityLow => "activity_low",
            GateSkip::JudgeThrottled => "judge_throttled",
        }
    }
}

/// 硬闸门结果。
pub(super) enum GateOutcome {
    Pass,
    Skip(GateSkip),
}

// =========================================================
// 时间换算
// =========================================================

/// 小时数转毫秒。
pub(super) fn hours_to_ms(hours: u32) -> i64 {
    hours as i64 * 3_600_000
}

/// 天数转毫秒。
pub(super) fn days_to_ms(days: u32) -> i64 {
    days as i64 * 86_400_000
}

// =========================================================
// 硬闸门判定
// =========================================================

/// 硬闸门判定（按"资格 → 就绪 → 打扰控制 → 隐私"分层判定）。
///
/// 顺序:
/// 1. 人格可见性（召回策略白名单）；
/// 2. 人格主动开关（user 类硬排除 / 手动关 / 自动冷）；
/// 3. 应用状态就绪；
/// 4. 首次启用宽限期（首次见到即起算，宽限期内不打扰；grace=0 立即放行）；
/// 5. 免打扰时段；
/// 6. 每日上限（先按本地日期跨日重置）；
/// 7. 全局日上限（全部人格合计；不限时不传全局状态）；
/// 8. 冷却（距上次生成的最短间隔）；
/// 9. 退避与回应检测（用户已回应则解除退避）；
/// 10. 距上次对话的最小空闲（无历史视为足够空闲）；
/// 11. 线上 provider 隐私确认。
///
/// 返回:
/// - `Pass` 或首个未通过的跳过原因；存储查询失败上抛。
pub(super) async fn evaluate_gates(
    engine: &Engine,
    persona: &Persona,
    now: i64,
    config: &ProactiveConfig,
    global: Option<&ProactiveGlobalState>,
    st: &mut ProactiveState,
) -> RamariaResult<GateOutcome> {
    // ---- 1. 人格可见性 ----
    if !engine.recall_policy().persona_allowed(&persona.uid) {
        return Ok(GateOutcome::Skip(GateSkip::PersonaNotAllowed));
    }

    // ---- 2. 人格主动开关（user 硬排除 / 手动关 / 自动冷）----
    if switch::is_user_persona(persona) {
        return Ok(GateOutcome::Skip(GateSkip::PersonaExcluded));
    }
    match switch::load_mode(engine.storage_ref().as_ref(), &persona.uid).await? {
        ProactivePersonaMode::Off => return Ok(GateOutcome::Skip(GateSkip::PersonaDisabled)),
        ProactivePersonaMode::On => {} // 手动强开：无对话历史也放行（其余打扰控制照常）
        ProactivePersonaMode::Auto => {
            let has_dialogue = engine
                .storage_ref()
                .as_ref()
                .has_local_user_message_by_persona(&persona.uid)
                .await?;
            if !has_dialogue {
                return Ok(GateOutcome::Skip(GateSkip::PersonaCold));
            }
        }
    }

    // ---- 3. 应用状态 ----
    if engine.current_state() != AppState::Ready {
        return Ok(GateOutcome::Skip(GateSkip::StateNotReady));
    }

    // ---- 4. 首次启用宽限期（首次见到即起算）----
    let first = match st.first_seen_at {
        Some(t) => t,
        None => {
            st.first_seen_at = Some(now);
            now
        }
    };
    if now.saturating_sub(first) < days_to_ms(config.startup_grace_days) {
        return Ok(GateOutcome::Skip(GateSkip::StartupGrace));
    }

    // ---- 5. 免打扰时段（配置为空视为未配置；解析失败降级为无免打扰）----
    let quiet_raw = config.quiet_hours.trim();
    if !quiet_raw.is_empty() {
        match parse_quiet_hours(quiet_raw) {
            Some(quiet) => {
                if quiet.contains(state::local_minute_of_day(now)) {
                    return Ok(GateOutcome::Skip(GateSkip::QuietHours));
                }
            }
            None => {
                debug!(
                    raw = %quiet_raw,
                    "主动调度：免打扰时段配置无法解析，本轮按无免打扰处理"
                );
            }
        }
    }

    // ---- 6. 每日上限（先跨日重置）----
    let today = state::local_date_str(now);
    if st.daily_date != today {
        st.daily_count = 0;
        st.daily_date = today;
    }
    if st.daily_count >= config.daily_limit {
        return Ok(GateOutcome::Skip(GateSkip::DailyLimit));
    }

    // ---- 7. 全局日上限（全部人格合计；不限时调用方不传全局状态）----
    if config.daily_total_limit > 0 {
        if let Some(global) = global {
            if global.daily_count >= config.daily_total_limit {
                return Ok(GateOutcome::Skip(GateSkip::DailyTotalLimit));
            }
        }
    }

    // ---- 8. 冷却 ----
    if let Some(last) = st.last_sent_at {
        if now.saturating_sub(last) < hours_to_ms(config.cooldown_hours) {
            return Ok(GateOutcome::Skip(GateSkip::Cooldown));
        }
    }

    // ---- 9. 退避与回应检测 ----
    if let Some(last_sent) = st.last_sent_at {
        let last_user = engine
            .storage_ref()
            .as_ref()
            .last_user_message_time_by_persona(&persona.uid)
            .await?;
        let responded = last_user.is_some_and(|t| t > last_sent);
        if responded {
            // 用户已回应：解除退避
            st.silence_streak = 0;
        } else if now.saturating_sub(last_sent) >= days_to_ms(config.silence_backoff_days) {
            return Ok(GateOutcome::Skip(GateSkip::SilenceBackoff));
        }
    }

    // ---- 10. 距上次对话的最小空闲（无历史视为足够空闲）----
    if let Some(last) = engine
        .storage_ref()
        .as_ref()
        .last_message_time_by_persona(&persona.uid)
        .await?
    {
        if now.saturating_sub(last) < hours_to_ms(config.min_idle_hours) {
            return Ok(GateOutcome::Skip(GateSkip::MinIdle));
        }
    }

    // ---- 11. 隐私门禁（线上 provider 未确认时跳过）----
    if !crate::privacy::online_privacy_confirmed(engine).await? {
        return Ok(GateOutcome::Skip(GateSkip::PrivacyUnconfirmed));
    }

    Ok(GateOutcome::Pass)
}
