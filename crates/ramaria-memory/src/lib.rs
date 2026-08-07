//! rust/crates/ramaria-memory/src/lib.rs - Ramaria 记忆系统核心模块
//!
//! 设计特点:
//! - 实现完整的分层记忆管线: L0→L1 摘要、L1→L2 事件提取、L2→L3 性格推断
//! - 提供通用记忆运算: Ebbinghaus 衰减、RRF 多通道融合
//! - L1 摘要: 取消息→格式化→调LLM→解析JSON→校验→存L1+写关键词
//! - 事件提取: 按触发条件取L1→调LLM→解析结构化事件→降级兜底→生成paraphrase
//! - 所有 LLM 依赖通过 `LlmProvider` trait 注入，便于 mock 测试
//! - 纯数学模块（decay/rrf）零 I/O，不依赖数据库或异步运行时

pub mod bm25;
pub mod decay;
pub mod event;
pub mod graph_retriever;
pub mod inference;
pub mod init;
pub mod job;
pub mod keyword;
pub mod l1;
pub mod llm_gate;
pub mod prompt;
pub mod rag;
pub mod rebuild;
pub mod retriever;
pub mod rrf;
pub mod token_budget;
mod utils;
pub mod utt;
pub mod vector; // 内部共享工具（不暴露到公共 API）

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

// BM25 全文检索
pub use bm25::{
    Bm25Config, Bm25Index, Bm25IndexBuilder, DocId, tokenize, tokenize_fields, tokenize_with_dict,
    tokenize_with_freq,
};

// Vector 向量检索
pub use vector::{
    BruteForceIndex, VectorEntry, VectorHit, VectorIndex, VectorIndexConfig, VectorIndexError,
    make_vector_label, parse_vector_label,
};

// Graph 图谱检索
pub use graph_retriever::{
    GraphEdge, GraphHit, GraphNode, GraphRetriever, GraphRetrieverConfig, graph_hits_to_rrf_pairs,
};

// Retriever 三通道组合检索
pub use retriever::{
    L1DocView, L2DocView, Retriever, RetrieverConfig, SearchRequest, SearchResult,
};

// RAG Persona-Aware 检索增强生成
// 注：PersonaKind 已统一到 ramaria_core::types，不再从此模块 re-export
pub use rag::{
    RagConfig, assemble_rag_context, filter_by_persona, format_context_text, format_graph_context,
};

// L1 Summarizer
pub use l1::{L1Summarizer, L1SummarizerConfig};

// Event Extractor & TopicBatcher
pub use event::{
    DegradeConfig, EventExtractor, EventExtractorConfig, L1Item, ParaphraseConfig,
    TopicBatcherConfig, TopicCluster, build_degraded_event, generate_paraphrase,
};

// Inference ( + + — 性格推断全管线)
pub use inference::{
    AdmissionTrack,
    // 统计 + 聚类 + 收缩
    AttitudeSample,
    CalibratedWeightConfig,
    // 漂移检测 + 置信度更新
    CalibrationConfig,
    CalibrationDiff,
    CalibrationTracker,
    CategoryDriftResult,
    CategoryEventData,
    // LLM 推断 + 后处理
    CategorySignal,
    CategoryStats,
    // 因果链特征（A8）
    CausalChainFeatures,
    ClassifiedEvents,
    ClusterAssignment,
    ClusterDescription,
    ClusteringConfig,
    ClusteringResult,
    ConfidenceConfig,
    ConfidenceSummary,
    ConsistencyAnalysis,
    CrossCategoryMetrics,
    // 跨版本簇匹配
    CrossVersionMatch,
    CrossVersionMatchResult,
    CyclePattern,
    DiffAction,
    DimensionDriftResult,
    DriftConfig,
    DriftSummary,
    EventEnrichment,
    HistoricalSnapshot,
    InferenceResult,
    InferredTrait,
    InferrerConfig,
    PhaseBResult,
    PhaseBSource,
    PhaseCResult,
    PostProcessResult,
    RepresentativeEvent,
    ShrinkConfig,
    ShrinkPrior,
    StatsConfig,
    StatsSummary,
    TentativePromotionConfig,
    TentativePromotionResult,
    TraitConfidenceUpdate,
    TraitDiff,
    apply_layered_shrinkage,
    build_layer_hints_from_traits,
    build_step1_prompt,
    build_step2_prompt,
    build_step3_prompt,
    calibrate_salience,
    classify_event,
    classify_events,
    compute_calibrated_weight,
    compute_calibrated_weights_batch,
    compute_calibration_diff,
    compute_category_stats,
    compute_confidence,
    compute_consistency,
    compute_cross_category_metrics,
    compute_domain_prior,
    compute_dynamic_gamma,
    compute_e_delta,
    compute_e_total,
    compute_emotional_stability,
    compute_global_stats,
    compute_narrative_consistency,
    compute_share_kurtosis,
    compute_share_skewness,
    compute_simple_weight,
    compute_simple_weights_batch,
    compute_trait_diff,
    cosine_similarity,
    detect_category_drift,
    detect_dimension_drift,
    extract_causal_features,
    extract_primary_category,
    format_causal_features_text,
    generate_semantic_label,
    generate_semantic_labels_for_clusters,
    group_by_category,
    logit,
    match_clusters_cross_version,
    merge_consistency,
    mock_infer,
    permutation_test,
    persist_cluster_snapshots_with_semantic_labels,
    post_process_inference,
    prefilter_events,
    promote_tentative_events,
    query_cross_version_matches,
    run_clustering,
    run_confidence_update,
    run_drift_detection,
    run_phase_a_stats,
    run_phase_b_inference,
    run_phase_c_update,
    run_shrinkage,
    run_shrinkage_layered,
    select_prior,
    select_representative_events,
    shrink_category,
    shrink_presentation,
    shrink_share,
    shrink_valence,
    sigmoid,
    simple_density_cluster,
    situation_multiplier,
    time_decay_weight,
    update_trait_confidence,
    wasserstein_1d,
    weighted_mean,
    weighted_ratio,
    weighted_variance,
};

// Keyword 关键词处理
pub use keyword::{AliasManager, BigramWithDictionaryNormalizer};

// Init 冷启动
pub use init::{
    ColdStartConfig, ColdStartResult, PersonaToml, SHARED_CHAT_STYLE_RULES,
    initialize_rama_persona, parse_persona_toml,
};

// Job 后台任务管理
pub use job::{JobManager, JobManagerConfig, JobResult, JobType, status as job_status};

// Rebuild 索引重建
pub use rebuild::{IndexRebuilder, RebuildConfig, RebuildStats, events_to_views, l1_list_to_views};

// Prompt System Prompt 构建
pub use prompt::{
    builder::{PromptConfig, PromptContext, assemble_prompt, build_cross_session_narrative},
    example_selector::{ExampleSelector, ExampleSelectorConfig, extract_keywords},
    injection_guard::{MemoryInjectionStatus, apply_injection_guard, check_injection},
};

// Token Budget 管理
pub use token_budget::{
    BudgetedContext, TokenBudgetConfig, apply_token_budget, estimate_tokens, truncate_at_boundary,
};
