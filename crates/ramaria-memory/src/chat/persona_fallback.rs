//! crates/ramaria-memory/src/chat/persona_fallback.rs - persona.toml 冷启动兜底
//!
//! 设计特点:
//! - DB persona.config 优先，文件系统回退（新路径 → 旧路径兼容）
//! - 由 `A_persona` + 共享回复规则组装有温度的基础 prompt
//! - 解析失败/文件缺失 → `None`，由上层降级到默认 Ramaria prompt
//! - 相对路径以进程工作目录为基准（与调用方 crate 位置无关）

use crate::init::{parse_persona_toml, resolve_chat_style_rules};

use super::time::now_timestamp_str;

// =========================================================
// persona.toml 冷启动兜底
// =========================================================

/// 尝试加载 persona.toml 并构建有温度的基础 system prompt。
///
/// 数据来源优先级:
/// 1. `db_config`: 从 DB persona.config 中读取的 TOML 内容（setup 时写入）
/// 2. 文件系统回退: `../config/personas/rama-0001.toml`，其次旧路径
///    `../config/persona.toml`（未迁移的旧安装兼容回退）
///
/// 参数:
/// - `db_config`: DB persona.config 内容（None 时直接走文件系统回退）。
///
/// 返回:
/// - `Some(prompt)`: 由 `A_persona` + `E_rules`（显式优先，缺省共享规则）组装的基础 prompt。
/// - `None`: 解析失败 / 文件缺失 —— 由上层降级到默认 Ramaria prompt。
pub fn load_persona_toml_prompt(db_config: Option<&str>) -> Option<String> {
    let content = if let Some(cfg) = db_config {
        // 优先使用 DB 中的 persona.toml 内容
        if cfg.contains("[identity]") || cfg.contains("[blocks]") {
            tracing::debug!("从 DB persona.config 加载 persona.toml");
            cfg.to_string()
        } else {
            // config 字段是其他 JSON 格式，回退到文件系统
            read_persona_toml_from_fs()?
        }
    } else {
        read_persona_toml_from_fs()?
    };

    let parsed = match parse_persona_toml(&content) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e, "persona.toml 解析失败");
            return None;
        }
    };

    let persona_block = parsed
        .blocks
        .iter()
        .find(|(k, _)| k == "A_persona")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    // 回复规则：显式 E_rules 优先，缺省回退共享规则（与生产装配路径同一口径）
    let rules_block = resolve_chat_style_rules(Some(content.as_str()));

    let name = &parsed.assistant_name;
    let time_str = now_timestamp_str();

    Some(format!(
        "你的名字是{name}。\n\n{persona_block}\n\n回复规则:\n{rules_block}\n\n\
         当前时间：{time_str}\n\n\
         你可以记住与用户的对话历史。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。"
    ))
}

/// 文件系统回退: 优先尝试新路径 `../config/personas/rama-0001.toml`，其次旧路径 `../config/persona.toml`。
///
/// 说明:
/// - 新路径为目录扫描模式，每文件 = 一个 persona。
/// - 旧路径保留作为兼容回退，供未迁移的旧安装使用。
/// - 相对路径以进程工作目录为基准（与调用方 crate 位置无关）。
fn read_persona_toml_from_fs() -> Option<String> {
    // 优先尝试新路径
    let new_path = "../config/personas/rama-0001.toml";
    if let Ok(c) = std::fs::read_to_string(new_path) {
        tracing::debug!(%new_path, "从文件系统加载 persona.toml (新路径)");
        return Some(c);
    }

    // 回退到旧路径
    let old_path = "../config/persona.toml";
    match std::fs::read_to_string(old_path) {
        Ok(c) => {
            tracing::debug!(%old_path, "从文件系统加载 persona.toml (旧路径兼容)");
            Some(c)
        }
        Err(e) => {
            tracing::debug!(%old_path, %e, "persona.toml 文件系统回退失败");
            None
        }
    }
}
