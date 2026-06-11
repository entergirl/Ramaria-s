//! rust/crates/ramaria-memory/src/decay.rs - Ramaria 记忆衰减模块
//!
//! 设计特点:
//! - 实现 Ebbinghaus 遗忘曲线，R = e^(-t / S_adjusted)
//! - 支持 salience 对记忆稳定性的加成: S_adjusted = S * (1 + salience * MULTIPLIER)
//! - 支持近期访问加成 (Access Boost): 近期被检索命中的记录保留率不低于 BOOST_FLOOR
//! - 支持证据衰减权重: w = e^(-t / S)，用于性格推断中的时间衰减
//! - 参数配置化，各层 (L0/L1/L2) 可独立设置稳定性系数 S
//! - 纯数学模块，零 I/O，不依赖数据库或异步运行时

/// Ebbinghaus 衰减配置。
///
/// 职责:
/// - 集中管理衰减相关的全部参数。
/// - 支持各记忆层级 (L0/L1/L2) 使用不同的稳定性系数。
///
/// 字段约定:
/// - `stability_s`: 稳定性系数，越大衰减越慢。L0=10, L1=30, L2=60。
/// - `salience_multiplier`: salience 对稳定性的加成倍率，默认 0.5。
/// - `access_boost_enabled`: 是否启用近期访问加成，默认 true。
/// - `access_boost_days`: 近期访问判定窗口 (天)，默认 7。
/// - `access_boost_floor`: 近期访问的保留率保底值，默认 0.5。
#[derive(Debug, Clone)]
pub struct DecayConfig {
    /// 稳定性系数 S，越大代表衰减越慢
    pub stability_s: f64,
    /// salience 对稳定性的加成倍率 (默认 0.5)
    pub salience_multiplier: f64,
    /// 是否启用访问加成
    pub access_boost_enabled: bool,
    /// 近期访问判定窗口 (天)
    pub access_boost_days: u32,
    /// 近期访问保留率保底值
    pub access_boost_floor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            stability_s: 30.0, // 默认 L1 层参数
            salience_multiplier: 0.5,
            access_boost_enabled: true,
            access_boost_days: 7,
            access_boost_floor: 0.5,
        }
    }
}

impl DecayConfig {
    /// 为 L0 层创建配置 (原始消息切片，衰减最快)。
    pub fn l0() -> Self {
        Self {
            stability_s: 10.0,
            ..Default::default()
        }
    }

    /// 为 L1 层创建配置 (单次对话摘要，衰减适中)。
    pub fn l1() -> Self {
        Self {
            stability_s: 30.0,
            ..Default::default()
        }
    }

    /// 为 L2 层创建配置 (聚合时间段摘要/事件，衰减最慢)。
    pub fn l2() -> Self {
        Self {
            stability_s: 60.0,
            ..Default::default()
        }
    }
}

// =========================================================
// 公共函数
// =========================================================

/// 一天的毫秒数。
const MS_PER_DAY: f64 = 86_400_000.0;

/// 计算距今的天数差。
///
/// 参数:
/// - `created_at_ms`: 创建时间的 Unix 毫秒时间戳。
/// - `now_ms`: 当前时间的 Unix 毫秒时间戳。
///
/// 返回:
/// - 距今的天数 (f64)。若 `created_at_ms` 在 `now_ms` 之后 (未来时间) 则返回 0.0。
///
/// 说明:
/// - 使用 `f64` 精确保留小数天数，避免整数截断导致衰减跳跃。
fn days_since(created_at_ms: i64, now_ms: i64) -> f64 {
    let delta_ms = now_ms.saturating_sub(created_at_ms);
    if delta_ms <= 0 {
        return 0.0;
    }
    (delta_ms as f64) / MS_PER_DAY
}

/// 按给定稳定性 S 计算纯时间衰减权重 (不含 salience 和访问加成)。
///
/// 用法:
/// - 用于证据时间衰减，例如 personality trait 的证据累积计算。
/// - 公式: w = e^(-t / S)
///
/// 参数:
/// - `created_at_ms`: 记忆创建时间 (Unix ms)。
/// - `now_ms`: 当前时间 (Unix ms)。
/// - `stability_s`: 稳定性系数 S。
///
/// 返回:
/// - 衰减权重 w ∈ (0.0, 1.0]。越旧的记忆 w 越低。
pub fn calc_decay_weight(created_at_ms: i64, now_ms: i64, stability_s: f64) -> f64 {
    let t = days_since(created_at_ms, now_ms);
    if t <= 0.0 || stability_s <= 0.0 {
        return 1.0;
    }
    (-t / stability_s).exp()
}

