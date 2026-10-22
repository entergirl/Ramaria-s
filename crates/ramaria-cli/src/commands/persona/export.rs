//! crates/ramaria-cli/src/commands/persona/export.rs - Ramaria 人格预设导出命令
//!
//! 设计特点:
//! - 画像级导出：traits / facts / 启用行为规则 + config 重放的对话身份 / 自由文本块 / 提示词补丁
//! - 缺省输出到 stdout（`--out -` 同义）；`--out <file>` 写入指定文件（输出编排复用数据导出通道）
//! - 隐私：不含证据链与会话原文；对话示例默认不含，`--with-examples` 显式开启（输出头注明）
//! - `--json` 信封：写文件时 `data` 含 `written_to`；stdout 形态 `data` 含 `toml` 全文

use std::sync::Arc;

use anyhow::Context;
use ramaria_service::Engine;

// =========================================================
// 命令入口
// =========================================================

/// 导出人格画像为预设文件（超集 TOML）。
///
/// 参数:
/// - `engine`: 服务层引擎引用。
/// - `uid`: 目标人格 uid。
/// - `out`: 输出文件路径（None / `-` = stdout）。
/// - `with_examples`: 是否含入对话示例。
/// - `json`: JSON 信封输出。
pub(super) async fn run(
    engine: &Arc<Engine>,
    uid: String,
    out: Option<String>,
    with_examples: bool,
    json: bool,
) -> anyhow::Result<()> {
    let outcome = engine
        .persona_export(&uid, with_examples)
        .await
        .context("人格导出失败")?;

    // 目标：None / "-" = stdout；其余 = 文件路径
    let target_path = out.as_deref().filter(|path| *path != "-");

    if json {
        return match target_path {
            Some(path) => {
                let written_to =
                    crate::commands::export::write_output(&outcome.toml, Some(path), "toml")?;
                crate::json::emit_ok(&serde_json::json!({
                    "uid": outcome.uid,
                    "name": outcome.name,
                    "written_to": written_to,
                    "stats": outcome.stats,
                    "hints": outcome.hints,
                }))
            }
            None => crate::json::emit_ok(&outcome),
        };
    }

    crate::commands::export::write_output(&outcome.toml, Some(target_path.unwrap_or("-")), "toml")?;

    let stats = &outcome.stats;
    crate::ui::info(&format!(
        "已导出人格 {}（{}）：traits {} / facts {} / behavior_rules {} / blocks {} 段 / prompt 补丁 {}",
        outcome.uid,
        outcome.name,
        stats.traits,
        stats.facts,
        stats.behavior_rules,
        stats.blocks,
        if stats.prompt_patch { "有" } else { "无" }
    ));
    if stats.examples_included > 0 {
        crate::ui::warn(&format!(
            "已含 {} 条对话示例（真实对话片段，分享前请确认）",
            stats.examples_included
        ));
    } else {
        crate::ui::info("画像级导出：不含证据链、对话示例与会话原文");
    }
    for hint in &outcome.hints {
        crate::ui::warn(hint);
    }
    Ok(())
}
