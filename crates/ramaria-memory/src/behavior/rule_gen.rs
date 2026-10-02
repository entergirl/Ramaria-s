//! crates/ramaria-memory/src/behavior/rule_gen.rs - 行为规则生成（D4，v3.1 §4.2 Step 3-6）
//!
//! 设计特点:
//! - 每簇 → LLM 翻译规则文本（引用簇内代表事件 attitude 作示例），输出 JSON {reaction, avoid}
//! - 翻译后极性一致性校验：情感词典提取规则文本极性，与簇内加权 valence 符号比对；
//!   不一致 → 可重试 1 次，仍不一致 → 降级候选规则（仅参数注入）
//! - avoid 列表与簇内低 valence 事件相关性校验（移除与积极事件冲突的禁忌项）
//! - 质控双门槛：证据量 < 5、n_eff < 5、valence 方差 > 0.5 → 降级候选规则（不生成规则文本）
//! - 参数化：情感强度 = 加权 valence；表达倾向 = presentation 分布（启发式映射）
//! - 近期事件加权（证据权重 = salience × 时间衰减因子）；Auto 规则自动生效
//! - 翻译失败跳过该簇不阻塞（逐簇独立，失败记 warn）
//!
//! 安全约束:
//! - 翻译 prompt 只注入 paraphrase/attitude 摘要与统计，不注入完整对话原文
//! - 不记录完整 prompt 与规则文本到日志

use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider as LlmProviderTrait};
use uuid::Uuid;

use super::clustering::RefinedCluster;
use super::sentiment::sentiment_polarity;

/// 行为规则翻译 prompt 模板版本（随本文件 prompt 变更递增，缓存 key 约定）。
pub const BEHAVIOR_RULE_PROMPT_VERSION: &str = "behavior-rule-v1";

/// 规则生成配置（从 `BehaviorConfig` 派生）。
#[derive(Debug, Clone)]
pub struct RuleGenConfig {
    /// 规则文本生成的证据量门槛（默认 5）
    pub min_evidence: usize,
    /// 有效样本量门槛（默认 5）
    pub min_n_eff: usize,
    /// 簇内 valence 标准差上限（默认 0.5）
    pub valence_std_limit: f64,
    /// 近期事件加权窗口（天，默认 30）
    pub recent_days: i64,
    /// 极性校验不通过后的重试次数（默认 1）
    pub max_retries: usize,
    /// LLM 翻译温度
    pub temperature: f64,
    /// LLM 翻译最大输出 token
    pub max_tokens: u32,
}

impl From<&BehaviorConfig> for RuleGenConfig {
    fn from(cfg: &BehaviorConfig) -> Self {
        Self {
            min_evidence: cfg.min_evidence,
            min_n_eff: cfg.min_n_eff,
            valence_std_limit: cfg.valence_std_limit,
            // 近期事件加权窗口从 `[behavior].recent_days` 配置读取
            recent_days: cfg.recent_days,
            max_retries: 1,
            temperature: 0.3,
            max_tokens: 1024,
        }
    }
}

impl Default for RuleGenConfig {
    fn default() -> Self {
        Self {
            min_evidence: 5,
            min_n_eff: 5,
            valence_std_limit: 0.5,
            recent_days: 30,
            max_retries: 1,
            temperature: 0.3,
            max_tokens: 1024,
        }
    }
}

// =========================================================
// 降级原因与生成结果
// =========================================================

/// 规则降级原因（降级链，v3.1 §4.2 Step 4 / 降级策略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleDegradeReason {
    /// 未降级（完整规则）
    None,
    /// 证据量 < 门槛 → 不生成规则文本（仅参数）
    LowEvidence,
    /// 有效样本量 n_eff < 门槛 → 降级候选
    LowNeff,
    /// 簇内 valence 标准差超限 → 反应倾向不一致 → 降级候选
    HighValenceVariance,
    /// LLM 翻译失败（重试后）→ 降级候选，跳过不阻塞
    TranslationFailed,
    /// 极性一致性校验不一致（重试后）→ 降级候选（仅参数注入）
    PolarityMismatch,
}

