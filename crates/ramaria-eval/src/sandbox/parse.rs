//! crates/ramaria-eval/src/sandbox/parse.rs - Ramaria 评测沙盒输入解析
//!
//! 设计特点:
//! - 行级格式：角色前缀大小写不敏感，另接受中文别名（用户 / 助手）
//! - 轮次边界：空行或下一个角色前缀结束当前轮，轮内无前缀行按续行拼接
//! - 错误带行号并附格式提示，不静默丢弃无法识别的内容
//! - 行模式输入支持头部指令（后端来源 / 地址 / 模型 / 向量 / 人格）与 --- 分隔线，
//!   不引入正则与额外依赖

use std::path::PathBuf;

use ramaria_service::{ChatRole, ChatTurn};

use super::SandboxInput;
use super::backend::{SandboxBackendChoice, SandboxBackendMode, SandboxModelKind};

/// 行模式输入格式提示（解析错误与空输入错误共用）。
pub(crate) const LINE_MODE_HINT: &str = "行模式格式：可选头部指令 @backend inherit|local|cloud、@base-url <地址>、@model <标识>、@embedding <路径>、@persona <路径>，随后 user:/assistant: 对话，单独一行 --- 后写问题";

/// 角色前缀表（小写比较；中文别名与英文等价）。
const ROLE_PREFIXES: [(&str, ChatRole); 4] = [
    ("user:", ChatRole::User),
    ("assistant:", ChatRole::Assistant),
    ("用户:", ChatRole::User),
    ("助手:", ChatRole::Assistant),
];

// =========================================================
// 对话块解析
// =========================================================

/// 解析纯对话块（`user:` / `assistant:` 前缀，空行分轮）。
///
/// 参数:
/// - `text`: 对话文本（每行一条消息片段）。
///
/// 返回:
/// - 成功时按出现顺序返回轮次；
/// - 轮外出现无前缀行、或某轮内容为空时返回带行号的错误文案。
pub fn parse_dialogue(text: &str) -> Result<Vec<ChatTurn>, String> {
    parse_dialogue_from(text, 0)
}

/// 解析对话块并按起始行偏移校正错误行号。
fn parse_dialogue_from(text: &str, line_offset: usize) -> Result<Vec<ChatTurn>, String> {
    let mut turns = Vec::new();
    let mut pending: Option<PendingTurn> = None;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = line_offset + index + 1;
        if raw_line.trim().is_empty() {
            flush_pending(&mut turns, &mut pending)?;
            continue;
        }

        if let Some((role, content)) = parse_role(raw_line) {
            flush_pending(&mut turns, &mut pending)?;
            pending = Some(PendingTurn {
                role,
                line: line_number,
                content: content.trim().to_string(),
            });
            continue;
        }

        match pending.as_mut() {
            Some(turn) => {
                turn.content.push('\n');
                turn.content.push_str(raw_line.trim());
            }
            None => {
                return Err(format!(
                    "第 {line_number} 行缺少角色前缀（user:/assistant:）；{LINE_MODE_HINT}"
                ));
            }
        }
    }

    flush_pending(&mut turns, &mut pending)?;
    Ok(turns)
}

/// 未落盘的当前轮。
struct PendingTurn {
    role: ChatRole,
    line: usize,
    content: String,
}

/// 结束当前轮并写入结果；内容为空时报错。
fn flush_pending(
    turns: &mut Vec<ChatTurn>,
    pending: &mut Option<PendingTurn>,
) -> Result<(), String> {
    let Some(turn) = pending.take() else {
        return Ok(());
    };
    let content = turn.content.trim().to_string();
    if content.is_empty() {
        return Err(format!(
            "第 {} 行 {}: 后缺少内容",
            turn.line,
            turn.role.as_str()
        ));
    }
    turns.push(ChatTurn {
        role: turn.role,
        content,
    });
    Ok(())
}

/// 识别行首角色前缀。
///
/// 返回:
/// - 命中时返回角色与该行剩余文本（未裁剪）；
/// - 未命中返回 `None`。
fn parse_role(line: &str) -> Option<(ChatRole, &str)> {
    let trimmed = line.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    for (prefix, role) in ROLE_PREFIXES {
        if lowered.starts_with(prefix) {
            return Some((role, &trimmed[prefix.len()..]));
        }
    }
    None
}

// =========================================================
// 行模式输入解析
// =========================================================

/// 行模式解析结果。
///
/// 字段约定:
/// - `backend`: 后端来源选择（无 `@backend` 指令时为继承当前配置）；
/// - `backend_explicit`: 输入是否显式指定了后端来源或独立配置项，
///   为 `false` 时由调用方决定的默认来源生效；
/// - `input`: 人格预设、对话轮次与问题。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedLineInput {
    pub backend: SandboxBackendChoice,
    pub backend_explicit: bool,
    pub input: SandboxInput,
}

/// 头部指令名表（顺序无关，重复出现按错误处理）。
const DIRECTIVE_NAMES: [&str; 5] = ["@persona", "@backend", "@base-url", "@model", "@embedding"];

