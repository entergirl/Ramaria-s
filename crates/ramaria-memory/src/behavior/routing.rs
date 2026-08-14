//! crates/ramaria-memory/src/behavior/routing.rs - 情境路由（D5，v3.1 §4.3）
//!
//! 设计特点:
//! - 查询构造：最近 3~5 条消息拼接 → 查询向量 q（情境通道）+ 话题词（tokenize 词频 Top-N）
//! - 候选评分：score = γ·max(0, cos(q, 簇中心)) + (1−γ)·Jaccard(K_query, K_rule)
//!   —— cos clip 到 [0,1] 避免量纲混融；关键词项用**查询侧** Jaccard（分母取查询侧，
//!   避免偏袒窄规则——多关键词宽规则被系统性惩罚）
//! - 阈值 θ_route：全部低于 → 不注入（静默降级，等同 v1.4 行为）
//! - Top 1~3 排序合并：主规则完整注入（reaction + params + avoid），次规则仅合并
//!   avoid 与互补 params；valence 方向矛盾（语义相似但极性相反）→ 丢弃次规则
//! - embedding 不可用 → cos 项权重归零，退化为纯关键词匹配
//! - 纯计算 + embedding trait 注入，便于 mock 确定性测试
//!
//! 边界:
//! - 本模块只产出"路由决策"（命中规则 + 合并结果）；注入 prompt 由 M6（F 任务）
//!   `render_behavior_block` 消费，本版本不触碰 prompt 层。

use ramaria_core::behavior::{BehaviorParams, BehaviorRule};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::Message;

use super::clustering::cosine_clipped;
use crate::bm25::tokenize;

/// 查询构造时的消息条数窗口（最近 3~5 条，取窗口内全部）。
pub const QUERY_MESSAGE_WINDOW: usize = 5;
/// 话题词提取条数上限。
pub const QUERY_KEYWORD_LIMIT: usize = 10;

// =========================================================
// 查询构造
// =========================================================

/// 查询上下文（当前对话情境）。
///
/// 字段约定:
/// - `query_vector`: 查询向量 q（消息拼接的 embedding；embedding 不可用时为 None）。
/// - `keywords`: 话题词（消息内容 tokenize 词频 Top-N，查询侧 Jaccard 用）。
#[derive(Debug, Clone, PartialEq)]
pub struct QueryContext {
    /// 查询向量 q
    pub query_vector: Option<Vec<f32>>,
    /// 话题词
    pub keywords: Vec<String>,
}

/// 从最近消息构造查询上下文。
///
/// 参数:
/// - `messages`: 当前会话消息（取最近 `QUERY_MESSAGE_WINDOW` 条）。
/// - `embedder`: 嵌入模型 provider；`None` → 查询向量为 None（纯关键词降级）。
///
/// 说明:
/// - 拼接最近消息文本（角色前缀 + 内容），embedding 失败仅记 warn、向量置 None，
///   不阻塞查询（静默降级链）。
pub async fn build_query_context(
    messages: &[Message],
    embedder: Option<&dyn EmbeddingProvider>,
) -> RamariaResult<QueryContext> {
    let recent: Vec<&Message> = messages.iter().rev().take(QUERY_MESSAGE_WINDOW).collect();
    // 向量化文本：带角色前缀（供 embedding 区分发言方）
    let mut texts: Vec<String> = Vec::with_capacity(recent.len());
    // 话题词文本：仅消息内容（不含"用户: "前缀，避免噪声词稀释查询侧 Jaccard）
    let mut content_joined = String::new();
    for m in recent.iter().rev() {
        let prefix = match m.role {
            ramaria_core::types::MessageRole::User => "用户: ",
            _ => "对方: ",
        };
        texts.push(format!("{prefix}{}", m.content));
        content_joined.push_str(&m.content);
        content_joined.push('\n');
    }

    // 话题词：纯内容 tokenize 词频 Top-N
    let mut freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for t in tokenize(&content_joined) {
        *freq.entry(t).or_insert(0) += 1;
    }
    let mut kw: Vec<(String, usize)> = freq.into_iter().collect();
    kw.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let keywords: Vec<String> = kw
        .into_iter()
        .take(QUERY_KEYWORD_LIMIT)
        .map(|(k, _)| k)
        .collect();

    let joined = texts.join("\n");
    let query_vector = match embedder {
        Some(emb) => match emb.embed(&joined).await {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, "情境路由查询向量化失败，降级纯关键词");
                None
            }
        },
        None => None,
    };

    Ok(QueryContext {
        query_vector,
        keywords,
    })
}

