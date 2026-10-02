//! crates/ramaria-service/src/diagnostics/redact.rs - 敏感信息脱敏
//!
//! 设计特点:
//! - 两道防线：API key 收集阶段脱敏（`[REDACTED]`）+ 导出前二次脱敏（统一入口）
//! - 二次脱敏：消息类字段值 → `<N chars>`（不落原文）；绝对路径 → 只保留文件名
//! - 保留行结构与空白，便于人工阅读与定位；不依赖上游日志是否已做脱敏
//! - 脱敏原语同时供索引构建失败原因等诊断摘要文本复用（统一口径，避免两套实现）
//! - 路径判定排除 URL 协议段与词内斜杠，避免误伤（`https://...` 保持原样）

use std::path::Path;

// =========================================================
// API key 脱敏
// =========================================================

/// 对配置文件内容做 API key 脱敏。
///
/// 脱敏规则:
/// - 匹配模式: 行中包含 `api_key` 或 `apikey`（不区分大小写），且包含 `=`（赋值语句）。
/// - 将 `=` 右侧的内容替换为 ` "[REDACTED]"`。
/// - 不修改注释行（以 `#` 开头）。
///
/// 返回:
/// - 脱敏后的完整文本。
pub(super) fn redact_api_keys(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            // 跳过纯注释行
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                return line.to_string();
            }

            // 检测 api_key 或 apikey（不区分大小写）
            let lower = trimmed.to_lowercase();
            if lower.contains("api_key") || lower.contains("apikey") {
                // 找到 `=` 的位置
                if let Some(eq_pos) = trimmed.find('=') {
                    let key_part = &trimmed[..eq_pos + 1];
                    return format!("{key_part} \"[REDACTED]\"");
                }
            }

            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// =========================================================
// 内部实现: 导出前二次脱敏
// =========================================================

/// 消息类字段名（完整名，或 `xxx_preview` 形式的后缀名）。
///
/// 说明:
/// - 命中即把字段值替换为字符数占位 `<N chars>`，避免原文随诊断包外发。
/// - 不含 `*_len` 类长度字段（如 `msg_len=12` 本身已不含原文，保持可诊断性）。
const SENSITIVE_FIELD_MARKERS: &[&str] = &[
    "preview", "content", "text", "message", "msg", "reply", "input", "summary", "notes",
    "excerpt", "snippet",
];

/// 二次脱敏原语（日志 / 配置导出与诊断摘要的统一入口）。
///
/// 规则:
/// 1. 消息类字段值 → `<N chars>`（N 为字符数，不输出原文）；
/// 2. 绝对路径（Windows 盘符 / UNC / Unix 绝对路径）→ 仅保留最后一段（文件名）。
///
/// 说明:
/// - 保留行结构与空白，便于人工阅读与定位；
/// - 该函数是"最后一道防线"，不依赖上游日志是否已做脱敏；同时供索引构建失败原因等
///   诊断摘要文本的脱敏复用（统一口径，避免两套脱敏实现）；
/// - 字段值以空白分隔且未加引号时只能取到首个词（结构化日志的内容字段通常由
///   Debug 格式化加引号，可完整覆盖）。
pub(crate) fn redact_for_export(content: &str) -> String {
    content
        .split_inclusive('\n')
        .map(redact_line)
        .collect::<String>()
}

/// 单行脱敏：按空白切分 token（保留空白片段），逐 token 处理。
fn redact_line(line: &str) -> String {
    split_words_with_separators(line)
        .into_iter()
        .map(|(is_space, part)| {
            if is_space {
                part.to_string()
            } else {
                // 先按字段脱敏（可能吞掉带引号的值），再替换其中的绝对路径
                redact_paths_in(&redact_sensitive_field(part))
            }
        })
        .collect()
}

/// 按空白切分但保留空白片段（保证脱敏后行结构与空白原样保留）。
fn split_words_with_separators(line: &str) -> Vec<(bool, &str)> {
    let mut parts: Vec<(bool, &str)> = Vec::new();
    let mut start = 0usize;
    let mut current: Option<bool> = None;

    for (i, ch) in line.char_indices() {
        let is_space = ch.is_whitespace();
        if current == Some(!is_space) {
            parts.push((!is_space, &line[start..i]));
            start = i;
        }
        current = Some(is_space);
    }
    if start < line.len() {
        parts.push((current.unwrap_or(false), &line[start..]));
    }
    parts
}