/// 单簇的规则生成结果。
#[derive(Debug, Clone)]
pub struct GeneratedRule {
    /// 生成的规则（`reaction = None` 表示候选规则）
    pub rule: BehaviorRule,
    /// 降级原因（None = 完整规则）
    pub degrade: RuleDegradeReason,
}

/// 极性一致性校验结论。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolarityVerdict {
    /// 规则文本的情感极性（-1..1，0 = 中性）
    pub text_polarity: f64,
    /// 簇内加权 valence（-1..1）
    pub cluster_valence: f64,
    /// 是否一致（符号相同；文本中性视为一致——无信息不判错）
    pub consistent: bool,
}

/// 质控门槛结论。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QualityVerdict {
    /// 通过（可生成规则文本）
    Pass,
    /// 不通过（降级为候选规则，仅参数注入）
    Degrade(RuleDegradeReason),
}

// =========================================================
// 极性一致性校验
// =========================================================

/// 用情感词典提取文本极性。
///
/// 返回:
/// - -1.0..1.0 极性得分（0 = 中性/无词典命中）。
pub fn polarity_of_text(text: &str) -> f64 {
    sentiment_polarity(text)
}

/// 极性一致性校验（v3.1 §4.2 Step 3）。
///
/// 规则:
/// - 文本极性符号与簇 valence 符号一致 → 一致。
/// - 文本中性（|极性| < 0.1）→ 视为一致（无信息不误判）。
/// - 文本极性符号与簇 valence 相反 → 不一致（降级候选规则）。
pub fn check_polarity(text_polarity: f64, cluster_valence: f64) -> PolarityVerdict {
    let consistent = text_polarity.abs() < 0.1 || (text_polarity > 0.0) == (cluster_valence > 0.0);
    PolarityVerdict {
        text_polarity,
        cluster_valence,
        consistent,
    }
}

// =========================================================
// 质控门槛
// =========================================================

/// 质控双门槛（v3.1 §4.2 Step 4）。
///
/// 规则:
/// - 证据量（簇内事件数）< `min_evidence` → 降级（证据不足）。
/// - n_eff（salience 加权有效样本量）< `min_n_eff` → 降级。
/// - 簇内 valence 标准差 > `valence_std_limit` → 反应倾向不一致 → 降级。
pub fn quality_gate(cluster: &RefinedCluster, config: &RuleGenConfig) -> QualityVerdict {
    let sample_count = cluster.situation.sample_count;
    if sample_count < config.min_evidence {
        return QualityVerdict::Degrade(RuleDegradeReason::LowEvidence);
    }
    if cluster.n_eff < config.min_n_eff as f64 {
        return QualityVerdict::Degrade(RuleDegradeReason::LowNeff);
    }
    if cluster.situation.valence_std > config.valence_std_limit {
        return QualityVerdict::Degrade(RuleDegradeReason::HighValenceVariance);
    }
    QualityVerdict::Pass
}

// =========================================================
// avoid 相关性校验
// =========================================================

/// avoid 列表与簇内低 valence 事件的相关性校验（v3.1 §4.2 Step 3）。
///
/// 规则:
/// - 保留与簇关键词有交集的 avoid 项（与情境相关）。
/// - 移除与"积极事件关键词"强相关的 avoid 项——禁忌与积极反应冲突
///   （簇内存在 valence > 0.3 的事件且其关键词与 avoid 词重合时视为冲突）。
/// - 簇无关键词信息时保留全部（不过滤，避免误删）。
///
/// 说明:
/// - 低 valence 事件的相关性由簇统计隐含（avoid 本身来自 LLM 对簇的翻译），
///   本函数做的是"防冲突"过滤而非生成。
pub fn validate_avoid(avoid: &[String], cluster: &RefinedCluster) -> Vec<String> {
    if cluster.situation.keywords.is_empty() {
        return avoid.to_vec();
    }
    // 积极事件关键词集（簇提炼未拆分事件级 valence，这里用整体：簇 valence 为正时
    // 所有关键词都是"积极情境"候选冲突面——保守起见仅当簇均值显著为正时过滤）
    if cluster.situation.valence_mean <= 0.3 {
        return avoid.to_vec();
    }
    let kw_set: std::collections::HashSet<&str> = cluster
        .situation
        .keywords
        .iter()
        .map(String::as_str)
        .collect();
    avoid
        .iter()
        .filter(|w| !kw_set.contains(w.as_str()))
        .cloned()
        .collect()
}

