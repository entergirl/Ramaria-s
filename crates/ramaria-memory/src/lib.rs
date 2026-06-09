//! rust/crates/ramaria-memory/src/lib.rs - Ramaria 记忆系统核心模块
//!
//! 设计特点:
//! - 实现完整的分层记忆管线: L0→L1 摘要、L1→L2 事件提取、L2→L3 性格推断
//! - 提供通用记忆运算: Ebbinghaus 衰减、RRF 多通道融合
//! - L1 摘要: 取消息→格式化→调LLM→解析JSON→校验→存L1+写关键词
//! - 事件提取: 按触发条件取L1→调LLM→解析结构化事件→降级兜底→生成paraphrase
//! - 所有 LLM 依赖通过 `LlmProvider` trait 注入，便于 mock 测试
//! - 纯数学模块（decay/rrf）零 I/O，不依赖数据库或异步运行时

pub mod decay;
pub mod event;
pub mod inference;
pub mod l1;
pub mod rrf;
mod utils; // 内部共享工具（不暴露到公共 API）

// 后续 Phase 2 子模块在此添加:
// pub mod prompt;
// pub mod rag;
// pub mod retriever;
// pub mod init;

// =========================================================
// 公共 re-export
// =========================================================

// Decay
pub use decay::{
    DecayConfig, adjust_distance, apply_access_boost, calc_decay_r, calc_decay_weight,
    calc_retention,
};

// RRF
pub use rrf::{
    ChannelResult, FusedResult, RrfConfig, rrf_fuse, rrf_single_channel, rrf_two_channels,
};

// L1 Summarizer
pub use l1::{L1Summarizer, L1SummarizerConfig};

// Event Extractor
pub use event::{
    DegradeConfig, EventExtractor, EventExtractorConfig, ParaphraseConfig, build_degraded_event,
    generate_paraphrase,
};

// Inference (Phase A + Phase B + Phase C — 性格推断全管线)
pub use inference::{
    // Phase A: 统计 + 聚类 + 收缩
    AttitudeSample,
    // Phase C: 漂移检测 + 置信度更新
    CalibrationConfig,
    CalibrationDiff,
    CalibrationTracker,
    CategoryDriftResult,
    CategoryEventData,
    // Phase B: LLM 推断 + 后处理
    CategorySignal,
    CategoryStats,
    ClusterAssignment,
    ClusterDescription,
    ClusteringConfig,
    ClusteringResult,
    ConfidenceConfig,
    ConfidenceSummary,
    ConsistencyAnalysis,
    CrossCategoryMetrics,
    DiffAction,
    DimensionDriftResult,
    DriftConfig,
    DriftSummary,
    InferenceResult,
    InferredTrait,
    InferrerConfig,
    PostProcessResult,
    RepresentativeEvent,
    ShrinkConfig,
    StatsConfig,
    StatsSummary,
    TraitConfidenceUpdate,
    TraitDiff,
    build_step1_prompt,
    build_step2_prompt,
    build_step3_prompt,
    compute_calibration_diff,
    compute_category_stats,
    compute_confidence,
    compute_consistency,
    compute_cross_category_metrics,
    compute_dynamic_gamma,
    compute_e_delta,
    compute_e_total,
    compute_emotional_stability,
    compute_global_stats,
    compute_narrative_consistency,
    compute_share_kurtosis,
    compute_share_skewness,
    compute_trait_diff,
    cosine_similarity,
    detect_category_drift,
    detect_dimension_drift,
    extract_primary_category,
    group_by_category,
    logit,
    merge_consistency,
    mock_infer,
    permutation_test,
    post_process_inference,
    prefilter_events,
    run_clustering,
    run_confidence_update,
    run_drift_detection,
    run_phase_a_stats,
    run_shrinkage,
    select_representative_events,
    shrink_category,
    shrink_presentation,
    shrink_share,
    shrink_valence,
    sigmoid,
    simple_density_cluster,
    time_decay_weight,
    update_trait_confidence,
    wasserstein_1d,
    weighted_mean,
    weighted_ratio,
    weighted_variance,
};

/// 模块存活检查 (Phase 0 占位，可后续移除)
pub fn hello_memory() -> &'static str {
    "ramaria-memory is ready"
}
