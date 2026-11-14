//! crates/ramaria-eval/src/tui/console/review/detail.rs - Ramaria 审查屏逐题详情
//!
//! 设计特点:
//! - 只读产物：分节文本由逐题记录与判分结果拼装，不重算任何指标
//! - 分节顺序固定：输入、参考答案、回复、召回证据、检索指标、分项得分、可信度
//! - 缺值统一写 `-`，整段缺失给一句说明，不省略分节标题
//! - 召回轨迹复用沙盒渲染入口，审查屏与沙盒对同一 trace 的口径一致
//! - 多行正文拆成多条文本行，避免控件把换行当成普通字符

use crate::run::RunItemRecord;
use crate::sandbox::trace_lines;
use crate::score::ScoredItem;

#[cfg(test)]
mod tests;

// =========================================================
// 文本行
// =========================================================

/// 详情行样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailStyle {
    /// 普通正文
    Normal,
    /// 分节标题
    Heading,
    /// 命中 / 正常
    Good,
    /// 未命中 / 失败
    Bad,
    /// 说明与轨迹
    Muted,
}

/// 一条详情文本。
///
/// 字段约定:
/// - `text`: 已排好版的一行文本；
/// - `style`: 渲染用样式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailLine {
    pub(crate) text: String,
    pub(crate) style: DetailStyle,
}

/// 一道题的完整详情。
///
/// 字段约定:
/// - `item`: 逐题运行记录；
/// - `scored`: 判分结果（未判分为 `None`）；
/// - `lines`: 分节文本（渲染只读）。
pub(crate) struct DetailView {
    pub(crate) item: RunItemRecord,
    pub(crate) scored: Option<ScoredItem>,
    pub(crate) lines: Vec<DetailLine>,
}

// =========================================================
// 组装
// =========================================================

/// 组装一道题的详情文本。
///
/// 参数:
/// - `item`: 逐题运行记录；
/// - `scored`: 判分结果，`None` 表示未判分（缺产物或该题未判）。
///
/// 返回:
/// - 含全部分节的详情视图；判分相关分节在缺值处给说明而不是省略。
pub(crate) fn build_detail(item: RunItemRecord, scored: Option<ScoredItem>) -> DetailView {
    let mut lines = Vec::new();
    push_input(&mut lines, &item);
    push_reference(&mut lines, &item);
    push_reply(&mut lines, &item);
    push_recall(&mut lines, &item);
    push_retrieval(&mut lines, scored.as_ref());
    push_rubric(&mut lines, scored.as_ref());
    push_extra(&mut lines, scored.as_ref());
    DetailView {
        item,
        scored,
        lines,
    }
}

/// 输入分节。
fn push_input(lines: &mut Vec<DetailLine>, item: &RunItemRecord) {
    heading(lines, "输入");
    push_labeled(lines, "题目", &item.question);
    push_line(
        lines,
        format!("提问时间: {}   样本: {}", item.ask_time, item.sample_key),
        DetailStyle::Normal,
    );
    push_line(
        lines,
        format!("人格: {}   条件: {}", item.persona_uid, item.condition_id),
        DetailStyle::Normal,
    );
    push_line(
        lines,
        format!(
            "口径: {} / {} / {} 字符 / {}",
            item.tier, item.pipeline, item.budget_chars, item.answer_model_effective
        ),
        DetailStyle::Normal,
    );
    push_line(
        lines,
        format!(
            "类别: {}   题型: {}",
            join_u8(&item.category),
            join_or(&item.question_type)
        ),
        DetailStyle::Normal,
    );
}

/// 参考答案分节。
fn push_reference(lines: &mut Vec<DetailLine>, item: &RunItemRecord) {
    heading(lines, "参考答案");
    push_labeled(lines, "参考答案", &item.gold_answer);
    push_line(
        lines,
        format!("gold 消息: {} 条", item.gold_message_ids.len()),
        DetailStyle::Muted,
    );
    push_line(
        lines,
        format!("证据: {}", join_or(&item.evidence)),
        DetailStyle::Muted,
    );
}