/// 计算 Ebbinghaus 保留率 R。
///
/// 用法:
/// - 用于混合 RAG 检索时的记忆相关性调整。
/// - 公式: R = e^(-t / S_adjusted)
/// - S_adjusted = S * (1.0 + salience * multiplier)
///
/// 参数:
/// - `created_at_ms`: 记忆创建时间 (Unix ms)。
/// - `now_ms`: 当前时间 (Unix ms)。
/// - `salience`: 情感显著性 (0.0~1.0)，越高的记忆衰减越慢。超出范围将被钳制。
/// - `config`: 衰减配置。
///
/// 返回:
/// - 保留率 R ∈ (0.0, 1.0]，四舍五入到 4 位小数。
pub fn calc_decay_r(created_at_ms: i64, now_ms: i64, salience: f64, config: &DecayConfig) -> f64 {
    let t = days_since(created_at_ms, now_ms);
    if t <= 0.0 {
        return 1.0;
    }

    // 钳制 salience 到 [0.0, 1.0]
    let salience = salience.clamp(0.0, 1.0);

    // S_adjusted = S * (1.0 + salience * multiplier)
    // salience=0.0 → S_adjusted = S (无加成)
    // salience=0.5 → S_adjusted = 1.25 * S
    // salience=1.0 → S_adjusted = 1.5 * S
    let s_adjusted = config.stability_s * (1.0 + salience * config.salience_multiplier);

    let r = (-t / s_adjusted).exp();
    // 四舍五入到 4 位小数
    (r * 10000.0).round() / 10000.0
}

/// 应用访问加成后的最终保留率。
///
/// 用法:
/// - 在 `calc_decay_r` 的基础上，检查是否存在近期访问记录。
/// - 若 `last_accessed_at` 距今 ≤ `access_boost_days` 天，保底 R ≥ `access_boost_floor`。
///
/// 参数:
/// - `r`: 基础保留率 (来自 calc_decay_r)。
/// - `last_accessed_at_ms`: 最近访问时间 (Unix ms)，None 表示从未被访问。
/// - `now_ms`: 当前时间 (Unix ms)。
/// - `config`: 衰减配置。
///
/// 返回:
/// - 应用访问加成后的最终保留率。
pub fn apply_access_boost(
    r: f64,
    last_accessed_at_ms: Option<i64>,
    now_ms: i64,
    config: &DecayConfig,
) -> f64 {
    if !config.access_boost_enabled {
        return r;
    }
    let last_accessed = match last_accessed_at_ms {
        Some(ts) => ts,
        None => return r,
    };
    let days_since_access = days_since(last_accessed, now_ms);
    if days_since_access <= config.access_boost_days as f64 {
        r.max(config.access_boost_floor)
    } else {
        r
    }
}

/// 完整计算记忆保留率 (含 salience 加成和访问加成)。
///
/// 参数:
/// - `created_at_ms`: 创建时间 (Unix ms)。
/// - `last_accessed_at_ms`: 最近访问时间 (Unix ms)，可选。
/// - `now_ms`: 当前时间 (Unix ms)。
/// - `salience`: 显著性 (0.0~1.0)。
/// - `config`: 衰减配置。
///
/// 返回:
/// - 最终保留率 R ∈ (0.0, 1.0]。
pub fn calc_retention(
    created_at_ms: i64,
    last_accessed_at_ms: Option<i64>,
    now_ms: i64,
    salience: f64,
    config: &DecayConfig,
) -> f64 {
    let r = calc_decay_r(created_at_ms, now_ms, salience, config);
    apply_access_boost(r, last_accessed_at_ms, now_ms, config)
}

/// 距离调整时保留率的最小保底值，防止除以零或极小 R 导致数值异常。
const DISTANCE_ADJUST_MIN_R: f64 = 0.1;

