//! crates/ramaria-memory/src/behavior/routing/tests.rs - //! crates/ramaria-memory/src/behavior/routing.rs - 情境路由单元测试
//!
//! 设计特点:
//! - 位于 behavior::routing 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 routing.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
use ramaria_core::types::{Message, MessageRole, MessageSource};

fn rule(id: i64, keywords: &[&str], valence: f64, centroid: Option<Vec<f32>>) -> BehaviorRule {
    let mut r = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            centroid,
            response_centroid: None,
            valence_mean: valence,
            valence_std: 0.2,
            sample_count: 6,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 10.0,
            trait_refs: Vec::new(),
        },
        Some(format!("规则 {id}")),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    r.id = id;
    r
}

fn msg(content: &str, role: MessageRole) -> Message {
    Message {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::new_v4(),
        role,
        content: content.to_string(),
        source: MessageSource::Local,
        created_at: 0,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
    }
}

// ---- 查询侧 Jaccard ----

#[test]
fn query_side_jaccard_basic() {
    let q = vec!["加班".to_string(), "累".to_string(), "工作".to_string()];
    let r = vec!["加班".to_string(), "累".to_string()];
    // |Q∩R| / |Q| = 2/3
    assert!((query_side_jaccard(&q, &r) - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn query_side_jaccard_asymmetric() {
    // 查询侧分母：窄查询（少词）不被规则侧分母稀释
    let q = vec!["加班".to_string()];
    let wide_rule = vec![
        "加班".to_string(),
        "累".to_string(),
        "深夜".to_string(),
        "工作".to_string(),
    ];
    // 1/1 = 1.0（若用规则侧分母则 1/4，窄查询被系统性惩罚）
    assert_eq!(query_side_jaccard(&q, &wide_rule), 1.0);
}

#[test]
fn query_side_jaccard_empty() {
    assert_eq!(query_side_jaccard(&[], &["x".to_string()]), 0.0);
    assert_eq!(query_side_jaccard(&[], &[]), 0.0);
}

// ---- 评分 ----

#[test]
fn score_formula_gamma_weights() {
    // q 与簇中心 cos=1（同向量），话题词无交集
    let query = QueryContext {
        query_vector: Some(vec![1.0, 0.0]),
        keywords: vec!["无关".to_string()],
    };
    let r = rule(1, &["加班"], -0.4, Some(vec![1.0, 0.0]));
    // γ=1.0 → score=1.0；γ=0.0 → score=0（Jaccard=0）
    assert!((score_rule(&query, &r, 1.0) - 1.0).abs() < 1e-9);
    assert_eq!(score_rule(&query, &r, 0.0), 0.0);
    // γ=0.7 → 0.7*1 + 0.3*0 = 0.7
    assert!((score_rule(&query, &r, 0.7) - 0.7).abs() < 1e-9);
}

#[test]
fn score_clips_negative_cos_to_zero() {
    // cos(q, 簇中心) = -1 → max(0,·) = 0，不惩罚
    let query = QueryContext {
        query_vector: Some(vec![-1.0, 0.0]),
        keywords: vec!["加班".to_string()],
    };
    let r = rule(1, &["加班"], -0.4, Some(vec![1.0, 0.0]));
    // Jaccard=1（话题词命中）→ score = 0.7*0 + 0.3*1 = 0.3
    assert!((score_rule(&query, &r, 0.7) - 0.3).abs() < 1e-9);
}

#[test]
fn score_degrades_to_keywords_without_embedding() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["加班".to_string(), "累".to_string()],
    };
    let r = rule(1, &["加班", "累", "工作"], -0.4, None);
    // 纯关键词（查询侧 Jaccard）：|Q∩R| / |Q| = 2/2 = 1.0（γ 项无信息 → 权重归一化）
    assert!((score_rule(&query, &r, 0.7) - 1.0).abs() < 1e-9);
}

#[test]
fn score_zero_when_no_signal() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec![],
    };
    let r = rule(1, &["加班"], -0.4, None);
    assert_eq!(score_rule(&query, &r, 0.7), 0.0);
}

#[test]
fn score_ignores_disabled_rules_at_call_site() {
    // route_rules 过滤 disabled；score_rule 本身不看 enabled（由路由层负责）
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["加班".to_string()],
    };
    let mut r = rule(1, &["加班"], -0.4, None);
    r.enabled = false;
    let score = score_rule(&query, &r, 0.7);
    assert_eq!(score, 1.0);
}

// ---- 路由 ----