/// 回复分节。
fn push_reply(lines: &mut Vec<DetailLine>, item: &RunItemRecord) {
    heading(lines, "回复");
    match (&item.reply, &item.error) {
        (Some(reply), _) => push_labeled(lines, "回复", reply),
        (None, Some(error)) => push_line(lines, format!("失败: {error}"), DetailStyle::Bad),
        (None, None) => push_line(lines, "失败: 未记录原因", DetailStyle::Bad),
    }
    let mut summary = format!(
        "回复字数 {}   耗时 {}ms   尝试 {} 次",
        item.reply_chars, item.elapsed_ms, item.attempts
    );
    if let Some(preset) = item.preset_chars {
        summary.push_str(&format!("   预置上下文 {preset} 字符"));
    }
    push_line(lines, summary, DetailStyle::Muted);
    if let Some(usage) = &item.usage {
        push_line(
            lines,
            format!(
                "用量: 输入 {} / 输出 {} / 命中缓存 {}",
                usage.prompt_tokens, usage.completion_tokens, usage.cache_hit_tokens
            ),
            DetailStyle::Muted,
        );
    }
}

/// 召回证据分节。
fn push_recall(lines: &mut Vec<DetailLine>, item: &RunItemRecord) {
    heading(lines, "召回证据");
    let Some(recall) = &item.recall else {
        push_line(lines, "无召回（预置上下文或失败）", DetailStyle::Muted);
        return;
    };
    push_line(
        lines,
        format!(
            "命中 {} 条   上下文 {} 字符   注入 {} token   原文片段 {} 字符",
            recall.items.len(),
            recall.context_chars,
            recall.injected_tokens,
            recall.utt_chars
        ),
        DetailStyle::Muted,
    );
    if recall.items.is_empty() {
        push_line(lines, "（本题召回结果为空）", DetailStyle::Muted);
    }
    for entry in &recall.items {
        push_line(
            lines,
            format!("  {:<8} {:<40} {:.2}", entry.layer, entry.id, entry.score),
            DetailStyle::Normal,
        );
    }
    for line in trace_lines(&recall.trace) {
        push_line(lines, format!("  {line}"), DetailStyle::Muted);
    }
}

/// 检索指标分节。
fn push_retrieval(lines: &mut Vec<DetailLine>, scored: Option<&ScoredItem>) {
    if scored.is_none() {
        return;
    }
    heading(lines, "检索指标");
    let retrieval = scored.and_then(|item| item.retrieval.as_ref());
    let Some(retrieval) = retrieval else {
        push_line(
            lines,
            "无检索指标（空 gold、失败或缺工作库）",
            DetailStyle::Muted,
        );
        return;
    };
    push_line(
        lines,
        format!(
            "消息级: 覆盖 {:.2}→{:.2}   精确 {:.2}→{:.2}   首命 {}→{}   单元 {}→{}   不可映射 {}→{}",
            retrieval.coverage_pre,
            retrieval.coverage_post,
            retrieval.precision_pre,
            retrieval.precision_post,
            option_usize(retrieval.rank_pre),
            option_usize(retrieval.rank_post),
            retrieval.units_pre,
            retrieval.units_post,
            retrieval.unmappable_pre,
            retrieval.unmappable_post
        ),
        DetailStyle::Normal,
    );
    match &retrieval.artifact {
        Some(artifact) => push_line(
            lines,
            format!(
                "artifact: 覆盖 {:.2}→{:.2}   精确 {:.2}→{:.2}   首命 {}→{}   gold {}   缺失 {}",
                artifact.coverage_pre,
                artifact.coverage_post,
                artifact.precision_pre,
                artifact.precision_post,
                option_usize(artifact.rank_pre),
                option_usize(artifact.rank_post),
                artifact.gold_artifacts,
                artifact.missing_gold
            ),
            DetailStyle::Normal,
        ),
        None => push_line(
            lines,
            "artifact: 无（缺映射或 gold artifact 为空）",
            DetailStyle::Muted,
        ),
    }
}

