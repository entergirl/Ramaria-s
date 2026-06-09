//! rust/crates/ramaria-memory/src/inference/mod.rs - L2→L3 性格推断管线模块
//!
//! 设计特点:
//! - Phase A (纯数值): stats.rs 统计特征、clustering.rs 态度聚类、shrink.rs 贝叶斯收缩
//! - Phase B (LLM 推断): inferrer.rs 三步结构化推断 + mock + 后处理
//! - Phase C (增量更新): drift.rs Wasserstein 漂移检测、confidence.rs 证据累计置信度
//! - 全量校准: calibration.rs 累积 10 轮触发 + 全量差异对比
//! - 超参数锁定: HDBSCAN min_cluster_size=3, UMAP n_components=12, 置换检验 B=1000

pub mod calibration;
pub mod clustering;
pub mod confidence;
pub mod drift;
pub mod inferrer;
pub mod shrink;
pub mod stats;

// =========================================================
// 公共 re-export
// =========================================================

pub use calibration::{
    CalibrationConfig, CalibrationDiff, CalibrationTracker, compute_calibration_diff,
};
pub use clustering::{
    AttitudeSample, ClusterAssignment, ClusterDescription, ClusteringConfig, ClusteringResult,
    cosine_similarity, run_clustering, simple_density_cluster,
};
pub use confidence::{
    ConfidenceConfig, ConfidenceSummary, TraitConfidenceUpdate, compute_confidence,
    compute_consistency, compute_e_delta, compute_e_total, merge_consistency,
    run_confidence_update, time_decay_weight, update_trait_confidence,
};
pub use drift::{
    CategoryDriftResult, CategoryEventData, DimensionDriftResult, DriftConfig, DriftSummary,
    detect_category_drift, detect_dimension_drift, permutation_test, run_drift_detection,
    wasserstein_1d,
};
pub use inferrer::{
    CategorySignal, ConsistencyAnalysis, DiffAction, InferenceResult, InferredTrait,
    InferrerConfig, PostProcessResult, TraitDiff, build_step1_prompt, build_step2_prompt,
    build_step3_prompt, compute_trait_diff, mock_infer, post_process_inference,
};
pub use shrink::{
    ShrinkConfig, compute_dynamic_gamma, compute_global_stats, logit, run_shrinkage,
    shrink_category, shrink_presentation, shrink_share, shrink_valence, sigmoid,
};
pub use stats::{
    CategoryStats, CrossCategoryMetrics, RepresentativeEvent, StatsConfig, StatsSummary,
    compute_category_stats, compute_cross_category_metrics, compute_emotional_stability,
    compute_narrative_consistency, compute_share_kurtosis, compute_share_skewness,
    extract_primary_category, group_by_category, prefilter_events, run_phase_a_stats,
    select_representative_events, weighted_mean, weighted_ratio, weighted_variance,
};
