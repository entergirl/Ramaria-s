//! crates/ramaria-cli/src/commands/persona/render.rs - 人格命令文本渲染
//!
//! 设计特点:
//! - 导入结果摘要与拆解执行行渲染。
//! - TOML 多行块提取与截断摘要，供 prompt 预览复用。

use ramaria_memory::preset::PreviewMode;
use ramaria_service::types::{DecomposeExecution, PersonaImportOutcome};

/// 渲染导入结果摘要（人格行 + 写入统计 + 拆解行）。
pub(super) fn render_outcome(outcome: &PersonaImportOutcome) -> String {
    let reg = &outcome.preview.registration;
    let uid = reg.uid.as_deref().unwrap_or("（自动生成）");
    let mode = match reg.mode {
        PreviewMode::Create => "新建",
        PreviewMode::Merge => "合并",
    };
    let written = &outcome.written;
    let mut out = format!("导入完成：persona {uid}（{mode}）\n");
    out.push_str(&format!(
        "  已写入：traits {} / facts {} / examples {} / behavior_rules {}（失败 {}）\n",
        written.traits, written.facts, written.examples, written.behavior_rules, written.failed
    ));
    out.push_str(&format!(
        "  拆解：{}\n",
        decompose_label(&outcome.decompose)
    ));
    out
}

/// 拆解执行行文本（未启用 / 已执行 / 跳过原因）。
fn decompose_label(exec: &DecomposeExecution) -> String {
    if exec.executed {
        return format!(
            "已执行（traits {} / facts {}）",
            exec.trait_count, exec.fact_count
        );
    }
    if !exec.requested {
        return "未启用".to_string();
    }
    let reason = match exec.skipped_reason.as_deref() {
        Some("privacy_unconfirmed") => "隐私未确认",
        Some("no_input") => "无拆解输入",
        Some("llm_failed") => "LLM 调用失败",
        Some(other) => other,
        None => "未执行",
    };
    format!("已跳过（{reason}）")
}

// =========================================================
// TOML 解析辅助函数
// =========================================================
//
// 注: `extract_toml_value` 已提取至 `crate::util` 模块。
// `extract_toml_block` / `summarize_block` 仅被 persona 命令使用，保留于此。

/// 从 TOML 文本中提取 `[blocks]` 节下的多行字符串块。
///
/// 使用简单的状态机解析 TOML 的三引号多行字符串 `"""..."""`。
/// 精确匹配 `key = """` 开始，直到遇到单独的 `"""` 结束。
///
/// 参数:
/// - `content`: 完整 TOML 文本。
/// - `key`: 要查找的块名（如 "A_persona"）。
///
/// 返回:
/// - `Some(block_content)`: 找到的块内容（去除开头的空行）。
/// - `None`: 未找到或格式不符合预期。
pub(super) fn extract_toml_block(content: &str, key: &str) -> Option<String> {
    let lines = content.lines();
    let mut in_block = false;
    let mut block_lines: Vec<&str> = Vec::new();

    for line in lines {
        let trimmed = line.trim();

        if in_block {
            // 遇到独立的三引号表示结束
            if trimmed == "\"\"\"" {
                break;
            }
            block_lines.push(line);
        } else if trimmed.starts_with(&format!("{key} = \"\"\"")) {
            in_block = true;
            // 如果同一行后面还有内容（罕见），直接开始收集
            let after_marker = &trimmed[key.len() + 5..]; // skip `key = """`
            if after_marker == "\"\"\"" {
                // 单行空块
                break;
            }
            // 如果 """ 在同一行后还有内容（非标准写法），跳过
        }
    }

    if block_lines.is_empty() {
        return None;
    }

    // 去除首尾空行
    while block_lines.first().is_some_and(|l| l.trim().is_empty()) {
        block_lines.remove(0);
    }
    while block_lines.last().is_some_and(|l| l.trim().is_empty()) {
        block_lines.pop();
    }

    if block_lines.is_empty() {
        return None;
    }

    Some(block_lines.join("\n"))
}

/// 将文本截断为指定最大字符数，添加统一省略号 `…`。
///
/// 字符边界与省略号语义由 `ramaria_core::text::truncate_chars` 统一实现，不破坏 UTF-8。
pub(super) fn summarize_block(text: &str, max_chars: usize) -> String {
    // 先压缩多余空白为单个空格，便于单行展示
    let compressed: String = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    ramaria_core::text::truncate_chars(&compressed, max_chars)
}