/// 分项得分分节。
fn push_rubric(lines: &mut Vec<DetailLine>, scored: Option<&ScoredItem>) {
    if scored.is_none() {
        return;
    }
    heading(lines, "分项得分");
    let rubric = scored.and_then(|item| item.rubric.as_ref());
    let Some(rubric) = rubric else {
        push_line(
            lines,
            "无分项判分（缺要点、无回复或 judge 未启用）",
            DetailStyle::Muted,
        );
        return;
    };
    let ratio = match rubric.ratio {
        Some(ratio) => format!("{:.1}%", ratio * 100.0),
        None => "-".to_string(),
    };
    push_line(
        lines,
        format!(
            "得分 {:.2} / {:.2}（{}）   全命中 {}   判分失败 {}",
            rubric.score,
            rubric.max,
            ratio,
            yes_no(rubric.full_hit),
            yes_no(rubric.judge_failed)
        ),
        DetailStyle::Normal,
    );
    for point in &rubric.points {
        let (mark, style) = match point.hit {
            Some(true) => ("命中", DetailStyle::Good),
            Some(false) => ("未命中", DetailStyle::Bad),
            None => ("未判", DetailStyle::Muted),
        };
        let reason = point
            .reason
            .as_deref()
            .map(|text| text.replace('\n', " "))
            .unwrap_or_else(|| "-".to_string());
        push_line(
            lines,
            format!(
                "  [{mark}] {:.1} — {}（票 {}/{}）",
                point.weight,
                reason,
                point.votes.iter().flatten().count(),
                point.votes.len()
            ),
            style,
        );
    }
}

/// 可信度、人格与提示分节。
fn push_extra(lines: &mut Vec<DetailLine>, scored: Option<&ScoredItem>) {
    let Some(scored) = scored else {
        heading(lines, "判分");
        push_line(
            lines,
            "未判分（先运行 ramaria eval score <run-id>）",
            DetailStyle::Bad,
        );
        return;
    };

    let credibility = &scored.credibility;
    push_line(
        lines,
        format!(
            "可信度: UA 提示 {}   UA 判定 {}   CD 判定 {}   判分失败 {}",
            yes_no_option(credibility.ua_refusal_hint),
            refusal_text(credibility.ua_judge),
            premise_text(credibility.cd_judge),
            yes_no(credibility.judge_failed)
        ),
        DetailStyle::Muted,
    );
    if let Some(persona) = &scored.persona {
        push_line(
            lines,
            format!(
                "人格: 语气 {}   事实 {}   情感 {}",
                match persona.tone {
                    Some(tone) => tone.to_string(),
                    None => "-".to_string(),
                },
                match &persona.fact {
                    Some(fact) => format!("{:.2}", fact.score),
                    None => "-".to_string(),
                },
                match &persona.emotion {
                    Some(emotion) => format!("{:.2}", emotion.score),
                    None => "-".to_string(),
                }
            ),
            DetailStyle::Muted,
        );
    }
    if !scored.notes.is_empty() {
        heading(lines, "提示");
        for note in &scored.notes {
            push_line(lines, format!("  {note}"), DetailStyle::Muted);
        }
    }
}

// =========================================================
// 文本辅助
// =========================================================

/// 追加分节标题。
fn heading(lines: &mut Vec<DetailLine>, text: &str) {
    push_line(lines, text, DetailStyle::Heading);
}

/// 追加一条文本行。
fn push_line(lines: &mut Vec<DetailLine>, text: impl Into<String>, style: DetailStyle) {
    lines.push(DetailLine {
        text: text.into(),
        style,
    });
}

/// 追加「标题: 正文」，正文含换行时拆成多行并缩进两格。
fn push_labeled(lines: &mut Vec<DetailLine>, label: &str, text: &str) {
    let mut parts = text.split('\n');
    let first = parts.next().unwrap_or_default();
    push_line(lines, format!("{label}: {first}"), DetailStyle::Normal);
    for part in parts {
        push_line(lines, format!("  {part}"), DetailStyle::Normal);
    }
}

/// 逗号拼接；空列表写 `-`。
fn join_or(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

/// 类别码拼接；空列表写 `-`。
fn join_u8(values: &[u8]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// 可选下标；缺值写 `-`。
fn option_usize(value: Option<usize>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "-".to_string(),
    }
}

/// 布尔中文。
fn yes_no(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}

/// 可选布尔中文。
fn yes_no_option(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "是",
        Some(false) => "否",
        None => "-",
    }
}

/// UA 判定文案。
fn refusal_text(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "拒答",
        Some(false) => "未拒答",
        None => "-",
    }
}

/// CD 判定文案。
fn premise_text(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "接受错误前提",
        Some(false) => "未接受",
        None => "-",
    }
}
