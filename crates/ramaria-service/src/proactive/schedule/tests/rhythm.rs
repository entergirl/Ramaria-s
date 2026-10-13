//! crates/ramaria-service/src/proactive/schedule/tests/rhythm.rs - 人格节律缓存与时机权重合成
//!
//! 设计特点:
//! - 由 父测试模块 以 mod rhythm; 收纳，经 use super::* 取用父模块辅助桩与装配。
//! - 覆盖：单轮调度写入人格节律直方图缓存、时机权重合成实参（用户活跃 × 人格节律）。
//! - 用固定时间戳与预置缓存驱动单轮调度，不依赖网络与真实时钟。
//! - 断言口径：库中 reload 的运行时状态、选题器收到的时机权重实参。

use super::*;

/// 记录时机权重实参的选题器（不产出候选）。
struct RecordingPicker {
    weight: Mutex<Option<f64>>,
}

impl RecordingPicker {
    /// 尚未收到调用时权重为 None。
    fn new() -> Self {
        Self {
            weight: Mutex::new(None),
        }
    }

    /// 最近一次收到的时机权重。
    fn weight(&self) -> Option<f64> {
        *self.weight.lock().expect("记录锁不应中毒")
    }
}

#[async_trait]
impl TopicPicker for RecordingPicker {
    async fn pick(
        &self,
        _engine: &Engine,
        _persona: &str,
        _now: i64,
        _state: &mut ProactiveState,
        activity_weight: f64,
    ) -> Option<ProactiveDirective> {
        *self.weight.lock().expect("记录锁不应中毒") = Some(activity_weight);
        None
    }
}

// =========================================================
// 人格节律缓存
// =========================================================

/// 单轮调度按窗口统计人格节律并写入缓存（缓存归属日期为今日）。
#[tokio::test]
async fn rhythm_histogram_cached_after_tick() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.rhythm_min_samples = 1;
    let (engine, storage, dir) = ready_engine("proactive-rhythm-cache", config).await;

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(summary.attempts, 1, "选题链应正常完成");

    let reloaded = reload_state(&storage).await;
    assert!(
        reloaded.rhythm_histogram.is_some(),
        "单轮应统计并写入人格节律直方图"
    );
    assert_eq!(
        reloaded.rhythm_histogram_date,
        state::local_date_str(now),
        "缓存应记录归属日期"
    );
    assert!(
        reloaded.wake_minute_median.is_none(),
        "历史消息在用户活跃统计窗口外，唤醒中位不可用"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 时机权重合成
// =========================================================

/// 以指定节律强度运行合成场景，返回选题器收到的时机权重。
///
/// 场景:
/// - 用户模型缓存：当前小时 50 条、峰值 100 条 → 归一化 0.5，门通过，用户权重 0.75；
/// - 人格模型缓存：当前小时零计数、峰值 4 条 → 归一化 0.0。
async fn run_combined_case(tag: &str, rhythm_weight: f64) -> f64 {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.active_hours_weight = 0.5;
    config.proactive.active_hours_min_samples = 1;
    config.proactive.rhythm_min_samples = 1;
    config.proactive.rhythm_weight = rhythm_weight;
    let (engine, storage, dir) = ready_engine(tag, config).await;

    let hour = state::local_hour(now) as usize;
    let peak_hour = (hour + 1) % 24;
    let today = state::local_date_str(now);

    let mut user_histogram = [0u32; 24];
    user_histogram[hour] = 50;
    user_histogram[peak_hour] = 100;
    let mut rhythm_histogram = [0u32; 24];
    rhythm_histogram[peak_hour] = 4;

    let st = ProactiveState {
        hour_histogram: Some(user_histogram),
        histogram_date: today.clone(),
        rhythm_histogram: Some(rhythm_histogram),
        rhythm_histogram_date: today,
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;

    let picker = RecordingPicker::new();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    let weight = picker.weight().expect("选题器应收到时机权重");
    let _ = std::fs::remove_dir_all(dir);
    weight
}

/// 合成生效：零节律强度（norm = 0）按配置强度衰减到下限。
#[tokio::test]
async fn combined_weight_applies_rhythm_attenuation() {
    // combine_weight(0.75, Some(0.0), 0.5) = 0.75 × (0.5 + 0.5 × 0) = 0.375
    let weight = run_combined_case("proactive-rhythm-combined", 0.5).await;
    assert!((weight - 0.375).abs() < 1e-9, "期望 0.375，实际 {weight}");
}

/// 强度 0 退化：合成权重等于用户活跃权重（行为与仅用户活跃一致）。
#[tokio::test]
async fn combined_weight_degenerates_when_rhythm_off() {
    let weight = run_combined_case("proactive-rhythm-off", 0.0).await;
    assert!((weight - 0.75).abs() < 1e-9, "期望 0.75，实际 {weight}");
}
