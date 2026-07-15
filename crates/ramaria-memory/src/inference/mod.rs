//! rust/crates/ramaria-memory/src/inference/mod.rs - L2→L3 性格推断管线模块
//!
//! 设计特点:
//! - (纯数值): stats.rs 统计特征（v1.3 校准权重链）、clustering.rs 态度聚类、shrink.rs 贝叶斯收缩
//! - (LLM 推断): inferrer.rs 三步结构化推断 + mock + 后处理
//! - (增量更新): drift.rs Wasserstein 漂移检测、confidence.rs 证据累计置信度
//! - 编排层: orchestrator.rs Phase B/C 异步编排 + 降级 + 持久化
//! - 全量校准: calibration.rs 累积 10 轮触发 + 全量差异对比
//! - 超参数锁定: HDBSCAN min_cluster_size=3, UMAP n_components=12, 置换检验 B=1000
//! - v1.3 新增: AdmissionTrack 三轨准入、CalibratedWeightConfig 校准权重链、EventEnrichment 增强数据

pub mod calibration;
pub mod causal;
pub mod clustering;
pub mod confidence;
pub mod drift;
pub mod inferrer;
pub mod orchestrator;
pub mod shrink;
pub mod stats;

// =========================================================
// 公共 re-export
// =========================================================

pub use calibration::{
    CalibrationConfig, CalibrationDiff, CalibrationTracker, compute_calibration_diff,
};
pub use causal::{
    CausalChainFeatures, CyclePattern, extract_causal_features, format_causal_features_text,
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
pub use orchestrator::{
    PhaseBResult, PhaseBSource, PhaseCResult, apply_layered_shrinkage,
    build_layer_hints_from_traits, run_phase_b_inference, run_phase_c_update,
};
pub use shrink::{
    ShrinkConfig, ShrinkPrior, compute_domain_prior, compute_dynamic_gamma, compute_global_stats,
    logit, run_shrinkage, run_shrinkage_layered, select_prior, shrink_category,
    shrink_presentation, shrink_share, shrink_valence, sigmoid,
};
pub use stats::{
    AdmissionTrack, CalibratedWeightConfig, CategoryStats, ClassifiedEvents, CrossCategoryMetrics,
    EventEnrichment, RepresentativeEvent, StatsConfig, StatsSummary, TentativePromotionConfig,
    TentativePromotionResult, calibrate_salience, classify_event, classify_events,
    compute_calibrated_weight, compute_calibrated_weights_batch, compute_category_stats,
    compute_cross_category_metrics, compute_emotional_stability, compute_narrative_consistency,
    compute_share_kurtosis, compute_share_skewness, compute_simple_weight,
    compute_simple_weights_batch, extract_primary_category, group_by_category, prefilter_events,
    promote_tentative_events, run_phase_a_stats, select_representative_events,
    situation_multiplier, weighted_mean, weighted_ratio, weighted_variance,
};
