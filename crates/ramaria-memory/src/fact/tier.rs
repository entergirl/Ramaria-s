//! crates/ramaria-memory/src/fact/tier.rs - 知识层分层与时效衰减
//!
//! 设计特点:
//! - 依据 ProfileField 决定事实分层策略（stable / volatile / historical）
//! - stable: 基础信息/兴趣爱好/社交长期/说话风格——不轻易覆盖（需互证或 manual），不衰减
//! - volatile: 近期状态/近期背景——新覆盖旧保留版本链，随事件时间衰减
//! - historical: 历史事件——只追加不覆盖
//! - 衰减为幂等纯函数，仅作用于 volatile；稳定/历史事实权重恒定

use ramaria_core::types::{FactTier, ProfileField};

// =========================================================
// 分层策略
// =========================================================

/// 分层策略查询结构（无状态，纯函数集）。
pub struct FactTierPolicy;

impl FactTierPolicy {
    /// 依据 ProfileField 返回事实分层。
    ///
    /// 说明:
    /// - 稳定事实（基础信息/兴趣爱好/社交长期/说话风格）：长期成立，覆盖需强证据。
    /// - 动态事实（近期状态/近期背景）：时间敏感，新覆盖旧且随时间衰减。
    /// - 历史事实（历史事件）：客观发生过的过去事件，只追加不覆盖。
    ///
    /// 返回:
    /// - `FactTier` 分层。
    pub fn tier(field: ProfileField) -> FactTier {
        tier_for_field(field)
    }
}

/// 依据 ProfileField 返回事实分层（独立函数，供测试与上层直接调用）。
pub fn tier_for_field(field: ProfileField) -> FactTier {
    match field {
        // 稳定：长期成立的基础事实
        ProfileField::BasicInfo
        | ProfileField::Interests
        | ProfileField::Social
        | ProfileField::SpeakingStyle => FactTier::Stable,
        // 动态：近期状态 / 近期背景
        ProfileField::PersonalStatus | ProfileField::RecentContext => FactTier::Volatile,
        // 历史：客观发生过的过去事件
        ProfileField::History | _ => FactTier::Historical,
    }
}

/// 返回分层的描述文本（CLI/断言使用）。
pub fn describe_tier(tier: FactTier) -> &'static str {
    match tier {
        FactTier::Stable => "稳定（长期成立，覆盖需强证据）",
        FactTier::Volatile => "动态（近期状态/背景，随事件时间衰减）",
        FactTier::Historical | _ => "历史（客观发生过的过去事件，只追加）",
    }
}

// =========================================================
// 时效衰减
// =========================================================

/// 计算事实的时效权重（0.0..1.0）。
///
/// 参数:
/// - `tier`: 事实分层。
/// - `event_time`: 事实来源事件时间（Unix 毫秒）。
/// - `now`: 当前时间（Unix 毫秒）。
/// - `halflife_days`: volatile 半衰期天数（默认由调用方传入配置）。
///
/// 说明:
/// - volatile: 随事件时间指数衰减 `R = 2^(-days/halflife)`，表达"近期事实更相关"。
/// - stable / historical: 恒为 1.0（不作为时效因素限制）。
///
/// 返回:
/// - [0.0, 1.0] 的时效权重。
pub fn decay_weight(tier: FactTier, event_time: i64, now: i64, halflife_days: u32) -> f64 {
    match tier {
        FactTier::Stable | FactTier::Historical => 1.0,
        FactTier::Volatile => {
            let days = (now - event_time).max(0) as f64 / (1000.0 * 86400.0);
            let hl = halflife_days.max(1) as f64;
            (2.0f64).powf(-days / hl).clamp(0.0, 1.0)
        }
        _ => 1.0,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_maps_to_expected_tier() {
        assert_eq!(tier_for_field(ProfileField::BasicInfo), FactTier::Stable);
        assert_eq!(tier_for_field(ProfileField::Interests), FactTier::Stable);
        assert_eq!(tier_for_field(ProfileField::Social), FactTier::Stable);
        assert_eq!(
            tier_for_field(ProfileField::SpeakingStyle),
            FactTier::Stable
        );
        assert_eq!(
            tier_for_field(ProfileField::PersonalStatus),
            FactTier::Volatile
        );
        assert_eq!(
            tier_for_field(ProfileField::RecentContext),
            FactTier::Volatile
        );
        assert_eq!(tier_for_field(ProfileField::History), FactTier::Historical);
    }

    #[test]
    fn stable_and_historical_do_not_decay() {
        assert_eq!(decay_weight(FactTier::Stable, 0, 1_000_000, 30), 1.0);
        assert_eq!(decay_weight(FactTier::Historical, 0, 1_000_000, 30), 1.0);
    }

    #[test]
    fn volatile_decays_with_event_age() {
        let now = 1000 * 86400 * 100; // 任意 now
        // 刚发生 → 权重 1.0
        let fresh = decay_weight(FactTier::Volatile, now, now, 30);
        assert!((fresh - 1.0).abs() < 1e-9);
        // 一个半衰期前 → 权重 ~0.5
        let ago = decay_weight(FactTier::Volatile, now - 30 * 86400 * 1000, now, 30);
        assert!(
            (ago - 0.5).abs() < 1e-9,
            "30 天半衰期满应约 0.5，实际 {ago}"
        );
        // 更早 → 权重更低
        let older = decay_weight(FactTier::Volatile, now - 60 * 86400 * 1000, now, 30);
        assert!(older < ago);
    }

    #[test]
    fn volatile_clamps_to_unit_range() {
        // 未来时间（防御时钟偏移）→ clamp 到 1.0
        let now = 1000;
        let w = decay_weight(FactTier::Volatile, now + 5000, now, 30);
        assert!((0.0..=1.0).contains(&w));
        // halflife_days=0 → max(1) 兜底避免除零
        let w2 = decay_weight(FactTier::Volatile, now - 1000, now, 0);
        assert!((0.0..=1.0).contains(&w2));
    }
}
