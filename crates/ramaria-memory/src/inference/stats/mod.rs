//! crates/ramaria-memory/src/inference/stats/mod.rs - Ramaria 统计特征提取模块（Phase A 数值计算）
//!
//! 设计特点:
//! - 模块原为单文件 stats.rs（约 3800 行），按自然职责拆分为若干子模块，对外引用路径保持不变。
//! - A1 三轨动态准入: confirmed/tentative/discarded 替代置信度硬截断（admission.rs）。
//! - 校准权重链: w_i = salience_cal × confidence_factor × situation_multiplier × source_support（weights.rs）。
//! - A3 按分类聚合 / E 动机二次分组: 校准加权均值/方差/有效样本量（category.rs）。
//! - A6 跨分类高阶指标: 情绪稳定性、叙事一致性、态度矛盾、share 偏度/峰度（cross.rs）。
//! - A7 代表性事件选取: 每分类取 salience 最高的代表性事件（representative.rs）。
//! - 主编排 run_phase_a_stats: 收口 A1/A3/A6/A7/E 为 StatsSummary（run.rs）。
//! - 通用加权统计原语 weighted.rs 供各子模块共享。
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时，所有输入由调用方传入。
//! - 可独立单元测试，无需 mock StorageBackend（tests.rs 经 #[cfg(test)] 收纳）。
//! - 向后兼容: prefilter_events 保留但委托给三轨分类；StatsConfig::default() 行为不变。
//!
//! 可见性说明:
//! - are_different_batches / normalize_group_weights 为 pub(super)，仅供根级测试套件直接调用，非外部 API。
//! - 子模块间通过 crate 内部路径取用共享符号，不扩大对外公开面。

mod admission;
mod category;
mod config;
mod cross;
mod representative;
mod run;
mod weighted;
mod weights;

#[cfg(test)]
mod tests;

// =========================================================
// 公共 re-export（对外路径 inference::stats::* 保持不变）
// =========================================================

pub use admission::{
    TentativePromotionConfig, TentativePromotionResult, classify_event, classify_events,
    prefilter_events, promote_tentative_events,
};
pub use category::{
    compute_category_stats, compute_motive_stats, extract_motive_tags, extract_primary_category,
    group_by_category, group_by_motive,
};
pub use config::{
    AdmissionTrack, CalibratedWeightConfig, CategoryStats, ClassifiedEvents, CrossCategoryMetrics,
    EventEnrichment, MotiveStats, RepresentativeEvent, StatsConfig, StatsSummary,
};
pub use cross::{
    compute_cross_category_metrics, compute_emotional_stability, compute_narrative_consistency,
    compute_share_kurtosis, compute_share_skewness,
};
pub use representative::select_representative_events;
pub use run::run_phase_a_stats;
pub use weighted::{weighted_mean, weighted_ratio, weighted_variance};
pub use weights::{
    calibrate_salience, compute_calibrated_weight, compute_calibrated_weights_batch,
    compute_simple_weight, compute_simple_weights_batch, situation_multiplier,
};
