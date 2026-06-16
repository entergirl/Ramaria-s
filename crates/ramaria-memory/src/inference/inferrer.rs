//! rust/crates/ramaria-memory/src/inference/inferrer.rs - 三步 LLM 结构化性格推断
//!
//! 设计特点:
//! - Step 1: 逐分类个性模式提取 — 统计指标 + 态度聚类结果 → 分类级性格信号
//! - Step 2: 跨分类一致性比较 — 识别底色/主色调/点缀候选
//! - Step 3: 合成三层结构化性格画像 → PersonalityTrait 记录
//! - 输出后处理: 语义匹配/新增/废弃/差量更新（简化版：按 trait_label 去重）
//! - Mock 推断: 基于 StatsSummary 生成确定性人格标签，支持无 LLM 测试
//! - 依赖注入: 通过 LlmProvider trait 解耦具体 LLM 实现
//! - 信息缺口标记: n_eff < 5 的分类附降低确信度声明

use ramaria_core::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus};

use crate::inference::stats::{
    CategoryStats, CrossCategoryMetrics, RepresentativeEvent, StatsSummary,
};

// =========================================================
// 配置类型
// =========================================================

/// 推断器配置。
#[derive(Debug, Clone)]
pub struct InferrerConfig {
    /// LLM 生成温度
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
    /// 小样本分类的 n_eff 阈值（低于此值附降低确信度声明）
    pub low_evidence_threshold: f64,
    /// 每步最多 token 数
    pub step_max_tokens: u32,
}

impl Default for InferrerConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 2048,
            low_evidence_threshold: 5.0,
            step_max_tokens: 2048,
        }
    }
}

// =========================================================
// 结构化中间输出类型
// =========================================================

/// Step 1 输出：单个分类的性格信号。
#[derive(Debug, Clone)]
pub struct CategorySignal {
    /// 分类名
    pub category: String,
    /// 性格信号标签（如"尽责""社交回避"）
    pub signal_label: String,
    /// 支持性证据引用（统计指标摘要）
    pub evidence_citation: String,
    /// 跨领域稳定性预判（"stable"/"contextual"/"uncertain"）
    pub stability_judgment: String,
    /// 有效样本量是否充足
    pub sufficient_evidence: bool,
}

/// Step 2 输出：跨分类一致性分析。
#[derive(Debug, Clone)]
pub struct ConsistencyAnalysis {
    /// 底色候选（跨分类一致的信号标签列表）
    pub base_candidates: Vec<String>,
    /// 主色调候选（最高权重分类的信号标签）
    pub primary_candidates: Vec<String>,
    /// 点缀候选（条件性信号标签列表）
    pub accent_candidates: Vec<String>,
    /// 分析说明
    pub notes: String,
}

/// Step 3 输出：最终性格画像（在解析为 Vec<PersonalityTrait> 前的中间形态）。
#[derive(Debug, Clone)]
pub struct InferredTrait {
    pub layer: String,
    pub trait_label: String,
    pub meaning: String,
    pub not_meaning: Option<String>,
    pub trigger: Option<String>,
    pub suppress: Option<String>,
    pub related: Option<String>,
    pub seq: i32,
}

/// 完整推断结果。
#[derive(Debug, Clone)]
pub struct InferenceResult {
    /// Step 1 逐分类信号
    pub category_signals: Vec<CategorySignal>,
    /// Step 2 一致性分析
    pub consistency: ConsistencyAnalysis,
    /// Step 3 推断的性格标签
    pub traits: Vec<PersonalityTrait>,
}

// =========================================================
// Prompt 构建
// =========================================================