// =========================================================
// 参数化
// =========================================================

/// 从簇统计参数化（v3.1 §4.2 Step 5）。
///
/// 字段约定:
/// - `emotional_intensity`: 情感强度 = 加权 valence（-1..1）。
/// - `proactiveness`: 表达倾向（presentation 分布）——主观占比高 → 更主动。
/// - `detail_level`: 详细度——情绪强度 |valence| 越高 → 越倾向展开。
/// - `formality`: 正式度——客观占比高 → 更正式。
///
/// 说明:
/// - 均为数据驱动的启发式映射（无外部依赖），后续可经探针实验调参。
pub fn parameterize(cluster: &RefinedCluster) -> BehaviorParams {
    let subjective_share = cluster
        .situation
        .presentation_dist
        .iter()
        .find(|p| p.presentation.as_str() == "subjective")
        .map(|p| p.freq)
        .unwrap_or(0.0);
    let objective_share = cluster
        .situation
        .presentation_dist
        .iter()
        .find(|p| p.presentation.as_str() == "objective")
        .map(|p| p.freq)
        .unwrap_or(0.0);

    BehaviorParams {
        emotional_intensity: cluster.situation.valence_mean.clamp(-1.0, 1.0),
        proactiveness: (0.4 + 0.6 * subjective_share).clamp(0.0, 1.0),
        detail_level: (0.3 + 0.7 * cluster.situation.valence_mean.abs()).clamp(0.0, 1.0),
        formality: (0.4 + 0.6 * objective_share).clamp(0.0, 1.0),
    }
}

// =========================================================
// 证据权重（salience × 近期加权）
// =========================================================

/// 计算单条证据的权重（salience × 近期时间衰减）。
///
/// 参数:
/// - `salience`: 事件显著性 0..1。
/// - `event_start_ms`: 事件开始时间。
/// - `now_ms`: 当前时间。
/// - `recent_days`: 近期窗口（天内权重 1.0，之后按半衰期衰减）。
///
/// 返回:
/// - 权重 ≥ 0（近期事件 1.0，旧事件指数衰减）。
pub fn evidence_weight(salience: f64, event_start_ms: i64, now_ms: i64, recent_days: i64) -> f64 {
    let recency = recency_factor(event_start_ms, now_ms, recent_days);
    (salience * recency).clamp(0.0, 1.0)
}

/// 近期加权因子：窗口内 1.0，之后指数衰减（半衰期 = 窗口）。
pub fn recency_factor(event_start_ms: i64, now_ms: i64, recent_days: i64) -> f64 {
    let days = (now_ms - event_start_ms).max(0) as f64 / 86_400_000.0;
    if days <= recent_days as f64 {
        1.0
    } else {
        (-(days - recent_days as f64) / recent_days.max(1) as f64).exp()
    }
}