// =========================================================
// 候选评分
// =========================================================

/// 查询侧 Jaccard：|K_query ∩ K_rule| / |K_query|。
///
/// 说明:
/// - 分母取查询侧——若取规则侧，多关键词宽规则会被系统性惩罚（分子相同、分母更大）。
/// - 查询无话题词 → 0.0。
pub fn query_side_jaccard(query_kw: &[String], rule_kw: &[String]) -> f64 {
    if query_kw.is_empty() {
        return 0.0;
    }
    let rule_set: std::collections::HashSet<&str> = rule_kw.iter().map(String::as_str).collect();
    let mut inter = 0usize;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for k in query_kw {
        if rule_set.contains(k.as_str()) && seen.insert(k.as_str()) {
            inter += 1;
        }
    }
    inter as f64 / query_kw.len() as f64
}

/// 规则候选评分（v3.1 §4.3 Step 2）。
///
/// 公式:
/// - `score = γ·max(0, cos(q, 簇中心)) + (1−γ)·Jaccard(K_query, K_rule)`
/// - cos 项 clip 到 [0,1]（负相关视为 0，不惩罚）。
///
/// 降级:
/// - 查询向量或簇中心任一缺失 → cos 项权重归零，退化为纯关键词（权重归一化）。
pub fn score_rule(query: &QueryContext, rule: &BehaviorRule, gamma: f64) -> f64 {
    let gamma = gamma.clamp(0.0, 1.0);
    let has_vector = query.query_vector.is_some() && rule.situation.centroid.is_some();
    let cos_term = match (&query.query_vector, &rule.situation.centroid) {
        (Some(q), Some(c)) => cosine_clipped(q, c).max(0.0),
        _ => 0.0,
    };
    let jac = query_side_jaccard(&query.keywords, &rule.situation.keywords);
    if has_vector {
        gamma * cos_term + (1.0 - gamma) * jac
    } else {
        // embedding 不可用 → 纯关键词匹配（γ 项无信息）
        jac
    }
}

// =========================================================
// 路由编排
// =========================================================

/// 路由参数（从 `BehaviorConfig` 派生）。
#[derive(Debug, Clone, Copy)]
pub struct RoutingParams {
    /// 路由阈值 θ_route（默认 0.6，全部低于 → 不注入）
    pub theta_route: f64,
    /// cos 项权重 γ（默认 0.7）
    pub gamma: f64,
    /// Top-N 合并上限（默认 3）
    pub top_n: usize,
}

impl From<&BehaviorConfig> for RoutingParams {
    fn from(cfg: &BehaviorConfig) -> Self {
        Self {
            theta_route: cfg.theta_route,
            gamma: cfg.gamma,
            top_n: cfg.top_n,
        }
    }
}

impl Default for RoutingParams {
    fn default() -> Self {
        Self {
            theta_route: 0.6,
            gamma: 0.7,
            top_n: 3,
        }
    }
}

/// 命中的单条规则。
#[derive(Debug, Clone, PartialEq)]
pub struct RouteTarget {
    /// 命中的规则
    pub rule: BehaviorRule,
    /// 路由得分
    pub score: f64,
}

/// 路由结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingResult {
    /// 是否命中（≥1 条规则 ≥ θ_route；false = 静默降级不注入）
    pub matched: bool,
    /// 主规则（Top-1，完整注入 reaction + params + avoid）
    pub primary: Option<RouteTarget>,
    /// 次规则（Top-2/3，仅合并 avoid 与互补 params；已丢弃 valence 矛盾者）
    pub secondary: Vec<RouteTarget>,
}