/// 格式化分类统计为 LLM 可读文本。
fn format_category_stats(cat: &CategoryStats, low_threshold: f64) -> String {
    let warning = if cat.n_eff < low_threshold {
        format!(
            "⚠️ 警告：此分类有效样本量仅 {:.1}（< {:.0}），以下统计可靠性有限，请降低推断确信度。\n",
            cat.n_eff, low_threshold
        )
    } else {
        String::new()
    };

    format!(
        "{}\
分类: {}\n\
  - 原始事件数: {}\n\
  - 有效样本量 (n_eff): {:.1}\n\
  - 加权效价均值: {:.2} | 标准差: {:.2} | 正面占比: {:.1}%\n\
  - 加权分享意愿均值: {:.2} | 标准差: {:.2}\n\
  - 陈述方式: 客观 {:.1}% | 主观 {:.1}% | 混合 {:.1}%\n\
  - 组权重: {:.1}%\n",
        warning,
        cat.category,
        cat.event_count,
        cat.n_eff,
        cat.valence_mean,
        cat.valence_std,
        cat.valence_positive_ratio * 100.0,
        cat.share_mean,
        cat.share_std,
        cat.presentation_objective_ratio * 100.0,
        cat.presentation_subjective_ratio * 100.0,
        cat.presentation_mixed_ratio * 100.0,
        cat.group_weight * 100.0,
    )
}

/// 格式化跨分类指标为文本。
fn format_cross_category(metrics: &CrossCategoryMetrics) -> String {
    format!(
        "跨分类高阶指标:\n\
  - 情绪稳定性 (全局 valence 加权标准差): {:.2}（越小越平稳）\n\
  - 叙事一致性 (跨分类 presentation 分布相似度): {:.2}（1.0 表示完全一致）\n\
  - 态度矛盾检测: {}（≥1 表示可能存在跨分类内在矛盾）\n\
  - 社交开放性: share 偏度 {:.2}（>0=右偏）| 峰度 {:.2}（>0=尖峰）\n",
        metrics.emotional_stability,
        metrics.narrative_consistency,
        if metrics.attitude_contradiction_count > 0 {
            "是"
        } else {
            "否"
        },
        metrics.share_skewness,
        metrics.share_kurtosis,
    )
}

/// 格式化代表性事件为文本。
fn format_representative_events(events: &[RepresentativeEvent], max_display: usize) -> String {
    if events.is_empty() {
        return "（无代表性事件）\n".to_string();
    }
    let mut s = String::from("代表性事件:\n");
    for (i, ev) in events.iter().take(max_display).enumerate() {
        s.push_str(&format!("  {}. [{}] {}\n", i + 1, ev.category, ev.title));
        s.push_str(&format!("     摘要: {}\n", ev.summary));
        if let Some(ref att) = ev.attitude {
            s.push_str(&format!("     态度: {}\n", att));
        }
        s.push_str(&format!(
            "     效价: {:.2} | 显著性: {:.2}\n",
            ev.valence, ev.salience
        ));
    }
    s
}

/// 构建 Step 1 prompt：逐分类个性模式提取。
pub fn build_step1_prompt(stats: &StatsSummary, config: &InferrerConfig) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "你是一位性格心理分析师。基于以下统计数据和事件摘要，对用户在每个生活领域的性格表现进行分析。\n\n",
    );
    prompt.push_str("## 分类统计\n\n");

    for cat in &stats.categories {
        prompt.push_str(&format_category_stats(cat, config.low_evidence_threshold));
        prompt.push('\n');
    }

    let max_events = 5;
    prompt.push_str(&format_representative_events(
        &stats.representative_events,
        max_events,
    ));
    prompt.push('\n');

    prompt.push_str(&format!(
        "## 任务\n\
对上述每个分类，提炼该分类下呈现的性格信号。输出 JSON 对象，键为分类名，值为对象包含:\n\
- signal_label: 性格信号标签（2-4字中文词）\n\
- evidence_citation: 引用统计指标作为证据\n\
- stability_judgment: \"stable\"/\"contextual\"/\"uncertain\"\n\
- sufficient_evidence: true/false\n\n\
只输出 JSON，不要任何其他文字。\n\n\
约束:\n\
- 只基于提供的数据推断，不引入对人类一般性的先验知识\n\
- n_eff < {:.0} 的分类视为 uncertain\n\
- 如果数据不足以支持任何推断，signal_label 填 \"insufficient_data\"\n",
        config.low_evidence_threshold
    ));

    prompt
}