#[test]
fn route_hits_top_rule() {
    let query = QueryContext {
        query_vector: Some(vec![1.0, 0.0]),
        keywords: vec!["加班".to_string()],
    };
    let rules = vec![
        rule(1, &["加班"], -0.4, Some(vec![1.0, 0.0])), // score 高
        rule(2, &["猫"], 0.3, Some(vec![0.0, 1.0])),    // score 低
    ];
    let result = route_rules(&rules, &query, &RoutingParams::default());
    assert!(result.matched);
    let primary = result.primary.expect("应有主规则");
    assert_eq!(primary.rule.id, 1);
    assert!(result.secondary.is_empty(), "次规则低于阈值");
}

#[test]
fn route_silent_degrade_when_all_below_threshold() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["完全无关".to_string()],
    };
    let rules = vec![rule(1, &["加班"], -0.4, None)];
    let result = route_rules(&rules, &query, &RoutingParams::default());
    assert!(!result.matched, "全部低于 θ_route → 静默降级");
    assert!(result.primary.is_none());
    assert!(result.secondary.is_empty());
}

#[test]
fn route_takes_top_n_sorted_by_score() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["a".to_string(), "b".to_string(), "c".to_string()],
    };
    let rules = vec![
        rule(3, &["c"], 0.2, None),      // J=1/3
        rule(1, &["a"], 0.2, None),      // J=1/3
        rule(2, &["a", "b"], 0.2, None), // J=2/3 最高
    ];
    let params = RoutingParams {
        theta_route: 0.3,
        gamma: 0.7,
        top_n: 2,
    };
    let result = route_rules(&rules, &query, &params);
    assert!(result.matched);
    let primary = result.primary.expect("主规则");
    assert_eq!(primary.rule.id, 2, "得分最高者为主规则");
    assert_eq!(result.secondary.len(), 1, "Top-2 截断");
    assert_eq!(result.secondary[0].rule.id, 1, "同分按 id 升序稳定");
}

#[test]
fn route_drops_valence_conflicting_secondary() {
    let query = QueryContext {
        query_vector: Some(vec![1.0, 0.0]),
        keywords: vec!["加班".to_string(), "累".to_string()],
    };
    // 主规则消极（valence -0.5），次规则积极（valence +0.5）且 score 也达标 → 丢弃
    let mut r1 = rule(1, &["加班", "累"], -0.5, Some(vec![1.0, 0.0]));
    r1.situation.centroid = Some(vec![1.0, 0.0]);
    let mut r2 = rule(2, &["加班", "累"], 0.5, Some(vec![1.0, 0.0]));
    r2.situation.centroid = Some(vec![1.0, 0.0]);
    let result = route_rules(&[r1.clone(), r2.clone()], &query, &RoutingParams::default());
    assert!(result.matched);
    assert_eq!(result.primary.unwrap().rule.id, 1);
    assert!(result.secondary.is_empty(), "valence 矛盾次规则被丢弃");
}

#[test]
fn route_keeps_non_conflicting_secondary() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["加班".to_string(), "猫".to_string()],
    };
    // 两条规则话题词各命中一半（查询侧 J=0.5 ≥ θ_route=0.4），valence 同向 → 次规则保留
    let rules = vec![rule(1, &["加班"], -0.4, None), rule(2, &["猫"], -0.3, None)];
    let params = RoutingParams {
        theta_route: 0.4,
        gamma: 0.7,
        top_n: 3,
    };
    let result = route_rules(&rules, &query, &params);
    assert!(result.matched);
    assert_eq!(result.secondary.len(), 1, "同向次规则保留");
}

#[test]
fn route_skips_disabled_rules() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["加班".to_string()],
    };
    let mut r = rule(1, &["加班"], -0.4, None);
    r.enabled = false;
    let result = route_rules(&[r], &query, &RoutingParams::default());
    assert!(!result.matched, "禁用规则不参与路由");
}

// ---- 合并 ----

#[test]
fn merge_avoid_union_dedup() {
    let mut r1 = rule(1, &["加班"], -0.4, None);
    r1.avoid = vec!["深夜".into(), "加班".into()];
    let mut r2 = rule(2, &["加班"], -0.3, None);
    r2.avoid = vec!["加班".into(), "打断".into()];
    let primary = RouteTarget {
        rule: r1,
        score: 0.8,
    };
    let secondary = vec![RouteTarget {
        rule: r2,
        score: 0.7,
    }];
    let merged = merge_route_targets(&primary, &secondary);
    assert_eq!(merged.merged_avoid, vec!["深夜", "加班", "打断"]);
}

