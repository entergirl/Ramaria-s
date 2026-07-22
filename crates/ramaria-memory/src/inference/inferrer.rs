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
    CategoryStats, CrossCategoryMetrics, MotiveStats, RepresentativeEvent, StatsSummary,
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

// ---- v1.3 配置传播修复：从 ramaria-core 的可序列化配置创建 ----

impl From<ramaria_core::config::InferrerConf> for InferrerConfig {
    fn from(conf: ramaria_core::config::InferrerConf) -> Self {
        Self {
            temperature: conf.temperature,
            max_tokens: conf.max_tokens,
            low_evidence_threshold: conf.low_evidence_threshold,
            step_max_tokens: conf.step_max_tokens,
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
    /// LLM 推断的置信度（0.0..1.0）。
    /// 从 LLM JSON 输出中解析，不再统一硬编码 0.5。
    /// 若 LLM 未提供此字段，默认回退为 None（由后处理校准）。
    pub confidence: Option<f64>,
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
///
/// 说明:
/// - n_eff 不再等于原始事件数，而是校准权重之和：w_i = salience_cal × confidence_factor × situation_multiplier × source_support。
/// - tentative 事件（0.45 ≤ confidence < 0.6）以半权重参与统计。
/// - 分类级统计已应用分层经验贝叶斯收缩（base/primary 使用全局先验，accent 使用领域先验）。
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
  - 原始事件数: {} | 有效样本量 n_eff (校准权重之和): {:.1}\n\
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

/// 格式化动机统计为 LLM 可读文本。
///
/// 策略:
/// - 按 n_eff 降序展示动机标签及其统计指标。
/// - 仅展示 n_eff ≥ 1.0 的动机（过滤噪声）。
/// - 若结果为空或仅包含无效条目，返回空字符串。
///
/// 参数:
/// - `motive_stats`: 动机统计列表。
/// - `max_display`: 最多展示的动机标签数。
///
/// 返回:
/// - 格式化的动机统计文本段落。无有效数据时为空字符串。
pub fn format_motive_stats(motive_stats: &[MotiveStats], max_display: usize) -> String {
    let valid: Vec<&MotiveStats> = motive_stats
        .iter()
        .filter(|m| m.n_eff >= 1.0 && m.event_count > 0)
        .collect();

    if valid.is_empty() {
        return String::new();
    }

    let mut s = String::from("## 动机维度统计\n\n");
    s.push_str(&format!(
        "以下统计基于动机标签（Fundamental Motives Framework）对事件的二次分组聚合。\n\
共有 {} 个动机标签（n_eff≥1.0）。各动机标签下的统计指标有助于评估用户行为的动机驱动力。\n\n",
        valid.len()
    ));

    for m in valid.iter().take(max_display) {
        s.push_str(&format!(
            "动机「{}」:\n\
  - 事件数: {} | 有效样本量 (n_eff): {:.1}\n\
  - 加权效价均值: {:.2} | 标准差: {:.2} | 正面占比: {:.1}%\n\
  - 加权分享意愿均值: {:.2} | 标准差: {:.2}\n\
  - 陈述方式: 客观 {:.1}% | 主观 {:.1}% | 混合 {:.1}%\n\
  - 平均显著性: {:.2}\n\n",
            m.motive,
            m.event_count,
            m.n_eff,
            m.valence_mean,
            m.valence_std,
            m.valence_positive_ratio * 100.0,
            m.share_mean,
            m.share_std,
            m.presentation_objective_ratio * 100.0,
            m.presentation_subjective_ratio * 100.0,
            m.presentation_mixed_ratio * 100.0,
            m.avg_salience,
        ));
    }

    // 如有被截断的条目，备注说明
    if valid.len() > max_display {
        s.push_str(&format!(
            "（仅展示前 {} 个动机标签，共 {} 个。剩余动机未在 Prompt 中展示但仍参与后台统计。）\n\n",
            max_display,
            valid.len()
        ));
    }

    s
}

/// 构建 Step 1 prompt：逐分类个性模式提取。
///
/// v2.0 重构 (CRAFT 框架):
/// - Context: 统计方法说明 + 因果链特征 + 动机维度统计。
/// - Role: 田野心理学家视角，严格区分"话题领域"和"性格特征"。
/// - Action: 逐分类提炼性格信号。
/// - Format: 严格 JSON（键为分类名，值为信号对象）。
/// - Target: signal_label 必须是性格词不是话题词，evidence 具体可追溯。
///
/// 参数:
/// - `stats`: Phase A 统计摘要。
/// - `config`: 推断器配置。
/// - `causal_features_text`: 可选的因果链特征文本（由 A8 模块生成）。为 None 或空字符串时跳过。
/// - `motive_stats_text`: 可选的动机维度统计文本（由 E 模块生成）。为 None 或空字符串时跳过。
pub fn build_step1_prompt(
    stats: &StatsSummary,
    config: &InferrerConfig,
    causal_features_text: Option<&str>,
    motive_stats_text: Option<&str>,
) -> String {
    let mut prompt = String::new();

    // ---- CRAFT: Context（背景） ----
    prompt.push_str("# Context（背景）\n");
    prompt.push_str("你是一位性格心理分析师。基于统计数据和事件摘要，对用户在每个生活领域的性格表现进行分析。\n\n");
    prompt.push_str("本次统计使用校准权重链：w = salience_cal × confidence_factor × situation_multiplier × source_support。\n");
    prompt.push_str("- n_eff（有效样本量）是校准权重之和，不是原始事件数。高权重事件贡献更大。\n");
    prompt.push_str(
        "- tentative 事件（置信度 0.45–0.6）以半权重参与统计，discarded 事件（<0.45）已排除。\n",
    );
    prompt.push_str("- 分类级统计已应用分层经验贝叶斯收缩：Base/Primary 层使用全局先验，Accent 层使用领域先验。\n\n");

    // ---- 因果链特征（A8） ----
    if let Some(causal_text) = causal_features_text
        && !causal_text.is_empty()
    {
        prompt.push_str(causal_text);
    }

    // ---- 动机维度统计（E 模块） ----
    if let Some(motive_text) = motive_stats_text
        && !motive_text.is_empty()
    {
        prompt.push_str(motive_text);
    }

    // ---- CRAFT: Role（角色定位） ----
    prompt.push_str("# Role（角色定位）\n");
    prompt.push_str("你像一位田野心理学家：基于客观数据做谨慎推断，不引入对人类一般性的先验知识。");
    prompt.push_str(
        "你严格区分「话题领域」和「性格特征」——「工作」「社交」「家庭」是话题（数据的分组维度），",
    );
    prompt.push_str("而「尽责」「外向」「焦虑」才是性格（你要输出的东西）。\n\n");

    // ---- CRAFT: Action + Format（任务 + 输出格式） ----
    prompt.push_str("# Action（执行任务）\n");
    prompt.push_str("对每个分类，提炼该分类下呈现的性格信号。\n\n");

    prompt.push_str("# Format（输出格式）\n");
    prompt.push_str(
        "你的整个回复必须是一个裸 JSON 对象，以 { 开头、以 } 结尾。键为分类名，值为信号对象：\n\n",
    );
    prompt.push_str("{\n");
    prompt.push_str("  \"分类名1\": {\n");
    prompt.push_str("    \"signal_label\": \"2-4字中文性格特征词\",\n");
    prompt.push_str("    \"evidence_citation\": \"引用具体统计指标作为证据\",\n");
    prompt.push_str("    \"stability_judgment\": \"stable\",\n");
    prompt.push_str("    \"sufficient_evidence\": true\n");
    prompt.push_str("  },\n");
    prompt.push_str("  \"分类名2\": {\n");
    prompt.push_str("    \"signal_label\": \"insufficient_data\",\n");
    prompt.push_str("    \"evidence_citation\": \"n_eff 仅 1.2，样本量不足，无法可靠推断\",\n");
    prompt.push_str("    \"stability_judgment\": \"uncertain\",\n");
    prompt.push_str("    \"sufficient_evidence\": false\n");
    prompt.push_str("  }\n");
    prompt.push_str("}\n\n");

    prompt.push_str("字段约束：\n");
    prompt.push_str("- `signal_label`：必须是性格特征词（如「尽责」「社交回避」「情绪稳定」），");
    prompt.push_str("**不能是话题名**（如「沉浸体验」「系统逻辑」「AI模拟」）。数据不足以支持推断时填 `\"insufficient_data\"`。\n");
    prompt.push_str("- `evidence_citation`：引用具体统计指标（如「n_eff=8.5，valence 均值 0.8，正面占比 90%，主观陈述占比 70%」）。");
    prompt.push_str("可引用动机维度统计作为补充线索。\n");
    prompt.push_str("- `stability_judgment`：三选一——\n");
    prompt.push_str("  - `\"stable\"` — n_eff ≥ ");
    prompt.push_str(&config.low_evidence_threshold.to_string());
    prompt.push_str(" 且统计指标方向一致\n");
    prompt.push_str("  - `\"contextual\"` — n_eff 充足但统计指标方差大或存在矛盾\n");
    prompt.push_str("  - `\"uncertain\"` — n_eff < ");
    prompt.push_str(&config.low_evidence_threshold.to_string());
    prompt.push_str(" 或 signal_label 为 \"insufficient_data\"\n");
    prompt.push_str("- `sufficient_evidence`：n_eff ≥ ");
    prompt.push_str(&config.low_evidence_threshold.to_string());
    prompt.push_str(" 时为 true，否则为 false\n\n");

    // ---- CRAFT: Target（质量目标） ----
    prompt.push_str("# Target（质量目标）\n");
    prompt.push_str("- signal_label 是性格特征词，不是话题名——这是最关键的区分\n");
    prompt.push_str("- evidence_citation 要具体可追溯，不是笼统的「数据支持此结论」\n");
    prompt
        .push_str("- stability_judgment 诚实反映数据充分度——n_eff 不足时不要勉强给 \"stable\"\n\n");

    // ---- 分类统计数据 ----
    prompt.push_str("---\n\n");
    prompt.push_str("# 分类统计数据\n\n");
    for cat in &stats.categories {
        prompt.push_str(&format_category_stats(cat, config.low_evidence_threshold));
        prompt.push('\n');
    }

    let max_events = 5;
    prompt.push_str("# 代表性事件\n\n");
    prompt.push_str(&format_representative_events(
        &stats.representative_events,
        max_events,
    ));

    prompt
}

/// 构建 Step 2 prompt：跨分类一致性比较。
///
/// v2.0 重构 (CRAFT 框架):
/// - Context: 跨分类高阶指标含义说明。
/// - Role: 整合者视角，从分散信号识别底色/主色调/点缀。
/// - Action + Format: JSON 输出 base/primary/accent/excluded_categories/notes。
/// - 新增 excluded_categories 字段 + 3 条特殊处理规则处理 insufficient_data。
///
/// 参数:
/// - `category_signals`: Step 1 输出的逐分类信号。
/// - `metrics`: 跨分类高阶指标。
/// - `categories`: 分类统计（用于权重排名参考）。
pub fn build_step2_prompt(
    category_signals: &[CategorySignal],
    metrics: &CrossCategoryMetrics,
    categories: &[CategoryStats],
) -> String {
    let mut prompt = String::new();

    // ---- CRAFT: Context ----
    prompt.push_str("# Context（背景）\n");
    prompt.push_str("你是一位性格心理分析师。基于 Step 1 的逐分类性格信号和跨分类统计指标，识别跨领域的一致性模式。\n\n");
    prompt.push_str("跨分类高阶指标含义：\n");
    prompt.push_str(
        "- **情绪稳定性**（全局 valence 加权标准差）：越小越平稳，>0.5 表示情绪波动明显\n",
    );
    prompt.push_str("- **叙事一致性**（跨分类 presentation 分布相似度）：1.0=完全一致，<0.5 表示不同领域的信息呈现方式差异大\n");
    prompt.push_str(
        "- **态度矛盾检测**：≥1 表示可能存在跨分类内在矛盾（如在工作领域积极、在家庭领域消极）\n",
    );
    prompt.push_str(
        "- **社交开放性**：share 偏度（>0=右偏，倾向分享）和峰度（>0=尖峰，分享态度集中）\n\n",
    );

    // ---- CRAFT: Role ----
    prompt.push_str("# Role（角色定位）\n");
    prompt.push_str("你以整合者的视角工作：从分散的分类信号中识别反复出现的模式（底色）、在特定领域突出的模式（主色调）、");
    prompt.push_str(
        "以及仅在特定场景出现的模式（点缀）。你利用跨分类指标验证或质疑逐分类信号的一致性。\n\n",
    );

    // ---- 逐分类信号 ----
    prompt.push_str("---\n\n");
    prompt.push_str("# 逐分类信号\n\n");
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

    // ---- 跨分类指标 ----
    prompt.push_str(&format_cross_category(metrics));
    prompt.push('\n');

    // ---- 分类权重排名 ----
    prompt.push_str("# 分类权重排名（贝叶斯收缩后）\n");
    for cat in categories.iter().take(5) {
        prompt.push_str(&format!(
            "  {} - 权重 {:.1}% | n_eff={:.1} | 收缩后valence均值={:.3} | 收缩后share均值={:.3}\n",
            cat.category, cat.group_weight, cat.n_eff, cat.valence_mean, cat.share_mean,
        ));
    }
    prompt.push('\n');

    // ---- CRAFT: Action + Format ----
    prompt.push_str("# Action（执行任务）\n");
    prompt.push_str("基于逐分类信号和跨分类指标，输出三层性格候选。\n\n");

    prompt.push_str("# Format（输出格式）\n");
    prompt.push_str("你的整个回复必须是一个裸 JSON 对象，以 { 开头、以 } 结尾：\n\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"base_candidates\": [\"底色候选标签1\", \"底色候选标签2\"],\n");
    prompt.push_str("  \"primary_candidates\": [\"主色调候选标签1\"],\n");
    prompt.push_str("  \"accent_candidates\": [\"点缀候选标签1\", \"点缀候选标签2\"],\n");
    prompt.push_str("  \"excluded_categories\": [\"分类名A\", \"分类名B\"],\n");
    prompt.push_str("  \"notes\": \"2-3句话解释分层逻辑和排除原因\"\n");
    prompt.push_str("}\n\n");

    prompt.push_str("分层原则：\n");
    prompt.push_str("- **base_candidates**（底色）：跨 ≥2 个分类一致出现的信号，且这些分类的 sufficient_evidence 均为 true → 可能是跨场景的稳定性格\n");
    prompt.push_str("- **primary_candidates**（主色调）：仅在权重最高的分类中出现但 n_eff 最充足的信号 → 第一印象特征\n");
    prompt.push_str("- **accent_candidates**（点缀）：仅在特定分类中出现且 n_eff 较低，或与特定情境强相关的信号 → 条件性特征\n");
    prompt.push_str("- **excluded_categories**（排除的分类）：signal_label 为 \"insufficient_data\" 或 sufficient_evidence 为 false 的分类，");
    prompt.push_str("不参与三层分配，**仅需列出分类名**\n\n");

    prompt.push_str("特殊处理规则：\n");
    prompt.push_str("1. 遇到 signal_label 为 \"insufficient_data\" 的分类 → 将其加入 excluded_categories，不从中提取任何候选标签。");
    prompt.push_str("该分类的统计数据已在 Step 1 中声明不可靠，此处的职责是**不做强行推断**。\n");
    prompt.push_str("2. 遇到 stability_judgment 为 \"uncertain\" 但 signal_label 不是 \"insufficient_data\" 的分类 → ");
    prompt.push_str(
        "可以从该分类提取标签，但**只能放入 accent_candidates**，不能进入 base 或 primary。\n",
    );
    prompt.push_str("3. 跨分类指标显示「态度矛盾检测≥1」 → 在 notes 中记录矛盾涉及的分类对，");
    prompt.push_str("矛盾双方的标签各自降一级（base→primary, primary→accent）。\n\n");

    // ---- CRAFT: Target ----
    prompt.push_str("# Target（质量目标）\n");
    prompt.push_str("- 每个标签是 2-4 字的性格特征词\n");
    prompt.push_str("- 一个标签只能出现在一个层级（不能同时在 base 和 primary）\n");
    prompt.push_str("- notes 要解释分层逻辑——什么证据支持 base，为什么某标签退为 accent，哪些分类因数据不足被排除\n");
    prompt.push_str("- excluded_categories 必须包含所有 insufficient_data 的分类，不要遗漏\n");

    prompt
}

/// 构建 Step 3 prompt：合成结构化性格画像。
///
/// v2.0 重构 (CRAFT 框架):
/// - Context: 基于 Step 2 候选 + Step 1 信号合成最终画像。
/// - Role: 人格心理学家精确性——不只是"是什么"，更重要的是"不是什么"。
/// - Action: 为每个候选标签生成完整 trait 记录。
/// - Format: JSON 数组，含 layer/trait_label/meaning/not_meaning/trigger/suppress/related/confidence。
/// - Target: 差异化 confidence（按 n_eff 分段），not_meaning 必填。
///
/// 增加"话题 vs 性格"语义区分指令，防止 LLM 将
/// 对话话题名称（如"沉浸体验""系统逻辑"）当作性格标签输出。
/// 增加 confidence 差异化指导，要求 LLM 根据证据量
/// 给出不同的置信度，避免所有 trait 置信度统一。
pub fn build_step3_prompt(
    analysis: &ConsistencyAnalysis,
    _category_signals: &[CategorySignal],
    _stats: &StatsSummary,
) -> String {
    let mut prompt = String::new();

    // ---- CRAFT: Context ----
    prompt.push_str("# Context（背景）\n");
    prompt.push_str("你是一位性格心理分析师。基于 Step 2 的三层候选标签和 Step 1 的逐分类信号，合成最终的结构化性格画像。\n\n");

    // ---- CRAFT: Role ----
    prompt.push_str("# Role（角色定位）\n");
    prompt.push_str("你以人格心理学家的精确性工作：为每个标签赋予完整的语义描述——不只是「是什么」，更重要的是「不是什么」。");
    prompt.push_str("你基于数据充分度赋予差异化的置信度，而不是所有标签一个分数。\n\n");

    // ---- 候选标签 ----
    prompt.push_str("---\n\n");
    prompt.push_str("# Step 2 输出（三层候选标签）\n\n");

    prompt.push_str("## 底色候选\n");
    for label in &analysis.base_candidates {
        prompt.push_str(&format!("  - {}\n", label));
    }
    if analysis.base_candidates.is_empty() {
        prompt.push_str("  （无）\n");
    }

    prompt.push_str("\n## 主色调候选\n");
    for label in &analysis.primary_candidates {
        prompt.push_str(&format!("  - {}\n", label));
    }
    if analysis.primary_candidates.is_empty() {
        prompt.push_str("  （无）\n");
    }

    prompt.push_str("\n## 点缀候选\n");
    for label in &analysis.accent_candidates {
        prompt.push_str(&format!("  - {}\n", label));
    }
    if analysis.accent_candidates.is_empty() {
        prompt.push_str("  （无）\n");
    }
    prompt.push('\n');

    // ---- CRAFT: Action ----
    prompt.push_str("# Action（执行任务）\n");
    prompt.push_str("为 base_candidates / primary_candidates / accent_candidates 中的每个标签，生成完整的 trait 记录。\n\n");

    // ---- CRAFT: Format ----
    prompt.push_str("# Format（输出格式）\n");
    prompt.push_str("你的整个回复必须是一个裸 JSON 数组，以 [ 开头、以 ] 结尾：\n\n");
    prompt.push_str("[\n");
    prompt.push_str("  {\n");
    prompt.push_str("    \"layer\": \"Base\",\n");
    prompt.push_str("    \"trait_label\": \"尽责\",\n");
    prompt.push_str("    \"meaning\": \"该用户在工作相关场景中表现出高度的计划性和完成度，倾向于主动承担责任并追踪进展\",\n");
    prompt.push_str("    \"not_meaning\": \"不是在所有生活领域都同样尽责——在休闲社交场景中可能更随性和放松\",\n");
    prompt.push_str("    \"trigger\": \"工作场景、有时间压力的任务\",\n");
    prompt.push_str("    \"suppress\": \"纯社交场合、放松休息时\",\n");
    prompt.push_str("    \"related\": \"自律,成就导向\",\n");
    prompt.push_str("    \"seq\": 0,\n");
    prompt.push_str("    \"confidence\": 0.80\n");
    prompt.push_str("  }\n");
    prompt.push_str("]\n\n");

    prompt.push_str("字段约束：\n");
    prompt.push_str("- `layer`：Base / Primary / Accent——与 Step 2 的候选层级严格一致\n");
    prompt.push_str("- `trait_label`：2–4 字中文标签，与 Step 2 的候选标签一致\n");
    prompt.push_str("- `meaning`：1–2 句话，第三人称。描述具体行为模式，**不是重复标签名的同义词**（如标签「尽责」，meaning 不能说「此人很尽责」）\n");
    prompt.push_str("- `not_meaning`：**必须填写**（不能填 null 或空字符串）。澄清该标签的边界——「是什么但不是什么」。这是关键的排除性定义\n");
    prompt.push_str("- `trigger`：触发此特质的典型场景（可选，填 null）\n");
    prompt.push_str("- `suppress`：抑制此特质的典型场景（可选，填 null）\n");
    prompt.push_str("- `related`：其他相关标签名，逗号分隔（可选，填 null）\n");
    prompt.push_str("- `seq`：层内排序（0-based）\n");
    prompt.push_str(
        "- `confidence`：该推断的置信度 0.0–1.0，**必须差异化，不要所有标签使用相同值**——\n",
    );
    prompt.push_str("  - n_eff ≥ 10 且跨 ≥2 分类一致 → 0.80–0.90\n");
    prompt.push_str("  - n_eff 5–10 或单分类但 n_eff 充足 → 0.60–0.75\n");
    prompt.push_str("  - n_eff < 5 → 0.40–0.55\n");
    prompt.push_str(
        "  - 因跨分类矛盾被降级的标签（从 base 降为 primary/accent） → 在原有基础上 -0.10\n\n",
    );

    prompt.push_str("## 重要区分：话题 vs 性格\n");
    prompt.push_str("以下候选标签来自对用户在不同生活话题领域的行为统计。\n");
    prompt.push_str("话题名称（如「沉浸体验」「系统逻辑」「游戏」「技术开发」等）描述的是对话涉及的主题领域，不是性格特征。\n");
    prompt.push_str("你的任务是**从这些话题领域的行为模式中提炼出跨领域的性格特征**。\n\n");
    prompt.push_str("性格标签应描述人的稳定行为倾向，正确示例: \"尽责\"\"温和\"\"好奇\"\"坚韧\"\"外向\"\"谨慎\"\n");
    prompt.push_str("错误示例（这些是话题名，不是性格标签）: \"沉浸体验\"\"系统逻辑\"\"叙事驱动\"\"AI模拟\"\"规则构建\"\n\n");

    // ---- CRAFT: Target ----
    prompt.push_str("# Target（质量目标）\n");
    prompt.push_str("- 每个标签可独立理解：meaning 不需要看 trait_label 也能知道说的是什么\n");
    prompt.push_str("- not_meaning 是区分相似标签的关键（如「尽责但不焦虑」vs「尽责且焦虑」）\n");
    prompt.push_str("- confidence 必须有差异化\n");
    prompt.push_str("- 标签总数：3–8 条（base 1–2, primary 1–3, accent 1–3）\n");

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

        // 话题特征词黑名单。
        // L2 事件提取产出的 category 是话题聚类结果（如"沉浸体验""系统逻辑"），
        // 而非性格维度。若统计指标不足以提炼出性格信号，标记为"insufficient_data"
        // 而非直接用话题名生成伪性格标签。
        const TOPIC_BLACKLIST: &[&str] = &[
            "沉浸体验",
            "系统逻辑",
            "叙事驱动",
            "规则构建",
            "角色带入",
            "AI模拟",
            "卡面模拟",
            "游戏",
            "技术",
            "编程",
            "开发",
            "界面设计",
            "数值系统",
            "世界观设定",
        ];

        let is_topic_category = TOPIC_BLACKLIST.iter().any(|kw| cat.category.contains(kw));

        // 优先使用最显著的信号维度
        let (signal_label, stability) = if is_topic_category && !sufficient {
            // 话题类分类 + 低证据量 → 不足以推断性格信号
            (format!("{}-数据不足", cat.category), "uncertain")
        } else if cat.valence_mean > 0.4 && cat.valence_std < 0.4 {
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

    // ---- 动机维度的点缀特征 ----
    // 从动机统计中提取显著的动机驱动模式作为点缀 trait
    for motive_stat in stats.motive_stats.iter().take(3) {
        // 仅对 n_eff >= 2.0 的动机生成信号
        if motive_stat.n_eff < 2.0 {
            continue;
        }
        // 根据动机的效价模式生成 signal label
        let motive_signal = if motive_stat.valence_mean > 0.3 && motive_stat.valence_std < 0.5 {
            format!("动机-{}-正向驱动", motive_stat.motive)
        } else if motive_stat.valence_mean < -0.3 {
            format!("动机-{}-负向驱动", motive_stat.motive)
        } else if motive_stat.share_mean > 0.6 {
            format!("动机-{}-高分享", motive_stat.motive)
        } else if motive_stat.presentation_subjective_ratio > 0.6 {
            format!("动机-{}-主观表达", motive_stat.motive)
        } else {
            format!("动机-{}-驱动", motive_stat.motive)
        };
        // 仅当动机信号不在已有候选里且不重复时才添加
        if !accent_candidates.contains(&motive_signal) && accent_candidates.len() < 8 {
            accent_candidates.push(motive_signal);
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

    // 根据分类统计指标动态计算 evidence 和 consistency，
    // 避免所有 trait 使用相同的硬编码初始值（导致统一 47% 置信度）
    let compute_mock_evidence = |n_eff: f64| n_eff.clamp(0.0, 100.0);
    let compute_mock_consistency = |valence_std: f64, share_std: f64| {
        let avg_std = (valence_std + share_std) / 2.0;
        (1.0 - avg_std).clamp(0.1, 0.95)
    };
    let compute_mock_confidence = |evidence: f64, consistency: f64| {
        if evidence <= 0.0 {
            0.0
        } else {
            consistency * (1.0 - 1.0 / (1.0 + evidence))
        }
    };

    // 从信号标签中匹配对应分类的统计指标。
    // 信号标签格式为 "{category}-{signal}"（如"工作-积极稳定"），
    // 通过遍历所有 category 检查标签前缀来匹配。
    let find_stats_for_signal = |signal_label: &str| -> Option<(&CategoryStats, f64, f64)> {
        stats
            .categories
            .iter()
            .find(|c| signal_label.starts_with(&c.category))
            .map(|cs| {
                let ev = compute_mock_evidence(cs.n_eff);
                let con = compute_mock_consistency(cs.valence_std, cs.share_std);
                (cs, ev, con)
            })
    };

    // 底色
    for (i, label) in consistency.base_candidates.iter().enumerate().take(3) {
        let (evidence, consistency) = find_stats_for_signal(label)
            .map(|(_cs, ev, con)| (ev, con))
            .unwrap_or((1.0, 0.5));
        let confidence = compute_mock_confidence(evidence, consistency);

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
            confidence,
            evidence,
            consistency,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
        trait_seq = i as i32 + 1;
    }

    // 主色调
    for (i, label) in consistency.primary_candidates.iter().enumerate().take(2) {
        let (evidence, consistency) = find_stats_for_signal(label)
            .map(|(_cs, ev, con)| (ev, con))
            .unwrap_or((1.0, 0.5));
        let confidence = compute_mock_confidence(evidence, consistency);

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
            confidence,
            evidence,
            consistency,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
    }
    trait_seq += consistency.primary_candidates.len().min(2) as i32;

    // 点缀
    for (i, label) in consistency.accent_candidates.iter().enumerate().take(4) {
        let (evidence, consistency) = find_stats_for_signal(label)
            .map(|(_cs, ev, con)| (ev * 0.5, con * 0.7))
            // 点缀层证据量较低，总体折扣
            .unwrap_or((0.5, 0.3));
        // 动机维度标签（"动机-xxx-驱动"）或"内在矛盾型"取默认值
        let (evidence, consistency) = if label.starts_with("动机-") || label.starts_with("内在矛盾")
        {
            (0.5, 0.3)
        } else {
            (evidence, consistency)
        };
        let confidence = compute_mock_confidence(evidence, consistency);

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
            confidence,
            evidence,
            consistency,
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
            confirmed_count: 8,
            tentative_count: 0,
            discarded_count: 2,
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
            motive_stats: Vec::new(),
        }
    }

    // ---- Prompt 构建 ----

    #[test]
    fn build_step1_prompt_is_valid() {
        let stats = make_test_stats();
        let config = InferrerConfig::default();
        let prompt = build_step1_prompt(&stats, &config, None, None);
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
        assert!(prompt.contains("excluded_categories"));
        assert!(prompt.contains("special处理规则") || prompt.contains("特殊处理"));
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
            confirmed_count: 0,
            tentative_count: 0,
            discarded_count: 0,
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
            motive_stats: Vec::new(),
        };
        let result = mock_infer(&stats, "user-0001");
        assert!(result.category_signals.is_empty());
        assert!(result.traits.is_empty());
    }

    // ---- 动机维度 mock 推断 ----

    #[test]
    fn mock_infer_with_motive_stats_generates_motive_traits() {
        use crate::inference::stats::MotiveStats;

        let mut stats = make_test_stats();
        // 添加两个动机统计条目
        stats.motive_stats = vec![
            MotiveStats {
                motive: "地位维护".into(),
                event_count: 3,
                n_eff: 2.5,
                valence_mean: 0.5,
                valence_std: 0.2,
                valence_positive_ratio: 0.8,
                share_mean: 0.6,
                share_std: 0.2,
                presentation_objective_ratio: 0.3,
                presentation_subjective_ratio: 0.5,
                presentation_mixed_ratio: 0.2,
                avg_salience: 0.7,
            },
            MotiveStats {
                motive: "自主性".into(),
                event_count: 2,
                n_eff: 2.2,
                valence_mean: -0.4,
                valence_std: 0.3,
                valence_positive_ratio: 0.2,
                share_mean: 0.8,
                share_std: 0.1,
                presentation_objective_ratio: 0.1,
                presentation_subjective_ratio: 0.7,
                presentation_mixed_ratio: 0.2,
                avg_salience: 0.6,
            },
        ];

        let result = mock_infer(&stats, "user-0001");
        // 应该包含动机驱动的 accent trait
        let motive_traits: Vec<_> = result
            .traits
            .iter()
            .filter(|t| t.trait_label.contains("动机-"))
            .collect();
        assert!(
            !motive_traits.is_empty(),
            "mock_infer 应在有动机数据时生成动机相关 trait，实际 traits: {:?}",
            result
                .traits
                .iter()
                .map(|t| &t.trait_label)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn build_step1_prompt_includes_calibration_preamble() {
        let stats = make_test_stats();
        let config = InferrerConfig::default();
        let prompt = build_step1_prompt(&stats, &config, None, None);
        assert!(prompt.contains("校准权重链"));
        assert!(prompt.contains("confidence_factor"));
        assert!(prompt.contains("tentative 事件"));
        assert!(prompt.contains("分层经验贝叶斯收缩"));
    }

    #[test]
    fn build_step1_prompt_with_motive_text() {
        use crate::inference::stats::MotiveStats;

        let mut stats = make_test_stats();
        stats.motive_stats = vec![MotiveStats {
            motive: "归属".into(),
            event_count: 2,
            n_eff: 1.8,
            valence_mean: 0.3,
            valence_std: 0.2,
            valence_positive_ratio: 0.7,
            share_mean: 0.5,
            share_std: 0.15,
            presentation_objective_ratio: 0.4,
            presentation_subjective_ratio: 0.3,
            presentation_mixed_ratio: 0.3,
            avg_salience: 0.55,
        }];

        let motive_text = format_motive_stats(&stats.motive_stats, 5);
        let prompt =
            build_step1_prompt(&stats, &InferrerConfig::default(), None, Some(&motive_text));
        assert!(prompt.contains("动机维度统计"));
        assert!(prompt.contains("归属"));
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