/// 构建 Step 2 prompt：跨分类一致性比较。
pub fn build_step2_prompt(
    category_signals: &[CategorySignal],
    metrics: &CrossCategoryMetrics,
    categories: &[CategoryStats],
) -> String {
    let mut prompt = String::new();
    prompt.push_str("你是一位性格心理分析师。基于以下逐分类性格信号和跨分类统计指标，识别跨领域一致性模式。\n\n");

    prompt.push_str("## 逐分类信号\n\n");
    for sig in category_signals {
        prompt.push_str(&format!(
            "分类「{}」: 信号={} | 稳定性判定={} | 证据充足={}\n  证据: {}\n\n",
            sig.category,
            sig.signal_label,
            sig.stability_judgment,
            sig.sufficient_evidence,
            sig.evidence_citation,
        ));
    }

    prompt.push_str(&format_cross_category(metrics));
    prompt.push('\n');

    // 附加分类权重排名
    prompt.push_str("## 分类权重排名\n");
    for cat in categories.iter().take(5) {
        prompt.push_str(&format!(
            "  {} - 权重 {:.1}% | n_eff={:.1}\n",
            cat.category, cat.group_weight, cat.n_eff
        ));
    }
    prompt.push('\n');

    prompt.push_str(
        "## 任务\n\
分析哪些性格信号可以归入三层性格模型:\n\
- base (底色): 跨情境稳定的深层性格——需在≥2个分类中一致出现\n\
- primary (主色调): 最高权重分类的最突出信号——日常最明显\n\
- accent (点缀): 仅在特定分类或条件下出现的信号——包含矛盾检测来源\n\n\
输出 JSON:\n\
{\n\
  \"base_candidates\": [\"标签1\", \"标签2\"],\n\
  \"primary_candidates\": [\"标签1\"],\n\
  \"accent_candidates\": [\"标签1\", \"标签2\"],\n\
  \"notes\": \"简要分析说明\"\n\
}\n\n\
只输出 JSON。\n\
注意: 底色基于叙事一致性指标和跨组一致性；主色调基于最高权重分类；点缀基于矛盾检测和条件性模式。\n",
    );

    prompt
}

/// 构建 Step 3 prompt：合成结构化性格画像。
pub fn build_step3_prompt(
    analysis: &ConsistencyAnalysis,
    _category_signals: &[CategorySignal],
    _stats: &StatsSummary,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("你是一位性格心理分析师。基于以下分层分析结果，生成最终结构化性格画像。\n\n");

    prompt.push_str("## 底色候选\n");
    for label in &analysis.base_candidates {
        prompt.push_str(&format!("  - {}\n", label));
    }
    prompt.push_str("\n## 主色调候选\n");
    for label in &analysis.primary_candidates {
        prompt.push_str(&format!("  - {}\n", label));
    }
    prompt.push_str("\n## 点缀候选\n");
    for label in &analysis.accent_candidates {
        prompt.push_str(&format!("  - {}\n", label));
    }
    prompt.push('\n');

    prompt.push_str(
        "## 任务\n\
为每层的每个标签生成完整的 trait 记录。输出 JSON 数组，每元素包含:\n\
- layer: \"base\" / \"primary\" / \"accent\"\n\
- trait_label: 标签词（2-4字中文）\n\
- meaning: 在此人身上的具体含义（1-2句话，具体描述而非泛泛而谈）\n\
- not_meaning: 反向界定——它不是什么（如果有的话，null 表示无）\n\
- trigger: 浮现条件（accent 必填，其他可选，null 表示不特定）\n\
- suppress: 抑制条件\n\
- related: 与其他性格标签的关系\n\
- seq: 层内排序（0-based）\n\n\
约束:\n\
- 每条 trait 引用至少一个统计指标作为 evidence_citation\n\
- not_meaning 用于防止误解（如\"幽默\"的 not_meaning 可以是\"并非轻浮\"）\n\
- 底色最多3条，主色调最多2条，点缀最多4条\n\
- 只输出 JSON 数组，不要任何其他文字\n\n\
格式示例:\n\
[\n  {{\"layer\":\"primary\",\"trait_label\":\"温和\",\"meaning\":\"xxx\",\"not_meaning\":null,\"trigger\":null,\"suppress\":null,\"related\":null,\"seq\":0}},\n  ...\n]\n"
    );

    prompt
}

// =========================================================
// Mock 推断（无 LLM 依赖的测试支持）
// =========================================================