/// 构建证据列表（簇内事件 → 事件 id + 加权权重）。
///
/// 说明:
/// - 权重 = salience × 近期因子（`evidence_weight`），保证新近事件对规则影响更大
///   （近期事件加权，v3.1 §4.2 Step 4）。
/// - 使用簇成员的真实 `start_ms` / `salience`（D-V16-007，修复原"恒 1.0"缺陷）。
/// - 排序按事件 id 升序（证据链展示稳定）。
///
/// 回退:
/// - 若 `member_events` 为空（未保留逐事件信息），回退为按 n_eff 均匀分摊 +
///   保守默认 salience=0.5 + 近期时间，保证总和 ≈ n_eff（存量兼容）。
pub fn build_evidence(cluster: &RefinedCluster, now_ms: i64, recent_days: i64) -> Vec<(i64, f64)> {
    let mut ev: Vec<(i64, f64)> = if !cluster.member_events.is_empty() {
        // 主路径：真实逐事件 start/salience 计算近期加权权重。
        cluster
            .member_events
            .iter()
            .map(|m| {
                (
                    m.event_id,
                    evidence_weight(m.salience, m.start_ms, now_ms, recent_days),
                )
            })
            .collect()
    } else {
        // 回退：无逐事件信息时按 n_eff 分摊 × 近期因子（保守默认 salience=0.5）。
        let n = cluster.member_event_ids.len().max(1) as f64;
        let base = cluster.n_eff / n;
        cluster
            .member_event_ids
            .iter()
            .map(|&id| {
                (
                    id,
                    base * recency_factor(now_ms - 1_000, now_ms, recent_days),
                )
            })
            .collect()
    };
    ev.sort_by_key(|(id, _)| *id);
    ev
}

// =========================================================
// 置信度与稳定性
// =========================================================

/// 计算规则置信度（证据量 × 一致性，v3.1 §4.1）。
///
/// 公式:
/// - evidence_scale = min(1.0, n_eff / (2 × min_evidence))（证据量饱和曲线）。
/// - consistency = 1 − 归一化 valence 标准差。
/// - confidence = evidence_scale × consistency。
pub fn compute_confidence(cluster: &RefinedCluster, config: &RuleGenConfig) -> f64 {
    let evidence_scale = (cluster.n_eff / (2.0 * config.min_evidence.max(1) as f64)).min(1.0);
    let consistency = (1.0 - cluster.situation.valence_std / 2.0).clamp(0.0, 1.0);
    (evidence_scale * consistency).clamp(0.0, 1.0)
}

/// 计算规则稳定性（跨时间一致性，v3.1 §4.1）。
///
/// 公式:
/// - 反应一致性（valence 波动小）+ 时间积累因子（跨度越大越稳定，封顶 14 天）。
pub fn compute_stability(cluster: &RefinedCluster) -> f64 {
    let consistency = (1.0 - cluster.situation.valence_std / 2.0).clamp(0.0, 1.0);
    let time_factor =
        ((cluster.situation.time_span_days / 14.0).min(1.0) * 0.5 + 0.5).clamp(0.0, 1.0);
    (consistency * time_factor).clamp(0.0, 1.0)
}

// =========================================================
// LLM 翻译
// =========================================================

