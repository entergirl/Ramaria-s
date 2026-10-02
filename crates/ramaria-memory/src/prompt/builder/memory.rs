//! crates/ramaria-memory/src/prompt/builder/memory.rs - 记忆层（脉络层）构建
//!
//! 设计特点:
//! - `build_memory`: 近期对话脉络 + 相关历史记忆 + 原文片段 + 桥接的组装
//! - 接入脉络层预算分配器，独立预算 ≤ 30%，超限按优先级裁剪
//! - `render_utt_context`: utt 原文片段按相似度降序整块取舍（不做块内截断）
//! - `build_cross_session_narrative`: 近期 L1 摘要串联为话题脉络引导句
//! - 无 I/O 与 LLM 依赖，纯字符串拼接

use crate::prompt::layers::{LayerBudgetConfig, allocate_memory_layer_budget};
use crate::retriever::UttHit;

use super::{
    BRIDGE_LEAD, MEMORY_SECTION_INTRO, NARRATIVE_PLACEHOLDER, PromptConfig, PromptContext,
    RAG_PLACEHOLDER, UTT_LEAD,
};

// =========================================================
// Memory 块: 记忆上下文（最高优先级）
// =========================================================

/// 组装记忆层块：近期对话脉络 + 相关历史记忆 + 原文片段 + 桥接（`# 记忆（脉络层）`）。
///
/// v2.0: 从 Block C 从属位置提升为独立 Memory 块。
/// 接入脉络层预算分配器——
/// 独立预算 ≤ 30%，超限裁剪顺序：原文块 → 桥接头部 → 相关记忆 → 脉络保最近。
///
/// 子段落结构:
/// 1. `## 近期对话脉络` — 最近 1-3 条 L1 摘要的叙事引导句（预算内保最近）
/// 2. `## 相关历史记忆` — RAG 检索结果（条件注入）
/// 3. `## 原文片段` — utt 话语块（白名单外为 None 不产生段落）
/// 4. `## 桥接（上一会话尾部）` — 上一会话尾部原文
///
/// 探针消融（B0/B1/F4/S_*）:
/// - `config.include_narrative` / `include_memory_rag` / `include_utt` /
///   `include_bridge` 逐子段控制渲染；对应子段关闭时连同占位提示一起跳过。
/// - 全部子段关闭（B0 无记忆注入）→ 整个记忆块不产生（返回空串，由装配器跳过）。
pub(super) fn build_memory(context: &PromptContext, config: &PromptConfig) -> String {
    // B0 无记忆注入：所有记忆子段关闭 → 整块不产生（含头部与占位）。
    if !config.include_narrative
        && !config.include_memory_rag
        && !config.include_utt
        && !config.include_bridge
    {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::with_capacity(2);

    parts.push(MEMORY_SECTION_INTRO.to_string());

    // 脉络层预算分配（独立预算，默认 1000 tokens × 30% × 2 = 600 字符）
    let budget = config
        .memory_layer_budget_chars
        .unwrap_or_else(|| LayerBudgetConfig::default().budget_chars());
    let alloc = allocate_memory_layer_budget(
        context.utt_context.as_deref(),
        context.bridge_context.as_deref(),
        &context.recent_session_summaries,
        context.memory_context.as_deref(),
        budget,
    );

    // 近期对话脉络（预算内保最近；预算不足时显示"无历史对话"）
    if config.include_narrative {
        if alloc.summaries.is_empty() {
            parts.push(format!("\n\n## 近期对话脉络\n{}", NARRATIVE_PLACEHOLDER));
        } else {
            let narrative = build_cross_session_narrative(&alloc.summaries);
            let mut lines = vec!["\n\n## 近期对话脉络".to_string(), narrative];

            // 逐条列出近期摘要（截断到 120 字符）
            for (i, summary) in alloc.summaries.iter().enumerate() {
                let display = ramaria_core::text::truncate_chars(summary, 120);
                lines.push(format!("  {}. {}", i + 1, display));
            }

            parts.push(lines.join("\n"));
        }
    }

    // 相关历史记忆（RAG 结果；预算内句子边界截断）
    if config.include_memory_rag {
        match &alloc.rag {
            Some(rag) if !rag.trim().is_empty() => {
                // 段落标题已表明内容性质；引用时机由记忆层首段引导统一约束
                parts.push(format!("\n\n## 相关历史记忆\n{rag}"));
            }
            _ => {
                parts.push(format!("\n\n## 相关历史记忆\n{RAG_PLACEHOLDER}"));
            }
        }
    }

    // 原文片段（utt 话语块；预算不足/白名单外为 None → 不产生段落）
    if config.include_utt
        && let Some(utt) = &alloc.utt
        && !utt.trim().is_empty()
    {
        parts.push(format!("\n\n## 原文片段\n{UTT_LEAD}{utt}"));
    }

    // 桥接（上一会话尾部；预算不足/开关关闭/白名单外为 None → 不产生段落）
    if config.include_bridge
        && let Some(bridge) = &alloc.bridge
        && !bridge.trim().is_empty()
    {
        parts.push(format!(
            "\n\n## 桥接（上一会话尾部）\n{}{}",
            BRIDGE_LEAD, bridge
        ));
    }

    parts.join("")
}

// =========================================================
// utt 原文片段渲染与预算裁剪
// =========================================================

/// 按预算渲染【原文片段】段落内容（整块保留/丢弃，超预算按相似度从低到高丢整块）。
///
/// 规则:
/// - `hits` 必须按得分降序传入（检索侧保证）。
/// - 从高分到低分整块累加：未超预算的块全部保留，首个超预算的块及其后全部丢弃
///   （不做块内截断——原文是整体引用的，截断会破坏语义）。
///
/// 参数:
/// - `hits`: 检索命中（按得分降序）。
/// - `max_block_chars`: 全部块合计的字符预算上限（`[utt].max_block_chars`）。
///
/// 返回:
/// - 块文本序列（块间空行分隔）；`hits` 为空或首块即超预算时返回空字符串。
pub fn render_utt_context(hits: &[UttHit], max_block_chars: usize) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;

    for hit in hits {
        let text = hit.doc.block_text.trim();
        if text.is_empty() {
            continue;
        }
        let chars = text.chars().count();
        if used + chars > max_block_chars {
            // 超预算：丢整块（含其后所有块——已按相似度降序，剩余相似度更低）
            break;
        }
        kept.push(text.to_string());
        used += chars;
    }

    kept.join("\n\n")
}