/// 基于统计摘要的简易规则推断（Mock LLM 替代）。
///
/// 策略:
/// - 不调用真实 LLM，直接从统计指标推演出 PersonalityTrait 记录。
/// - 用于测试和 CI 环境，确保全管线可验证。
/// - 推断逻辑透明可审计。
///
/// 参数:
/// - `stats`: 统计摘要。
/// - `persona_uid`: 目标人格标识。
///
/// 返回:
/// - 推断结果（含 Step1/2/3 的完整输出）。
pub fn mock_infer(stats: &StatsSummary, persona_uid: &str) -> InferenceResult {
    let mut category_signals = Vec::new();
    let mut trait_seq = 0i32;

    // Step 1: 逐分类生成信号
    for cat in &stats.categories {
        let sufficient = cat.n_eff >= 5.0;

        // 优先使用最显著的信号维度
        let (signal_label, stability) = if cat.valence_mean > 0.4 && cat.valence_std < 0.4 {
            (format!("{}-积极稳定", cat.category), "stable")
        } else if cat.valence_mean < -0.3 {
            (format!("{}-消极回避", cat.category), "contextual")
        } else if cat.share_mean > 0.7 {
            (format!("{}-高分享", cat.category), "contextual")
        } else if cat.share_mean < 0.3 {
            (format!("{}-低分享", cat.category), "contextual")
        } else if cat.presentation_subjective_ratio > 0.6 {
            (format!("{}-主观表达", cat.category), "contextual")
        } else if cat.presentation_objective_ratio > 0.6 {
            (format!("{}-客观理性", cat.category), "contextual")
        } else if cat.valence_std > 0.6 {
            (format!("{}-情绪波动", cat.category), "contextual")
        } else {
            (format!("{}-中性投入", cat.category), "contextual")
        };

        category_signals.push(CategorySignal {
            category: cat.category.clone(),
            signal_label,
            evidence_citation: format!(
                "valence_mean={:.2}, share_mean={:.2}, n_eff={:.1}",
                cat.valence_mean, cat.share_mean, cat.n_eff
            ),
            stability_judgment: stability.to_string(),
            sufficient_evidence: sufficient,
        });
    }

    // Step 2: 跨分类分析
    let mut base_candidates = Vec::new();
    let mut primary_candidates = Vec::new();
    let mut accent_candidates = Vec::new();

    // 在 ≥2 个分类中出现且稳定性为 "stable" 的信号 → 底色候选
    let mut signal_freq: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for sig in &category_signals {
        *signal_freq.entry(sig.signal_label.clone()).or_default() += 1;
    }
    for (label, freq) in &signal_freq {
        if *freq >= 2 {
            base_candidates.push(label.clone());
        }
    }

    // 最高权重分类 → 主色调
    if let Some(top) = stats.categories.first()
        && let Some(sig) = category_signals.iter().find(|s| s.category == top.category)
    {
        primary_candidates.push(sig.signal_label.clone());
    }

    // 矛盾检测 → 点缀
    if stats.cross_category.attitude_contradiction_count > 0 {
        accent_candidates.push("内在矛盾型".to_string());
    }
    // n_eff 很小的分类 → 点缀
    for sig in &category_signals {
        if !sig.sufficient_evidence
            && sig.signal_label != "insufficient_data"
            && !accent_candidates.contains(&sig.signal_label)
        {
            accent_candidates.push(sig.signal_label.clone());
        }
    }

    let consistency = ConsistencyAnalysis {
        base_candidates,
        primary_candidates,
        accent_candidates,
        notes: "Mock 推断——基于统计阈值的规则推演。真实环境应替换为 LLM 推断。".to_string(),
    };

    // Step 3: 生成 PersonalityTrait
    let mut traits = Vec::new();
    let now = ramaria_core::types::now_ms();

    // 底色
    for (i, label) in consistency.base_candidates.iter().enumerate().take(3) {
        traits.push(PersonalityTrait {
            id: 0,
            persona_uid: persona_uid.to_string(),
            layer: TraitLayer::Base,
            trait_label: label.clone(),
            meaning: format!("在多个生活领域表现出'{}'模式", label),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: i as i32,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence: 0.5,
            evidence: 1.0,
            consistency: 0.5,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
        trait_seq = i as i32 + 1;
    }

    // 主色调
    for (i, label) in consistency.primary_candidates.iter().enumerate().take(2) {
        traits.push(PersonalityTrait {
            id: 0,
            persona_uid: persona_uid.to_string(),
            layer: TraitLayer::Primary,
            trait_label: label.clone(),
            meaning: format!("最突出地表现为'{}'", label),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: trait_seq + i as i32,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence: 0.5,
            evidence: 1.0,
            consistency: 0.5,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
    }
    trait_seq += consistency.primary_candidates.len().min(2) as i32;

    // 点缀
    for (i, label) in consistency.accent_candidates.iter().enumerate().take(4) {
        traits.push(PersonalityTrait {
            id: 0,
            persona_uid: persona_uid.to_string(),
            layer: TraitLayer::Accent,
            trait_label: label.clone(),
            meaning: format!("在特定条件下浮现'{}'特质", label),
            not_meaning: None,
            trigger: Some("特定领域或低样本量条件下".to_string()),
            suppress: None,
            related: None,
            seq: trait_seq + i as i32,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence: 0.3, // 点缀初始置信度较低
            evidence: 0.5,
            consistency: 0.3,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
    }

    InferenceResult {
        category_signals,
        consistency,
        traits,
    }
}

// =========================================================
// 输出后处理（T-INF-011）
// =========================================================

/// 推断后处理的差异类型。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DiffAction {
    /// 新增——旧画像中不存在
    Add,
    /// 更新——语义等价但含义变化
    Update,
    /// 废弃——旧 accent 不再有事件支撑
    Deprecate,
    /// 保留——无变化
    Keep,
}

/// 单条 trait 的差异记录。
#[derive(Debug, Clone)]
pub struct TraitDiff {
    /// 差异动作
    pub action: DiffAction,
    /// 新推断的 trait（Add/Update 时有值）
    pub new_trait: Option<PersonalityTrait>,
    /// 被替换的旧 trait ID（Update/Deprecate/Keep 时有值）
    pub old_trait_id: Option<i64>,
    /// 旧 trait 标签（供日志）
    pub old_label: Option<String>,
}

/// 推断后处理结果。
#[derive(Debug, Clone)]
pub struct PostProcessResult {
    /// 需要新增的 trait
    pub to_add: Vec<PersonalityTrait>,
    /// 需要更新的 trait（附带旧 ID）
    pub to_update: Vec<(i64, PersonalityTrait)>,
    /// 需要标记为废弃的 trait ID 列表
    pub to_deprecate: Vec<i64>,
    /// 差异详情
    pub diffs: Vec<TraitDiff>,
}

/// 将新推断的 trait 与已有 trait 做差异比较。
///
/// 策略（简化版——基于 trait_label 精确匹配）:
/// - 新 trait 的 label 在旧 trait 中找不到 → Add
/// - 新 trait 的 label 与旧 trait 匹配但 layer 不同 → Update
/// - 旧 accent trait 在新推断中消失 → Deprecate
/// - 其他 → Keep
///
/// 注意: 接入 embedding 后应替换为语义匹配。
///
/// 参数:
/// - `new_traits`: 新推断的 trait 列表。
/// - `old_traits`: 数据库中已有的 trait 列表。
/// - `persona_uid`: 目标人格标识。
///
/// 返回:
/// - PostProcessResult。
pub fn compute_trait_diff(
    new_traits: &[PersonalityTrait],
    old_traits: &[PersonalityTrait],
    _persona_uid: &str,
) -> PostProcessResult {
    let mut to_add = Vec::new();
    let mut to_update = Vec::new();
    let mut to_deprecate = Vec::new();
    let mut diffs = Vec::new();

    // 构建旧 trait 的 label→(id, trait) 映射
    let old_map: std::collections::HashMap<String, (i64, &PersonalityTrait)> = old_traits
        .iter()
        .filter(|t| t.status == TraitStatus::Active)
        .map(|t| (t.trait_label.clone(), (t.id, t)))
        .collect();

    let mut old_matched: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // 遍历新 trait
    for new_t in new_traits {
        if let Some(&(old_id, old_t)) = old_map.get(&new_t.trait_label) {
            old_matched.insert(old_id);
            if new_t.layer != old_t.layer || new_t.meaning != old_t.meaning {
                // layer 或 meaning 变化 → Update
                to_update.push((old_id, new_t.clone()));
                diffs.push(TraitDiff {
                    action: DiffAction::Update,
                    new_trait: Some(new_t.clone()),
                    old_trait_id: Some(old_id),
                    old_label: Some(old_t.trait_label.clone()),
                });
            } else {
                // 无变化 → Keep
                diffs.push(TraitDiff {
                    action: DiffAction::Keep,
                    new_trait: None,
                    old_trait_id: Some(old_id),
                    old_label: Some(old_t.trait_label.clone()),
                });
            }
        } else {
            // 新 trait → Add
            to_add.push(new_t.clone());
            diffs.push(TraitDiff {
                action: DiffAction::Add,
                new_trait: Some(new_t.clone()),
                old_trait_id: None,
                old_label: None,
            });
        }
    }

    // 未被匹配的旧 accent trait → 标记废弃
    for old_t in old_traits {
        if old_t.status == TraitStatus::Active
            && old_t.layer == TraitLayer::Accent
            && !old_matched.contains(&old_t.id)
        {
            to_deprecate.push(old_t.id);
            diffs.push(TraitDiff {
                action: DiffAction::Deprecate,
                new_trait: None,
                old_trait_id: Some(old_t.id),
                old_label: Some(old_t.trait_label.clone()),
            });
        }
    }

    PostProcessResult {
        to_add,
        to_update,
        to_deprecate,
        diffs,
    }
}

/// 对推断结果执行后处理。
///
/// 参数:
/// - `result`: 推断结果。
/// - `old_traits`: 已存在的 trait 列表。
/// - `persona_uid`: 目标人格标识。
///
/// 返回:
/// - PostProcessResult。
pub fn post_process_inference(
    result: &InferenceResult,
    old_traits: &[PersonalityTrait],
    persona_uid: &str,
) -> PostProcessResult {
    compute_trait_diff(&result.traits, old_traits, persona_uid)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_stats() -> StatsSummary {
        StatsSummary {
            total_events_in: 10,
            total_events_filtered: 8,
            category_count: 2,
            categories: vec![
                CategoryStats {
                    category: "工作".into(),
                    event_count: 5,
                    n_eff: 3.5,
                    valence_mean: 0.6,
                    valence_std: 0.3,
                    valence_positive_ratio: 0.8,
                    share_mean: 0.7,
                    share_std: 0.2,
                    presentation_objective_ratio: 0.5,
                    presentation_subjective_ratio: 0.3,
                    presentation_mixed_ratio: 0.2,
                    group_weight: 0.6,
                },
                CategoryStats {
                    category: "社交".into(),
                    event_count: 3,
                    n_eff: 1.8,
                    valence_mean: -0.2,
                    valence_std: 0.5,
                    valence_positive_ratio: 0.4,
                    share_mean: 0.8,
                    share_std: 0.1,
                    presentation_objective_ratio: 0.2,
                    presentation_subjective_ratio: 0.6,
                    presentation_mixed_ratio: 0.2,
                    group_weight: 0.4,
                },
            ],
            cross_category: CrossCategoryMetrics {
                emotional_stability: 0.45,
                narrative_consistency: 0.7,
                attitude_contradiction_count: 0,
                share_skewness: 0.1,
                share_kurtosis: -0.5,
            },
            representative_events: vec![RepresentativeEvent {
                title: "项目验收".into(),
                summary: "顺利完成项目验收".into(),
                attitude: Some("对成果感到满意".into()),
                valence: 0.8,
                salience: 0.9,
                category: "工作".into(),
            }],
        }
    }

    // ---- Prompt 构建 ----

    #[test]
    fn build_step1_prompt_is_valid() {
        let stats = make_test_stats();
        let config = InferrerConfig::default();
        let prompt = build_step1_prompt(&stats, &config);
        assert!(prompt.contains("工作"));
        assert!(prompt.contains("社交"));
        assert!(prompt.contains("n_eff"));
        assert!(prompt.contains("分类统计"));
    }

    #[test]
    fn build_step2_prompt_is_valid() {
        let stats = make_test_stats();
        let _config = InferrerConfig::default();
        let result = mock_infer(&stats, "user-0001");
        let prompt = build_step2_prompt(
            &result.category_signals,
            &stats.cross_category,
            &stats.categories,
        );
        assert!(prompt.contains("base_candidates"));
        assert!(prompt.contains("跨领域一致性"));
    }

    #[test]
    fn build_step3_prompt_is_valid() {
        let stats = make_test_stats();
        let result = mock_infer(&stats, "user-0001");
        let prompt = build_step3_prompt(&result.consistency, &result.category_signals, &stats);
        assert!(prompt.contains("layer"));
        assert!(prompt.contains("trait_label"));
    }

    // ---- Mock 推断 ----

    #[test]
    fn mock_infer_generates_signals() {
        let stats = make_test_stats();
        let result = mock_infer(&stats, "user-0001");
        assert_eq!(result.category_signals.len(), 2);
        // 工作分类 n_eff=3.5 < 5，应标记 insufficient_evidence
        let work_signal = result
            .category_signals
            .iter()
            .find(|s| s.category == "工作")
            .unwrap();
        assert!(!work_signal.sufficient_evidence);
        assert!(!work_signal.signal_label.is_empty());
    }

    #[test]
    fn mock_infer_generates_traits() {
        let stats = make_test_stats();
        let result = mock_infer(&stats, "user-0001");
        assert!(!result.traits.is_empty(), "应至少生成 traits");
        // 所有 trait 应有 persona_uid
        for t in &result.traits {
            assert_eq!(t.persona_uid, "user-0001");
        }
    }

    #[test]
    fn mock_infer_empty_stats() {
        let stats = StatsSummary {
            total_events_in: 0,
            total_events_filtered: 0,
            category_count: 0,
            categories: vec![],
            cross_category: CrossCategoryMetrics {
                emotional_stability: 0.0,
                narrative_consistency: 1.0,
                attitude_contradiction_count: 0,
                share_skewness: 0.0,
                share_kurtosis: 0.0,
            },
            representative_events: vec![],
        };
        let result = mock_infer(&stats, "user-0001");
        assert!(result.category_signals.is_empty());
        assert!(result.traits.is_empty());
    }

    // ---- 后处理（差异计算） ----

    #[test]
    fn compute_diff_new_traits() {
        let stats = make_test_stats();
        let result = mock_infer(&stats, "user-0001");
        let post = post_process_inference(&result, &[], "user-0001");
        assert!(!post.to_add.is_empty(), "旧画像为空时所有 trait 应新增");
        assert!(post.to_update.is_empty());
        assert!(post.to_deprecate.is_empty());
    }

    #[test]
    fn compute_diff_matching_traits() {
        let stats = make_test_stats();
        let result = mock_infer(&stats, "user-0001");
        let old = result.traits.clone(); // 模拟已有相同 traits
        let post = post_process_inference(&result, &old, "user-0001");
        // 所有 trait 应被匹配（无 Add），且 Keep ≥ 旧 trait 数（因可能有多层同名 label）
        let add_count = post
            .diffs
            .iter()
            .filter(|d| d.action == DiffAction::Add)
            .count();
        assert_eq!(add_count, 0, "相同 traits 对比不应产生 Add");
        let keep_count = post
            .diffs
            .iter()
            .filter(|d| d.action == DiffAction::Keep)
            .count();
        assert!(keep_count > 0, "应有至少一个 Keep");
    }

    #[test]
    fn compute_diff_accent_deprecation() {
        // 创建一个旧 accent trait，新推断中不包含
        let now = ramaria_core::types::now_ms();
        let old_accent = PersonalityTrait {
            id: 99,
            persona_uid: "user-0001".into(),
            layer: TraitLayer::Accent,
            trait_label: "过时标签".into(),
            meaning: "旧意义".into(),
            not_meaning: None,
            trigger: Some("旧条件".into()),
            suppress: None,
            related: None,
            seq: 0,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence: 0.3,
            evidence: 0.5,
            consistency: 0.3,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        };

        let stats = make_test_stats();
        let result = mock_infer(&stats, "user-0001");
        let post = post_process_inference(&result, &[old_accent], "user-0001");
        // 旧 accent 应被标记废弃
        assert!(post.to_deprecate.contains(&99));
    }

    // ---- InferrerConfig ----

    #[test]
    fn inferrer_config_defaults() {
        let config = InferrerConfig::default();
        assert_eq!(config.low_evidence_threshold, 5.0);
        assert_eq!(config.temperature, 0.3);
    }
}