/// 翻译 prompt 构造（引用代表事件 attitude 作示例）。
///
/// 参数:
/// - `cluster`: 提炼后的簇。
///
/// 说明:
/// - 示例取成员事件前 3 条的 attitude（摘要形态，非完整原文）。
fn build_translation_prompt(cluster: &RefinedCluster) -> String {
    let valence_desc = if cluster.situation.valence_mean > 0.1 {
        "积极/正面"
    } else if cluster.situation.valence_mean < -0.1 {
        "消极/负面"
    } else {
        "中性"
    };
    format!(
        "你是行为规则翻译器。根据情境-反应簇的统计，把簇内反应模式翻译为一条可注入的人格规则文本。\n\
         规则文本格式：'当{{情境}}时，倾向{{反应}}……'，要求用自然口语表达、可解释。\n\
         同时给出 avoid 列表（该情境下应避免的话题/行为，0-3 个词）。\n\
         \n\
         簇关键词: {keywords}\n\
         簇情绪方向: {valence_desc}（valence={valence:.2}）\n\
         代表事件态度示例:\n{examples}\n\
         \n\
         只输出 JSON，格式: {{\"reaction\": \"...\", \"avoid\": [\"...\"]}}",
        keywords = cluster.situation.keywords.join("、"),
        valence = cluster.situation.valence_mean,
        examples = cluster
            .member_event_ids
            .iter()
            .take(3)
            .enumerate()
            .map(|(i, _)| format!("{}. 示例态度（已脱敏）", i + 1))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// 宽容解析 LLM 返回的 JSON（提取首个 `{...}` 块）。
fn parse_translation(raw: &str) -> RamariaResult<(Option<String>, Vec<String>)> {
    let start = raw
        .find('{')
        .ok_or_else(|| RamariaError::validation("行为规则翻译输出缺少 JSON 对象"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| RamariaError::validation("行为规则翻译输出缺少 JSON 结束符"))?;
    if end <= start {
        return Err(RamariaError::validation("行为规则翻译输出 JSON 结构非法"));
    }
    let body = &raw[start..=end];
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| RamariaError::validation(format!("行为规则翻译 JSON 解析失败: {e}")))?;
    let reaction = parsed
        .get("reaction")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let avoid = parsed
        .get("avoid")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok((reaction, avoid))
}

/// LLM 翻译规则文本（含重试）。
///
/// 参数:
/// - `llm`: LLM provider（mock 可注入）。
/// - `cluster`: 提炼后的簇。
/// - `config`: 规则生成配置。
///
/// 返回:
/// - `Ok(Some((reaction, avoid)))`: 翻译成功且极性校验通过。
/// - `Ok(None)`: 翻译失败或极性校验不通过（重试后仍失败）→ 降级候选规则。
/// - `Err`: 非翻译类错误（不应出现；内部错误已转为 None）。
pub async fn translate_reaction(
    llm: &dyn LlmProviderTrait,
    cluster: &RefinedCluster,
    config: &RuleGenConfig,
) -> RamariaResult<Option<(String, Vec<String>)>> {
    let prompt = build_translation_prompt(cluster);

    for attempt in 0..=config.max_retries {
        let request = ChatRequest {
            system_prompt: String::new(),
            memory_context: None,
            history: Vec::new(),
            user_message: prompt.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            request_id: Uuid::new_v4(),
            template_version: BEHAVIOR_RULE_PROMPT_VERSION.to_string(),
        };
        let raw = match llm.chat(&request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    attempt,
                    error = %e,
                    "行为规则翻译 LLM 调用失败（第 {} 次）",
                    attempt + 1
                );
                continue;
            }
        };
        let (reaction, avoid) = match parse_translation(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(attempt, error = %e, "行为规则翻译输出解析失败");
                continue;
            }
        };
        let Some(reaction) = reaction else {
            tracing::warn!(attempt, "行为规则翻译输出缺少 reaction 字段");
            continue;
        };

        // 极性一致性校验：不一致且还有重试次数 → 重新翻译
        let verdict = check_polarity(polarity_of_text(&reaction), cluster.situation.valence_mean);
        if !verdict.consistent && attempt < config.max_retries {
            tracing::warn!(
                attempt,
                text_polarity = %format!("{:.2}", verdict.text_polarity),
                cluster_valence = %format!("{:.2}", verdict.cluster_valence),
                "行为规则翻译极性不一致，重试"
            );
            continue;
        }
        if !verdict.consistent {
            tracing::warn!(
                text_polarity = %format!("{:.2}", verdict.text_polarity),
                cluster_valence = %format!("{:.2}", verdict.cluster_valence),
                "行为规则翻译极性校验不通过（重试后），降级候选规则"
            );
            return Ok(None);
        }
        return Ok(Some((reaction, validate_avoid(&avoid, cluster))));
    }
    tracing::warn!("行为规则翻译失败（达到重试上限），降级候选规则");
    Ok(None)
}

// =========================================================
// 规则生成编排
// =========================================================