/// 解析行模式输入（头部指令 + 对话 + `---` + 问题）。
///
/// 参数:
/// - `text`: 标准输入全文。
///
/// 返回:
/// - 成功时返回后端来源选择与对话输入；
/// - 未知指令、指令缺参数 / 重复、继承模式带独立配置项、缺少 `---`、
///   对话为空或问题为空时返回带位置或原因的文案。
///
/// 说明:
/// - 指令只在对话块之前的连续头部区内生效，遇到第一条非指令行即结束；
/// - `@backend inherit` 与 `@base-url` / `@model` / `@embedding` 互斥。
pub fn parse_line_input(text: &str) -> Result<ParsedLineInput, String> {
    let lines: Vec<&str> = text.lines().collect();

    let mut cursor = 0;
    while cursor < lines.len() && lines[cursor].trim().is_empty() {
        cursor += 1;
    }

    let mut persona_path: Option<PathBuf> = None;
    let mut backend_mode: Option<SandboxBackendMode> = None;
    let mut backend_explicit = false;
    let mut base_url: Option<String> = None;
    let mut model_id: Option<String> = None;
    let mut embedding_path: Option<PathBuf> = None;

    while let Some(line) = lines.get(cursor) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            cursor += 1;
            continue;
        }
        if !trimmed.starts_with('@') {
            break;
        }

        let line_number = cursor + 1;
        let (name, value) = split_directive(trimmed);
        if !DIRECTIVE_NAMES.contains(&name) {
            return Err(format!(
                "第 {line_number} 行未知指令 {name}；{LINE_MODE_HINT}"
            ));
        }
        if value.is_empty() {
            return Err(format!(
                "第 {line_number} 行 {name} 缺少参数；{LINE_MODE_HINT}"
            ));
        }

        match name {
            "@persona" => {
                reject_duplicate(&persona_path, line_number, name)?;
                persona_path = Some(PathBuf::from(value));
            }
            "@backend" => {
                reject_duplicate(&backend_mode, line_number, name)?;
                backend_mode = Some(parse_backend_mode(value, line_number)?);
                backend_explicit = true;
            }
            "@base-url" => {
                reject_duplicate(&base_url, line_number, name)?;
                base_url = Some(value.to_string());
                backend_explicit = true;
            }
            "@model" => {
                reject_duplicate(&model_id, line_number, name)?;
                model_id = Some(value.to_string());
                backend_explicit = true;
            }
            _ => {
                reject_duplicate(&embedding_path, line_number, name)?;
                embedding_path = Some(PathBuf::from(value));
                backend_explicit = true;
            }
        }
        cursor += 1;
    }

    let mode = backend_mode.unwrap_or_default();
    let backend = match mode {
        SandboxBackendMode::Inherit => {
            if base_url.is_some() || model_id.is_some() || embedding_path.is_some() {
                return Err(
                    "继承模式不接受独立配置：先写 @backend local 或 @backend cloud".to_string(),
                );
            }
            SandboxBackendChoice::Inherit
        }
        SandboxBackendMode::Local | SandboxBackendMode::Cloud => {
            let kind = match mode {
                SandboxBackendMode::Local => SandboxModelKind::Local,
                _ => SandboxModelKind::Cloud,
            };
            SandboxBackendChoice::Independent {
                kind,
                base_url: base_url.unwrap_or_default(),
                model_id: model_id.unwrap_or_default(),
                embedding_model_path: embedding_path.take(),
            }
        }
    };

    let dialogue_start = cursor;
    let mut separator = None;
    for (index, line) in lines.iter().enumerate().skip(dialogue_start) {
        if line.trim() == "---" {
            separator = Some(index);
            break;
        }
    }
    let Some(separator) = separator else {
        return Err(format!("缺少 --- 分隔线；{LINE_MODE_HINT}"));
    };

    let dialogue_text = lines[dialogue_start..separator].join("\n");
    let turns = parse_dialogue_from(&dialogue_text, dialogue_start)?;
    if turns.is_empty() {
        return Err("对话为空；请在 --- 之前写 user:/assistant: 对话".to_string());
    }

    let question = lines[separator + 1..].join("\n").trim().to_string();
    if question.is_empty() {
        return Err("问题为空；请在 --- 之后填写要提问的内容".to_string());
    }

    Ok(ParsedLineInput {
        backend,
        backend_explicit,
        input: SandboxInput {
            persona_path,
            turns,
            question,
        },
    })
}

/// 把一行头部切为指令名与参数文本（参数取首个空白之后的部分）。
fn split_directive(line: &str) -> (&str, &str) {
    let mut parts = line.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let value = parts.next().unwrap_or("").trim();
    (name, value)
}

/// 同一指令重复出现时报错（行号定位到后出现的那一行）。
fn reject_duplicate<T>(slot: &Option<T>, line_number: usize, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!(
            "第 {line_number} 行 {name} 重复出现；{LINE_MODE_HINT}"
        ));
    }
    Ok(())
}

/// 解析 `@backend` 的取值。
fn parse_backend_mode(value: &str, line_number: usize) -> Result<SandboxBackendMode, String> {
    match value.to_ascii_lowercase().as_str() {
        "inherit" => Ok(SandboxBackendMode::Inherit),
        "local" => Ok(SandboxBackendMode::Local),
        "cloud" => Ok(SandboxBackendMode::Cloud),
        _ => Err(format!(
            "第 {line_number} 行 @backend 只接受 inherit / local / cloud，实际: {value}"
        )),
    }
}
