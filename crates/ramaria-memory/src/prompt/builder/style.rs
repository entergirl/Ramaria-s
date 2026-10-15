//! crates/ramaria-memory/src/prompt/builder/style.rs - 表达层（说话风格 + 对话示例）构建
//!
//! 设计特点:
//! - `build_style_layer`: 说话风格（手工优先）+ 自动风格规则 + 对话示例的组装
//! - 手工与自动风格互斥渲染（手工存在时覆盖自动规则，D-V17-004）
//! - `build_statement`: 说话片段渲染（无编号对话流；前文对话化 + 对方/你行）
//! - 纯字符串拼接，无 I/O 与 LLM 依赖

use super::role::{parse_persona_config, persona_patch};
use super::{PromptConfig, PromptContext, STATEMENT_LEAD, STYLE_USAGE_LEAD};

// =========================================================
// 表达层（说话风格）: 说话风格 + 对话示例
// =========================================================

/// 组装表达层块（`# 说话风格（表达层）`）：说话风格 + 自动风格规则 + 对话示例。
///
/// 对应 Personality + Statement 两块 + 自动风格规则子段（A3）。
///
/// 子段组合规则（手工覆盖优先，D-V17-004）:
/// - 手工 `speaking_style`（persona.config）存在 → 只注入手工风格
///   （自动风格规则被覆盖，不注入）。
/// - 手工不存在且 `style_rule_text`（自动规则）非空 → 注入 `## 自动风格规则`。
/// - 两子段皆缺省时整体不产生段落。
///
/// 探针消融（F3 / B0 / B1 / S_*）:
/// - `config.include_speaking_style=false` → 说话风格与自动风格规则均不渲染
///   （表达层关闭；对话示例仍由 `include_examples` 独立控制）。
///
/// 提示词补丁:
/// - `[prompt].style_suffix` 非空时追加为末尾子段（追加语义，不覆盖既有子段）；
///   补丁属 persona 配置，不受上述消融开关影响。
pub(super) fn build_style_layer(context: &PromptContext, config: &PromptConfig) -> String {
    let mut sub: Vec<String> = Vec::with_capacity(3);

    // 说话风格（persona.config 的 speaking_style，手工 E_rules 优先）
    let style = if config.include_speaking_style {
        build_personality(context)
    } else {
        String::new()
    };
    if !style.is_empty() {
        sub.push(style);
    } else if config.include_speaking_style {
        // 自动风格规则（A3 统计产出；手工覆盖时不注入）
        if let Some(rule) = context
            .style_rule_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sub.push(format!("## 自动风格规则\n{STYLE_USAGE_LEAD}\n{rule}"));
        }
    }

    // 对话示例（Few-shot）
    let statement = build_statement(context, config);
    if !statement.is_empty() {
        sub.push(statement);
    }

    // 提示词补丁（[prompt].style_suffix）：表达层末尾追加（不覆盖任何既有子段）；
    // 未配置 → 不产生任何输出（零字节变化），不随消融开关关闭
    if let Some(suffix) =
        persona_patch(context.persona.as_ref()).and_then(|patch| patch.style_suffix)
    {
        sub.push(suffix);
    }

    if sub.is_empty() {
        return String::new();
    }
    format!("# 说话风格（表达层）\n{}", sub.join("\n\n"))
}

/// 组装说话风格子段（`## 说话风格`）。
///
/// v2.0: 从 Block A 中独立出来，作为独立段。
/// 并入表达层作为子段；无 speaking_style 时返回空（不产生段落，由自动风格规则接管）。
///
/// 数据源（按优先级取值）:
/// 1. `persona.config`（persona.toml 原文）`[blocks].speaking_style`（用户自设，与 `A_persona`/`E_rules` 同级）；
/// 2. `persona.config` 的 JSON `speaking_style`（历史兼容）。
///
/// 正文前加使用引导行，避免风格描述被复述为介绍内容。
fn build_personality(context: &PromptContext) -> String {
    let Some(ref persona) = context.persona else {
        return String::new();
    };

    let style = parse_persona_config(persona)
        .and_then(|parsed| {
            parsed
                .blocks
                .into_iter()
                .find_map(|(key, value)| (key == "speaking_style").then_some(value))
        })
        .filter(|style| !style.trim().is_empty())
        .or_else(|| {
            persona
                .config
                .as_deref()
                .and_then(|cfg| serde_json::from_str::<serde_json::Value>(cfg).ok())
                .and_then(|obj| {
                    obj.get("speaking_style")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .filter(|style| !style.trim().is_empty())
        });

    match style {
        Some(style) => format!("## 说话风格\n{STYLE_USAGE_LEAD}\n{style}"),
        None => String::new(),
    }
}

/// 组装对话示例子段（`## 对话示例`）：说话片段（无编号，连续对话流）。
///
/// 渲染形态:
/// - 每条示例渲染为对话片段（前文对话行 + 对方行 + 你行），无编号、块间空行分隔；
/// - 前文（`context`）行由提取期"用户: / 你: "标签转为"对方：/ 你："对话行（最多 3 行）；
/// - 无示例时返回空（不产生段落）。
fn build_statement(context: &PromptContext, config: &PromptConfig) -> String {
    if !config.include_examples || context.examples.is_empty() {
        return String::new();
    }

    let mut blocks: Vec<String> = Vec::new();
    for ex in context.examples.iter().take(config.max_examples) {
        let mut lines: Vec<String> = Vec::new();

        // 前文语境（对话化渲染，最多 3 行）
        if let Some(ctx) = ex
            .context
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            for cl in ctx.lines().take(3) {
                let line = cl.trim();
                if line.is_empty() {
                    continue;
                }
                lines.push(promote_context_line(line));
            }
        }

        lines.push(format!("对方：{}", ex.partner));
        lines.push(format!("你：{}", ex.reply));
        blocks.push(lines.join("\n"));
    }

    if blocks.is_empty() {
        return String::new();
    }

    format!("## 对话示例\n{STATEMENT_LEAD}\n\n{}", blocks.join("\n\n"))
}

/// 把提取期 context 行（`用户: …` / `你: …`）转为对话片段行（`对方：…` / `你：…`）。
///
/// 说明:
/// - 前缀标签半角冒号统一为全角冒号（与示例行同构）；
/// - 无已知前缀时原样保留（防御）。
fn promote_context_line(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("用户:") {
        return format!("对方：{}", rest.trim());
    }
    if let Some(rest) = line.strip_prefix("你:") {
        return format!("你：{}", rest.trim());
    }
    line.to_string()
}