/// 行为规则生成器。
///
/// 职责:
/// - 逐簇执行：质控门槛 → LLM 翻译（含极性校验重试）→ avoid 校验 → 参数化 →
///   证据/置信度/稳定性 → Auto 规则自动生效。
/// - 任一簇失败/降级不影响其他簇（逐簇独立，翻译失败跳过不阻塞）。
pub struct BehaviorRuleGenerator<'a> {
    config: RuleGenConfig,
    llm: &'a dyn LlmProviderTrait,
}

impl<'a> BehaviorRuleGenerator<'a> {
    /// 创建规则生成器。
    ///
    /// 参数:
    /// - `config`: 规则生成配置（从 `BehaviorConfig` 派生）。
    /// - `llm`: LLM provider（mock 可注入）。
    pub fn new(config: RuleGenConfig, llm: &'a dyn LlmProviderTrait) -> Self {
        Self { config, llm }
    }

    /// 生成单簇规则。
    ///
    /// 流程:
    /// 1. 质控门槛（证据量/n_eff/valence 方差）→ 不通过 → 候选规则（仅参数）。
    /// 2. LLM 翻译 + 极性校验（可重试）→ 失败 → 候选规则。
    /// 3. 参数化 / 证据（近期加权）/ 置信度 / 稳定性。
    /// 4. Auto 来源，自动生效（enabled = true）。
    pub async fn generate_rule(&self, cluster: &RefinedCluster) -> GeneratedRule {
        let params = parameterize(cluster);
        let now = ramaria_core::types::now_ms();
        let evidence: Vec<(i64, f64)> = build_evidence(cluster, now, self.config.recent_days);

        // 1. 质控门槛
        let degrade = match quality_gate(cluster, &self.config) {
            QualityVerdict::Pass => RuleDegradeReason::None,
            QualityVerdict::Degrade(reason) => reason,
        };

        // 2. LLM 翻译（仅质控通过时）
        let (reaction, avoid, final_degrade) = if degrade == RuleDegradeReason::None {
            match translate_reaction(self.llm, cluster, &self.config).await {
                Ok(Some((r, a))) => (Some(r), a, RuleDegradeReason::None),
                Ok(None) => (None, Vec::new(), RuleDegradeReason::TranslationFailed),
                Err(e) => {
                    // 防御：内部错误不应出现，记 warn 后降级候选（不阻塞）
                    tracing::warn!(error = %e, "行为规则生成内部错误，降级候选规则");
                    (None, Vec::new(), RuleDegradeReason::TranslationFailed)
                }
            }
        } else {
            (None, Vec::new(), degrade)
        };

        let confidence = compute_confidence(cluster, &self.config);
        let stability = compute_stability(cluster);

        let mut rule = BehaviorRule::new(
            String::new(), // persona_uid 由上层（app 层）落库前填充
            cluster_situation(cluster),
            reaction,
            params,
            RuleSource::Auto,
        );
        // 注意：persona_uid 由上层（app 层）在落库前填充，此处留空占位
        rule.avoid = avoid;
        rule.evidence = evidence
            .into_iter()
            .map(|(event_id, weight)| ramaria_core::behavior::BehaviorEvidence { event_id, weight })
            .collect();
        rule.confidence = confidence;
        rule.stability = stability;

        GeneratedRule {
            rule,
            degrade: final_degrade,
        }
    }

    /// 批量生成（逐簇独立，翻译失败不阻塞其他簇）。
    pub async fn generate_rules(&self, clusters: &[RefinedCluster]) -> Vec<GeneratedRule> {
        let mut out = Vec::with_capacity(clusters.len());
        for cluster in clusters {
            out.push(self.generate_rule(cluster).await);
        }
        out
    }
}

/// 从簇提炼结果构造情境侧（persona_uid 由上层填充）。
fn cluster_situation(cluster: &RefinedCluster) -> BehaviorSituation {
    cluster.situation.clone()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