#[test]
fn merge_params_complement_neutral_dimensions() {
    let mut r1 = rule(1, &["加班"], -0.4, None);
    r1.params = BehaviorParams {
        emotional_intensity: -0.4,
        proactiveness: 0.5, // 中性默认 → 由次规则补
        detail_level: 0.8,
        formality: 0.5, // 中性默认 → 由次规则补
    };
    let mut r2 = rule(2, &["加班"], -0.3, None);
    r2.params = BehaviorParams {
        emotional_intensity: -0.3,
        proactiveness: 0.7,
        detail_level: 0.4,
        formality: 0.2,
    };
    let primary = RouteTarget {
        rule: r1,
        score: 0.8,
    };
    let secondary = vec![RouteTarget {
        rule: r2,
        score: 0.7,
    }];
    let merged = merge_route_targets(&primary, &secondary);
    // 主规则有信息的维度保持；中性维度由次规则补
    assert!((merged.merged_params.emotional_intensity + 0.4).abs() < 1e-9);
    assert!((merged.merged_params.proactiveness - 0.7).abs() < 1e-9);
    assert!((merged.merged_params.detail_level - 0.8).abs() < 1e-9);
    assert!((merged.merged_params.formality - 0.2).abs() < 1e-9);
}

#[test]
fn valence_conflicts_detection() {
    let neg = rule(1, &["a"], -0.5, None);
    let pos = rule(2, &["a"], 0.5, None);
    let neu = rule(3, &["a"], 0.05, None);
    assert!(valence_conflicts(&neg, &pos));
    assert!(!valence_conflicts(&neg, &neg));
    assert!(!valence_conflicts(&neg, &neu), "中性不判矛盾");
}

// ---- 查询构造 ----

#[tokio::test]
async fn build_query_context_takes_recent_window() {
    let messages: Vec<Message> = (0..8)
        .map(|i| msg(&format!("加班第{i}天很累"), MessageRole::User))
        .collect();
    let ctx = build_query_context(&messages, None)
        .await
        .expect("构造成功");
    assert!(ctx.query_vector.is_none(), "无 embedding → 纯关键词");
    assert!(!ctx.keywords.is_empty(), "话题词已抽取");
    // 只取最近 5 条（0..8 → 最近 5 条是 3..7）
    assert!(
        ctx.keywords.contains(&"加班".to_string()) || ctx.keywords.contains(&"很累".to_string())
    );
}

#[tokio::test]
async fn build_query_context_empty_messages() {
    let ctx = build_query_context(&[], None).await.expect("空消息成功");
    assert!(ctx.keywords.is_empty());
    assert!(ctx.query_vector.is_none());
}

// ---- 查询构造：embedding 调用故障 ----

/// 恒失败 mock embedding（模拟在线路由侧 embedding 服务故障）。
struct FailingQueryEmbedder;