/// 从近期 L1 摘要构建跨 session 叙事引导句。
///
/// 职责:
/// - 将孤立的 L1 摘要串联为话题脉络，告知 LLM 与对方聊过哪些话题。
/// - 多条摘要时追加用途指令，供 LLM 判断是否顺势延续话题。
///
/// 算法:
/// - 取最近 3 条摘要，提取前 30 字符作为话题锚点。
/// - 反转为主题时间线（最早→最近）后以"、"串联。
/// - 单条输出"你和对方聊过：{话题}。"；多条追加"可据此继续话题"。
///
/// 参数:
/// - `summaries`: 按时间降序排列的 L1 摘要文本列表。
///
/// 返回:
/// - 叙事引导句字符串。
pub fn build_cross_session_narrative(summaries: &[String]) -> String {
    if summaries.is_empty() {
        return String::new();
    }

    // 取最近 3 条
    let recent: Vec<&String> = summaries.iter().take(3).collect();

    // 提取每条摘要的前 30 字符作为话题锚点
    let topics: Vec<String> = recent
        .iter()
        .map(|s| {
            let anchor = ramaria_core::text::truncate_chars_bare(s, 30);
            anchor.trim().to_string()
        })
        .collect();

    // 反转为主题时间线（最早→最近）
    let mut timeline = topics.clone();
    timeline.reverse();

    let count = timeline.len();
    let topic_list = timeline.join("、");

    // 生成引导句：话题事实 + 多条时的延续用途
    if count == 1 {
        format!("你和对方聊过：{topic_list}。")
    } else {
        format!("你和对方聊过：{topic_list}。可据此继续话题。")
    }
}