/// 用保留率 R 调整检索距离。
///
/// 用法:
/// - 在向量检索中，使用 R 调整语义距离: adjusted_distance = distance / max(R, MIN_R)
/// - R 越小 (记忆越久) → adjusted_distance 越大 → 越不相关
/// - 最小保底 R 防止除以零或极小的 R 导致数值异常
///
/// 参数:
/// - `distance`: 原始语义距离 (如余弦距离)。
/// - `r`: 保留率。
///
/// 返回:
/// - 调整后的距离。
pub fn adjust_distance(distance: f64, r: f64) -> f64 {
    distance / r.max(DISTANCE_ADJUST_MIN_R)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 精确控制时间戳的辅助函数：返回从 now 往前 n 天的毫秒时间戳。
    fn days_ago_ms(now_ms: i64, days: i64) -> i64 {
        now_ms - days * 86_400_000
    }

    // --- days_since ---

    #[test]
    fn days_since_exactly_one_day() {
        let now = 1_000_000_000_000;
        let created = now - 86_400_000; // 恰好 1 天前
        let days = days_since(created, now);
        assert!((days - 1.0).abs() < 0.001);
    }

    #[test]
    fn days_since_future_returns_zero() {
        let now = 1_000_000_000_000;
        let created = now + 86_400_000; // 未来
        assert_eq!(days_since(created, now), 0.0);
    }

    #[test]
    fn days_since_same_time_returns_zero() {
        let now = 1_000_000_000_000;
        assert_eq!(days_since(now, now), 0.0);
    }

    // --- calc_decay_weight ---

    #[test]
    fn decay_weight_at_t0_is_one() {
        let now = 1_000_000_000_000;
        let w = calc_decay_weight(now, now, 30.0);
        assert!((w - 1.0).abs() < 0.001);
    }

    #[test]
    fn decay_weight_decreases_with_time() {
        let now = 1_000_000_000_000;
        let w1 = calc_decay_weight(days_ago_ms(now, 30), now, 30.0);
        let w2 = calc_decay_weight(days_ago_ms(now, 60), now, 30.0);
        assert!(w1 > w2, "older memory should have lower weight");
    }

    #[test]
    fn decay_weight_with_invalid_s() {
        let now = 1_000_000_000_000;
        // stability_s <= 0 → 返回 1.0
        assert_eq!(calc_decay_weight(days_ago_ms(now, 10), now, 0.0), 1.0);
        assert_eq!(calc_decay_weight(days_ago_ms(now, 10), now, -1.0), 1.0);
    }

    // --- calc_decay_r ---

    #[test]
    fn decay_r_at_t0_is_one() {
        let now = 1_000_000_000_000;
        let config = DecayConfig::l1();
        let r = calc_decay_r(now, now, 0.5, &config);
        assert!((r - 1.0).abs() < 0.001);
    }

    #[test]
    fn decay_r_salience_affects_rate() {
        let now = 1_000_000_000_000;
        let created = days_ago_ms(now, 30);
        let config = DecayConfig::l0(); // S=10

        let r_low = calc_decay_r(created, now, 0.0, &config);
        let r_mid = calc_decay_r(created, now, 0.5, &config);
        let r_high = calc_decay_r(created, now, 1.0, &config);

        // 高 salience 的 R 应更大 (衰减更慢)
        assert!(r_high > r_mid, "high salience should decay slower");
        assert!(r_mid > r_low, "mid salience should decay slower than none");

        // 验证公式: S_adjusted = 10 * (1 + 0.5*0.5) = 10 * 1.25 = 12.5
        // t=30, r_mid = e^(-30/12.5) ≈ e^(-2.4) ≈ 0.0907
        let expected = (-30.0_f64 / 12.5).exp();
        assert!(
            (r_mid - expected).abs() < 0.001,
            "expected {:.6}, got {:.6}",
            expected,
            r_mid
        );
    }

    #[test]
    fn decay_r_salience_clamped() {
        let now = 1_000_000_000_000;
        let created = days_ago_ms(now, 10);
        let config = DecayConfig::l1();

        // salience 超出范围应被钳制
        let r_neg = calc_decay_r(created, now, -0.5, &config);
        let r_zero = calc_decay_r(created, now, 0.0, &config);
        assert!((r_neg - r_zero).abs() < 0.0001);

        let r_over = calc_decay_r(created, now, 1.5, &config);
        let r_one = calc_decay_r(created, now, 1.0, &config);
        assert!((r_over - r_one).abs() < 0.0001);
    }

    #[test]
    fn decay_r_layers_have_different_rates() {
        let now = 1_000_000_000_000;
        let created = days_ago_ms(now, 30);

        let config_l0 = DecayConfig::l0(); // S=10
        let config_l1 = DecayConfig::l1(); // S=30
        let config_l2 = DecayConfig::l2(); // S=60

        let r0 = calc_decay_r(created, now, 0.5, &config_l0);
        let r1 = calc_decay_r(created, now, 0.5, &config_l1);
        let r2 = calc_decay_r(created, now, 0.5, &config_l2);

        // L0 衰减最快，L2 衰减最慢
        assert!(r2 > r1, "L2 should decay slower than L1");
        assert!(r1 > r0, "L1 should decay slower than L0");
    }

    #[test]
    fn decay_r_approaches_zero_for_very_old() {
        let now = 1_000_000_000_000;
        let created = days_ago_ms(now, 3650); // 10 年
        let config = DecayConfig::l0();
        let r = calc_decay_r(created, now, 0.0, &config);
        assert!(r < 0.001, "10 year old L0 memory should approach zero");
    }

    // --- apply_access_boost ---

    #[test]
    fn access_boost_when_recently_accessed() {
        let now = 1_000_000_000_000;
        let config = DecayConfig::default(); // boost_days=7, floor=0.5
        let r = 0.2; // 低保留率
        let last_accessed = Some(days_ago_ms(now, 3)); // 3 天前，在窗口内
        let boosted = apply_access_boost(r, last_accessed, now, &config);
        assert!(
            (boosted - 0.5).abs() < 0.001,
            "should boost to floor 0.5, got {}",
            boosted
        );
    }

    #[test]
    fn access_boost_no_boost_when_outside_window() {
        let now = 1_000_000_000_000;
        let config = DecayConfig::default();
        let r = 0.2;
        let last_accessed = Some(days_ago_ms(now, 10)); // 10 天前，超出窗口
        let result = apply_access_boost(r, last_accessed, now, &config);
        assert!(
            (result - 0.2).abs() < 0.001,
            "should not boost when outside window"
        );
    }

    #[test]
    fn access_boost_no_last_accessed() {
        let now = 1_000_000_000_000;
        let config = DecayConfig::default();
        let r = 0.2;
        let result = apply_access_boost(r, None, now, &config);
        assert!((result - 0.2).abs() < 0.001);
    }

    #[test]
    fn access_boost_disabled() {
        let now = 1_000_000_000_000;
        let config = DecayConfig {
            access_boost_enabled: false,
            ..Default::default()
        };
        let r = 0.2;
        let last_accessed = Some(days_ago_ms(now, 1));
        let result = apply_access_boost(r, last_accessed, now, &config);
        assert!((result - 0.2).abs() < 0.001);
    }

    #[test]
    fn access_boost_no_override_when_r_already_high() {
        let now = 1_000_000_000_000;
        let config = DecayConfig::default();
        let r = 0.8; // 已经高于 floor
        let last_accessed = Some(days_ago_ms(now, 1));
        let result = apply_access_boost(r, last_accessed, now, &config);
        assert!((result - 0.8).abs() < 0.001);
    }

    // --- calc_retention ---

    #[test]
    fn calc_retention_full_pipeline() {
        let now = 1_000_000_000_000;
        let config = DecayConfig::l1(); // S=30
        let created = days_ago_ms(now, 45); // 45 天前
        let accessed = Some(days_ago_ms(now, 2)); // 2 天前访问

        // 基础 R: t=45, S_adjusted=30*1.25=37.5, R=e^(-45/37.5)≈e^(-1.2)≈0.3012
        let retention = calc_retention(created, accessed, now, 0.5, &config);
        // 访问加成应提升到至少 0.5
        assert!(retention >= 0.5, "should be boosted, got {}", retention);
        assert!(
            (retention - 0.5).abs() < 0.001,
            "should be exactly floor 0.5, got {}",
            retention
        );
    }

    // --- adjust_distance ---

    #[test]
    fn adjust_distance_increases_when_r_small() {
        let dist = 0.3;
        // R=0.5 → adjusted = 0.3/0.5 = 0.6
        let adj = adjust_distance(dist, 0.5);
        assert!((adj - 0.6).abs() < 0.001);
    }

    #[test]
    fn adjust_distance_with_very_small_r() {
        let dist = 0.3;
        // R=0.01 → max(0.01, 0.1)=0.1 → adjusted = 0.3/0.1 = 3.0
        let adj = adjust_distance(dist, 0.01);
        assert!((adj - 3.0).abs() < 0.001);
    }

    #[test]
    fn adjust_distance_r_is_one_unchanged() {
        let dist = 0.3;
        let adj = adjust_distance(dist, 1.0);
        assert!((adj - 0.3).abs() < 0.001);
    }

    // --- 配置创建器 ---

    #[test]
    fn config_l0_has_stability_10() {
        let cfg = DecayConfig::l0();
        assert_eq!(cfg.stability_s, 10.0);
    }

    #[test]
    fn config_l1_has_stability_30() {
        let cfg = DecayConfig::l1();
        assert_eq!(cfg.stability_s, 30.0);
    }

    #[test]
    fn config_l2_has_stability_60() {
        let cfg = DecayConfig::l2();
        assert_eq!(cfg.stability_s, 60.0);
    }

    #[test]
    fn config_default_has_sensible_values() {
        let cfg = DecayConfig::default();
        assert!(cfg.stability_s > 0.0);
        assert!(cfg.salience_multiplier > 0.0);
        assert!(cfg.access_boost_enabled);
        assert!(cfg.access_boost_days > 0);
        assert!(cfg.access_boost_floor > 0.0 && cfg.access_boost_floor < 1.0);
    }
}
