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
//! - 查询侧话题词可经关键词池别名归一（`QueryKeywordNormalizer`）：
//!   词典增强分词 + 别名/待确认词 → 规范词，使"口语说法 ↔ 事件关键词"更易命中；
//!   词典为空 / 服务不可达时原样退化为纯 bigram 词频（零 embedding、行为等价）
//! - 纯计算 + embedding trait 注入，便于 mock 确定性测试
//!
//! 边界:
//! - 本模块只产出"路由决策"（命中规则 + 合并结果）；注入 prompt 由 M6（F 任务）
//!   `render_behavior_block` 消费，本版本不触碰 prompt 层。

use std::collections::HashMap;

use ramaria_core::behavior::{BehaviorParams, BehaviorRule};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::Message;

use super::clustering::cosine_clipped;
use crate::bm25::tokenize;
use crate::keyword::normalizer::{BigramWithDictionaryNormalizer, KeywordNormalizer};
use crate::keyword::pool::KeywordPool;

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

/// 查询侧关键词规范化快照——词典增强分词 + 别名归一（值形态，供路由使用）。
///
/// 职责:
/// - 从关键词池取词条（词典 + 别名解析表），使路由查询侧话题词经别名归一后参与
///   Jaccard 评分："口语说法 ↔ 事件关键词"经词典/别名归一后更易命中。
/// - 词典为空 / 服务不可达 → `empty()`（查询退化为纯 bigram 词频，行为等价）。
/// - 值形态可在读锁内构造、锁外复用；纯计算，零 I/O，零 embedding。
#[derive(Debug, Clone, Default)]
pub struct QueryKeywordNormalizer {
    /// 词典增强分词用全量词条文本（canonical + alias + pending）
    dictionary: Vec<String>,
    /// 别名 / 待确认词 → 规范词文本
    resolve: HashMap<String, String>,
}

impl QueryKeywordNormalizer {
    /// 空规范化器（词典为空，查询退化为纯 bigram 词频）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从关键词池构造（全量词条入词典；别名/待确认映射到规范词）。
    pub fn from_pool(pool: &KeywordPool) -> Self {
        let mut dictionary = Vec::with_capacity(pool.len());
        let mut resolve = HashMap::new();
        for entry in pool.iter() {
            let text = entry.token.as_str().to_string();
            dictionary.push(text.clone());
            if let Some(canonical) = pool.resolve(&entry.token)
                && canonical != &entry.token
            {
                resolve.insert(text, canonical.as_str().to_string());
            }
        }
        Self {
            dictionary,
            resolve,
        }
    }

    /// 词典是否为空（空时 `normalize_tokens` 退化为纯 bigram）。
    pub fn is_empty(&self) -> bool {
        self.dictionary.is_empty()
    }

    /// 话题词规范化：文本 → 词频 Top-N 前的候选词序列。
    ///
    /// - 词典非空：词典增强分词（完整词条保留），再对命中的别名/待确认词替换为规范词。
    /// - 词典为空：委托 `bm25::tokenize`（与既有纯 bigram 行为逐词一致）。
    pub fn normalize_tokens(&self, text: &str) -> Vec<String> {
        if self.is_empty() {
            return tokenize(text);
        }
        let dict_normalizer = BigramWithDictionaryNormalizer::from_dictionary(&self.dictionary);
        dict_normalizer
            .normalize(text)
            .into_iter()
            .map(|t| {
                self.resolve
                    .get(t.as_str())
                    .cloned()
                    .unwrap_or_else(|| t.into_inner())
            })
            .collect()
    }
}

/// 从最近消息构造查询上下文（无关键词池规范化，纯 bigram 词频）。
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
    build_query_context_with_normalizer(messages, embedder, &QueryKeywordNormalizer::empty()).await
}

/// 从最近消息构造查询上下文（可选关键词池规范化）。
///
/// 与 [`build_query_context`] 的唯一区别：话题词先经 `normalizer` 词典增强分词 +
/// 别名归一（`normalizer` 为空时行为与旧版纯 bigram 完全一致）。
///
/// 参数:
/// - `messages`: 当前会话消息（取最近 `QUERY_MESSAGE_WINDOW` 条）。
/// - `embedder`: 嵌入模型 provider；`None` → 查询向量为 None。
/// - `normalizer`: 查询侧关键词规范化器（空 = 纯 bigram 词频；调用方在锁外持有）。
pub async fn build_query_context_with_normalizer(
    messages: &[Message],
    embedder: Option<&dyn EmbeddingProvider>,
    normalizer: &QueryKeywordNormalizer,
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

    // 话题词：normalizer 词典增强/别名归一后的词频 Top-N
    let mut freq: HashMap<String, usize> = HashMap::new();
    for t in normalizer.normalize_tokens(&content_joined) {
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
mod tests;