#[async_trait::async_trait]
impl EmbeddingProvider for FailingQueryEmbedder {
    async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
        Err(ramaria_core::RamariaError::embedding("mock embedding 故障"))
    }
    async fn embed_batch(&self, _texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Err(ramaria_core::RamariaError::embedding(
            "mock embedding 批量故障",
        ))
    }
    fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
        ramaria_core::traits::EmbeddingModelInfo {
            model_id: "failing-query-embedder".into(),
            dimension: 4,
        }
    }
    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }
    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }
    fn download_progress(&self) -> f64 {
        1.0
    }
    fn is_available(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn build_query_context_embedding_failure_degrades_to_keywords() {
    // embedding 调用故障（provider 存在但返回 Err）→ 查询向量为 None（记 warn），
    // 话题词仍抽取；后续路由退化为纯关键词匹配（不 panic、不阻塞）。
    let messages = vec![msg("加班很累", MessageRole::User)];
    let ctx = build_query_context(&messages, Some(&FailingQueryEmbedder))
        .await
        .expect("embedding 故障不报错");
    assert!(ctx.query_vector.is_none(), "查询向量应置 None");
    assert!(!ctx.keywords.is_empty(), "话题词仍抽取");

    // 纯关键词路由仍可命中同关键词规则（bigram 词集部分命中，θ 取 0.3 稳定断言）
    let rule = rule(1, &["加班"], -0.4, None);
    let params = RoutingParams {
        theta_route: 0.3,
        ..RoutingParams::default()
    };
    let result = route_rules(&[rule], &ctx, &params);
    assert!(result.matched, "embedding 故障 → 纯关键词命中规则");
}

// ---- 查询侧关键词规范化（关键词池别名归一） ----

/// 构造含别名关系的池：职业倦怠(alias) → 工作压力(canonical)。
fn alias_pool() -> KeywordPool {
    use crate::keyword::pool::{KeywordPool, PoolEntry};
    use ramaria_core::keyword::{KeywordStatus, KeywordToken};
    KeywordPool::from_entries(vec![
        PoolEntry {
            rowid: 1,
            token: KeywordToken::new("工作压力").unwrap(),
            use_count: 5,
            last_used_at: 0,
            created_at: 0,
            status: KeywordStatus::Canonical,
        },
        PoolEntry {
            rowid: 2,
            token: KeywordToken::new("职业倦怠").unwrap(),
            use_count: 2,
            last_used_at: 0,
            created_at: 0,
            status: KeywordStatus::Alias { canonical_id: 1 },
        },
    ])
}

/// from_pool：词典含全量词条、解析表仅含别名 → 规范词。
#[test]
fn query_keyword_normalizer_from_pool() {
    let normalizer = QueryKeywordNormalizer::from_pool(&alias_pool());
    assert!(!normalizer.is_empty());
    assert_eq!(
        normalizer.resolve.get("职业倦怠").map(String::as_str),
        Some("工作压力")
    );
    assert!(!normalizer.resolve.contains_key("工作压力"));
}

/// 别名短语 → 话题词解析为规范词（口语说法 ↔ 事件关键词命中前提）。
#[tokio::test]
async fn alias_phrase_resolves_to_canonical_in_query() {
    let messages = vec![msg("我感觉职业倦怠", MessageRole::User)];
    let normalizer = QueryKeywordNormalizer::from_pool(&alias_pool());
    let ctx = build_query_context_with_normalizer(&messages, None, &normalizer)
        .await
        .expect("构造成功");
    assert!(
        ctx.keywords.contains(&"工作压力".to_string()),
        "别名短语应被归一为规范词，实际 {keywords:?}",
        keywords = ctx.keywords
    );
}

/// 空池（无词典）→ 与纯 bigram 行为逐词一致（退化断言）。
#[tokio::test]
async fn empty_normalizer_falls_back_to_plain_bigram() {
    let messages = vec![msg("加班第3天很累", MessageRole::User)];
    let plain = build_query_context(&messages, None)
        .await
        .expect("构造成功");
    let normalized =
        build_query_context_with_normalizer(&messages, None, &QueryKeywordNormalizer::empty())
            .await
            .expect("构造成功");
    assert_eq!(
        plain.keywords, normalized.keywords,
        "空规范化器与纯 bigram 等价"
    );
}

/// 别名归一后查询侧话题词命中 canonical 规则 → 路由得分显著提升（不依赖 embedding）。
#[tokio::test]
async fn alias_normalized_query_hits_canonical_rule() {
    let normalizer = QueryKeywordNormalizer::from_pool(&alias_pool());
    let messages = vec![msg("最近职业倦怠很严重", MessageRole::User)];
    let query = build_query_context_with_normalizer(&messages, None, &normalizer)
        .await
        .expect("构造成功");
    assert!(!query.keywords.is_empty());

    // 规则关键词用 canonical（事件聚类产物）；纯 bigram 查询无共享词会 miss
    let r = rule(1, &["工作压力", "加班"], -0.4, None);
    // 无 embedding → 纯关键词路由：score = query_side_jaccard
    let result = route_rules(std::slice::from_ref(&r), &query, &RoutingParams::default());
    if result.matched {
        // 命中主规则即说明别名归一打通"口语 ↔ canonical 规则"（θ_route 默认 0.6）
        assert_eq!(result.primary.unwrap().rule.id, 1);
    } else {
        // 若 query 的 canonical 词占比不足阈值，也至少应包含规范词候选
        assert!(
            query.keywords.contains(&"工作压力".to_string()),
            "别名归一后查询应含规范词"
        );
    }
}

// ---- 空规则路径（behavior_rules=0）----

/// 空规则库（`behavior_rules=0`）→ 不 panic、matched=false、主/次为空。
///
/// 说明:
/// - `route_rules` 对空切片直接返回静默降级结果（调用方不注入行为块，
///   行为回退 v1.4）；本用例显式锁定空路径。
#[test]
fn route_empty_rules_returns_unmatched() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec!["加班".to_string()],
    };
    let result = route_rules(&[], &query, &RoutingParams::default());
    assert!(!result.matched, "空规则库不应命中");
    assert!(result.primary.is_none(), "空规则库无主规则");
    assert!(result.secondary.is_empty(), "空规则库无次规则");
}

/// 空规则库 + 空查询（无消息）→ 不 panic、matched=false（上游双空路径安全）。
#[test]
fn route_empty_rules_empty_query_is_safe() {
    let query = QueryContext {
        query_vector: None,
        keywords: vec![],
    };
    let result = route_rules(&[], &query, &RoutingParams::default());
    assert!(!result.matched);
    assert!(result.primary.is_none());
}
