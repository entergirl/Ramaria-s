//! crates/ramaria-memory/src/inference/inferrer/prompt.rs - 三步推断 Prompt 构建
//!
//! 设计特点:
//! - Step 1: 逐分类个性模式提取（统计指标 + 因果链 + 动机维度 → 分类级性格信号）
//! - Step 2: 跨分类一致性比较（识别底色/主色调/点缀候选）
//! - Step 3: 合成三层结构化性格画像（含 confidence 差异化指导）
//! - 采用 CRAFT 框架组织 Prompt：Context / Role / Action / Format / Target
//! - 纯字符串拼接，无 I/O 与 LLM 依赖

use crate::inference::stats::{
    CategoryStats, CrossCategoryMetrics, MotiveStats, RepresentativeEvent, StatsSummary,
};

use super::types::{CategorySignal, ConsistencyAnalysis, InferrerConfig};

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
/// CRAFT 框架:
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
/// CRAFT 框架:
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
/// CRAFT 框架:
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