/// 单个 token 的字段脱敏：`字段=值` 中字段命中敏感名单时，值替换为 `<N chars>`。
fn redact_sensitive_field(token: &str) -> String {
    let Some(eq) = token.find('=') else {
        return token.to_string();
    };
    let name = token[..eq]
        .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .to_ascii_lowercase();
    if !is_sensitive_field_name(&name) {
        return token.to_string();
    }

    let value = &token[eq + 1..];
    if value.is_empty() {
        return token.to_string();
    }

    // 值可能是 Debug `?` 格式化的引号包裹，也可能是 Display `%` 的裸文本
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    let inner = if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    };
    let count = inner.chars().count();
    let prefix = &token[..eq];
    if quoted {
        format!("{prefix}=\"<{count} chars>\"")
    } else {
        format!("{prefix}=<{count} chars>")
    }
}

/// 字段名是否命中敏感名单（完整名，或 `xxx_preview` 形式的后缀名）。
fn is_sensitive_field_name(name: &str) -> bool {
    SENSITIVE_FIELD_MARKERS
        .iter()
        .any(|marker| name == *marker || name.ends_with(&format!("_{marker}")))
}

/// 替换 token 内的绝对路径为文件名（Windows 盘符 / UNC / Unix 绝对路径）。
fn redact_paths_in(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut i = 0usize;

    while i < token.len() {
        if let Some(len) = absolute_path_len_at(token, i) {
            out.push_str(&path_file_name(&token[i..i + len]));
            i += len;
            continue;
        }
        // 不是路径起点：按 UTF-8 字符整体推进（不切开多字节字符）
        let ch_len = token[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&token[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// 判断 `s[i..]` 是否以绝对路径开头；是则返回该路径片段长度（字节）。
///
/// 判定:
/// - 路径必须起始于边界（token 起点或空白/引号/等号/括号等之后），
///   避免把 URL（`https://...`）中的路径段误判为本地路径；
/// - Windows：`X:\` / `X:/`；UNC：`\\server\share`；
/// - Unix：`/xxx`（排除 `//` 与 `/` 后紧跟空白）。
fn absolute_path_len_at(s: &str, i: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if i != 0 && !is_path_boundary(bytes[i - 1]) {
        return None;
    }

    let windows_drive = i + 2 < bytes.len()
        && bytes[i].is_ascii_alphabetic()
        && bytes[i + 1] == b':'
        && (bytes[i + 2] == b'\\' || bytes[i + 2] == b'/');
    let unc = bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\';
    let unix = bytes[i] == b'/'
        && i + 1 < bytes.len()
        && bytes[i + 1] != b'/'
        && !bytes[i + 1].is_ascii_whitespace();

    if !(windows_drive || unc || unix) {
        return None;
    }

    let mut end = i;
    while end < bytes.len() && !is_path_terminator(bytes[end]) {
        end += 1;
    }
    // 仅取到前缀/单个分隔符（如裸 "C:\"）时视为非路径，保持原文
    if end <= i + 1 {
        return None;
    }
    Some(end - i)
}

/// 路径起点允许的前导字节（排除词内斜杠与 URL 协议段）。
fn is_path_boundary(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'"'
            | b'\''
            | b'='
            | b'('
            | b'['
            | b'{'
            | b','
            | b':'
            | b'<'
            | b'`'
            | b'|'
    )
}

/// 路径终止字节（路径片段到此为止，后续字符原样保留）。
fn is_path_terminator(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'"' | b'\''
                | b','
                | b')'
                | b']'
                | b'}'
                | b'>'
                | b'<'
                | b'|'
                | b'*'
                | b'?'
                | b';'
                | b'`'
        )
}

/// 取路径最后一段（文件名）；空路径（如 `/`）返回 `<path>` 占位。
fn path_file_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    if last.is_empty() {
        "<path>".to_string()
    } else {
        last.to_string()
    }
}

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
pub(super) fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}