/// 情境路由（v3.1 §4.3）。
///
/// 流程:
/// 1. 全部启用规则评分。
/// 2. 过滤 score ≥ θ_route。
/// 3. 得分降序取 Top-N。
/// 4. Top-1 为主规则；其余为次规则，与主规则 valence 方向矛盾者丢弃。
///
/// 返回:
/// - `matched = false` 时主/次均为空（调用方静默不注入，等同 v1.4）。
pub fn route_rules(
    rules: &[BehaviorRule],
    query: &QueryContext,
    params: &RoutingParams,
) -> RoutingResult {
    // 1-2. 评分 + 阈值过滤
    let mut scored: Vec<RouteTarget> = rules
        .iter()
        .filter(|r| r.enabled)
        .map(|r| RouteTarget {
            rule: r.clone(),
            score: score_rule(query, r, params.gamma),
        })
        .filter(|t| t.score >= params.theta_route)
        .collect();

    if scored.is_empty() {
        return RoutingResult {
            matched: false,
            primary: None,
            secondary: Vec::new(),
        };
    }

    // 3. 得分降序取 Top-N（同分按 id 升序保证稳定）
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rule.id.cmp(&b.rule.id))
    });
    scored.truncate(params.top_n.max(1));

    let primary = scored.remove(0);
    // 4. 丢弃与主规则 valence 方向矛盾的次规则
    let secondary: Vec<RouteTarget> = scored
        .into_iter()
        .filter(|t| !valence_conflicts(&primary.rule, &t.rule))
        .collect();

    RoutingResult {
        matched: true,
        primary: Some(primary),
        secondary,
    }
}

/// valence 方向矛盾判定（语义相似但极性相反 → 丢弃次规则）。
///
/// 规则:
/// - 双方 valence 均值都显著（|v| > 0.1）且符号相反 → 矛盾。
/// - 任一侧接近中性 → 不判矛盾（无方向信息）。
pub fn valence_conflicts(primary: &BehaviorRule, secondary: &BehaviorRule) -> bool {
    let a = primary.situation.valence_mean;
    let b = secondary.situation.valence_mean;
    a.abs() > 0.1 && b.abs() > 0.1 && a.signum() != b.signum()
}

// =========================================================
// 合并（主 + 次 → 注入决策）
// =========================================================

/// 合并后的注入决策（M6 消费；本版本只产出结构）。
#[derive(Debug, Clone, PartialEq)]
pub struct MergedDecision {
    /// 主规则（完整注入 reaction + params + avoid）
    pub primary_rule: BehaviorRule,
    /// 合并后的 avoid（主 + 次 并集，去重保序）
    pub merged_avoid: Vec<String>,
    /// 合并后的 params（主规则优先，中性维度由次规则互补）
    pub merged_params: BehaviorParams,
}

/// 合并主/次规则（v3.1 §4.3 Step 5）。
///
/// 规则:
/// - avoid：主 + 次 的并集（去重保序）。
/// - params 互补：主规则取值接近"中性默认"的维度（0.5 / 0.0）由次规则补充，
///   有信息量的维度以主规则为准。
pub fn merge_route_targets(primary: &RouteTarget, secondary: &[RouteTarget]) -> MergedDecision {
    let mut merged_avoid: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rule in std::iter::once(&primary.rule).chain(secondary.iter().map(|t| &t.rule)) {
        for w in &rule.avoid {
            if seen.insert(w.clone()) {
                merged_avoid.push(w.clone());
            }
        }
    }

    let mut merged_params = primary.rule.params;
    for t in secondary {
        merged_params = merge_params(&merged_params, &t.rule.params);
    }

    MergedDecision {
        primary_rule: primary.rule.clone(),
        merged_avoid,
        merged_params,
    }
}

/// 参数互补合并：主规则中性维度由次规则补充。
fn merge_params(primary: &BehaviorParams, secondary: &BehaviorParams) -> BehaviorParams {
    BehaviorParams {
        emotional_intensity: if primary.emotional_intensity == 0.0 {
            secondary.emotional_intensity
        } else {
            primary.emotional_intensity
        },
        proactiveness: if (primary.proactiveness - 0.5).abs() < 1e-9 {
            secondary.proactiveness
        } else {
            primary.proactiveness
        },
        detail_level: if (primary.detail_level - 0.5).abs() < 1e-9 {
            secondary.detail_level
        } else {
            primary.detail_level
        },
        formality: if (primary.formality - 0.5).abs() < 1e-9 {
            secondary.formality
        } else {
            primary.formality
        },
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
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
            ctx.keywords.contains(&"加班".to_string())
                || ctx.keywords.contains(&"很累".to_string())
        );
    }

    #[tokio::test]
    async fn build_query_context_empty_messages() {
        let ctx = build_query_context(&[], None).await.expect("空消息成功");
        assert!(ctx.keywords.is_empty());
        assert!(ctx.query_vector.is_none());
    }
}
