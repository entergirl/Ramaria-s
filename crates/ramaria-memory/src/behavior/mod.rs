//! crates/ramaria-memory/src/behavior/mod.rs - 行为层（L3 行为模型）模块
//!
//! 设计特点:
//! - clustering: D2 情境-反应聚类（双通道向量化 + 三路融合相似度 + 密度聚类 + 簇提炼）
//! - sentiment: 情感词典极性提取（D4 规则翻译极性一致性校验用）
//! - rule_gen: D4 规则生成（LLM 翻译 + 极性一致性校验 + 质控 + 参数化）
//! - routing: D5 情境路由（查询构造 + 候选评分 + Top-N 合并）
//! - incremental: D6 增量更新（归簇/待定池/衰减/漂移）
//!
//! 安全约束:
//! - 不记录任何对话原文；聚类/规则只处理 paraphrase 与结构化字段
//! - embedding 与 LLM 均经 trait 注入，便于 mock 确定性测试

pub mod clustering;
pub mod incremental;
pub mod routing;
pub mod rule_gen;
pub mod sentiment;

pub use clustering::{
    BehaviorClusterer, BehaviorSample, DensityClusterResult, RefinedCluster, cosine_clipped,
    dedup_keywords, density_cluster, fused_similarity, jaccard, refine_cluster, sample_from_event,
    vectorize,
};
pub use incremental::{
    IncrementalUpdateOutcome, PendingEvent, PendingPool, assign_event_to_cluster,
    compute_incremental_update, decay_evidence_weights, detect_reaction_drift,
    sample_rule_similarity,
};
pub use routing::{
    MergedDecision, QueryContext, RouteTarget, RoutingParams, RoutingResult, build_query_context,
    merge_route_targets, query_side_jaccard, route_rules, score_rule, valence_conflicts,
};
pub use rule_gen::{
    BEHAVIOR_RULE_PROMPT_VERSION, BehaviorRuleGenerator, GeneratedRule, PolarityVerdict,
    QualityVerdict, RuleDegradeReason, RuleGenConfig, build_evidence, check_polarity,
    compute_confidence, compute_stability, evidence_weight, parameterize, polarity_of_text,
    quality_gate, recency_factor, translate_reaction, validate_avoid,
};
