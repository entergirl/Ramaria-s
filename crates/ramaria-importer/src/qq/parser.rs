//! rust/crates/ramaria-importer/src/qq/parser.rs - QQ 聊天记录解析核心
//!
//! 设计特点:
//! - 适配两种格式: shuakami/qq-chat-exporter v5.x JSON + PC QQ 经典 .txt 导出
//! - JSON 格式: 完整解析 chatInfo、messages 数组，支持 6 种已知消息类型
//! - .txt 格式: 按时间戳行切分消息，支持多行消息合并
//! - 消息指纹: SHA-256 前 16 位 hex，用于跨导入批次的重复检测
//! - 编码兼容: 支持 UTF-8/UTF-8-BOM/GBK/Latin-1 多编码自动检测
//! - 角色映射: 导出者消息→role=user，对方消息→role=assistant+名称前缀
//! - Session 切割: 按 gap_minutes 时间间隔将消息流切割为独立会话
//! - 完整诊断: 报告包含成功/降级/跳过 三类统计及详细条目

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use ramaria_core::error::RamariaResult;
use sha2::{Digest, Sha256};

use crate::error;
use crate::traits::{ImportReport, ImportedSession, ParsedMessage};

// =========================================================
// QQ JSON 消息类型常量
// =========================================================

/// 普通文本消息（可能含图片元素）。
const TYPE_TEXT: &str = "type_1";
/// 回复/引用消息。
const TYPE_REPLY: &str = "type_3";
/// 语音消息。
const TYPE_AUDIO: &str = "type_6";
/// 卡片消息（名片等）。
const TYPE_CARD: &str = "type_7";
/// 视频消息。
const TYPE_VIDEO: &str = "type_9";
/// 合并转发消息。
const TYPE_FORWARD: &str = "type_11";

/// 图片元素类型标识。
const ELEM_IMAGE: &str = "image";
/// 回复引用元素类型标识。
const ELEM_REPLY: &str = "reply";

// =========================================================
// 格式检测
// =========================================================

/// 检测文件是否为 QQ 聊天记录格式。
///
/// 检测顺序:
/// 1. 先尝试 JSON 解析（qq-chat-exporter）。
/// 2. JSON 解析失败时，尝试 `.txt` 格式检测。
///
/// 返回:
/// - `true`: 文件可被 QQ parser 解析。
/// - `false`: 文件格式不匹配。
pub fn detect_qq_format(file_path: &Path) -> RamariaResult<bool> {
    // 先检查文件扩展名
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 如果扩展名明确不是 .json 或 .txt，快速拒绝
    if ext != "json" && ext != "txt" {
        // 仍尝试读取内容判断（有些导出文件无扩展名）
    }

    // 读取文件头部进行检测
    let content = match fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(_) => {
            // 尝试二进制读取
            let bytes = fs::read(file_path)
                .map_err(|e| error::read_error(&file_path.display().to_string(), e))?;

            // 检测 UTF-8 BOM
            if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
                match String::from_utf8(bytes[3..].to_vec()) {
                    Ok(s) => s,
                    Err(_) => return Ok(false),
                }
            } else {
                // 尝试 GBK
                match encoding_rs::GBK.decode(&bytes) {
                    (s, _, false) => s.into_owned(),
                    _ => return Ok(false),
                }
            }
        }
    };

    let trimmed = content.trim();

    // 检测 JSON 格式：以 { 开头且包含 chatInfo/messages
    if trimmed.starts_with('{')
        && trimmed.contains("\"chatInfo\"")
        && trimmed.contains("\"messages\"")
    {
        return Ok(true);
    }

    // 检测 .txt 格式：第一行匹配时间戳模式
    // 格式: YYYY-MM-DD HH:MM:SS 发送者名
    if is_txt_timestamp_line(trimmed.lines().next().unwrap_or("")) {
        return Ok(true);
    }

    Ok(false)
}

/// 检测文本行是否为 `.txt` 格式的时间戳行。
///
/// 格式: `YYYY-MM-DD HH:MM:SS <name>` 或 `YYYY/MM/DD HH:MM:SS <name>`
///
/// 安全约束: 时间戳部分（前 19 字节）必须是纯 ASCII，否则非时间戳行。
fn is_txt_timestamp_line(line: &str) -> bool {
    let line = line.trim();
    // 时间戳行最短: "YYYY-MM-DD HH:MM:SS " = 20 字节（均为 ASCII）
    if line.len() < 20 {
        return false;
    }

    // 安全校验: 前 20 字节必须在 UTF-8 边界上（纯 ASCII 的最短前缀）
    // 如果不是，说明包含多字节字符，不可能是时间戳行
    if !line.is_char_boundary(10) || !line.is_char_boundary(19) || !line.is_char_boundary(20) {
        return false;
    }

    // 检查日期部分: YYYY-MM-DD 或 YYYY/MM/DD
    let date_part = &line[..10];
    let date_sep = if date_part.chars().nth(4) == Some('-') {
        '-'
    } else if date_part.chars().nth(4) == Some('/') {
        '/'
    } else {
        return false;
    };

    // 验证年份（4 位数字）
    if !date_part[..4].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // 验证分隔符
    if date_part.chars().nth(4) != Some(date_sep) || date_part.chars().nth(7) != Some(date_sep) {
        return false;
    }
    // 验证月日（各 2 位数字）
    if !date_part[5..7].chars().all(|c| c.is_ascii_digit())
        || !date_part[8..10].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }

    // 检查时间部分: HH:MM:SS
    let time_part = &line[11..19];
    if time_part.chars().nth(2) != Some(':') || time_part.chars().nth(5) != Some(':') {
        return false;
    }
    if !time_part[..2].chars().all(|c| c.is_ascii_digit())
        || !time_part[3..5].chars().all(|c| c.is_ascii_digit())
        || !time_part[6..8].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }

    // 时间后面应有空格和发送者名
    if line.as_bytes().get(19) != Some(&b' ') {
        return false;
    }

    true
}

// =========================================================
// 工具函数
// =========================================================

/// 将 Unix 毫秒时间戳格式化为日期字符串（YYYY-MM-DD）。
fn ts_ms_to_date(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    // 使用简单的算术转换，避免依赖 chrono 时区
    let days_since_epoch = secs / 86400;
    let mut y = 1970i64;
    let mut remaining_days = days_since_epoch;

    // 计算年份
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }

    // 计算月份和日期
    let month_days = if is_leap(y) {
        MONTH_DAYS_LEAP
    } else {
        MONTH_DAYS
    };
    let mut m = 0usize;
    while m < 12 && remaining_days >= month_days[m] {
        remaining_days -= month_days[m];
        m += 1;
    }
    let month = m + 1;
    let day = remaining_days + 1;

    format!("{y:04}-{month:02}-{day:02}")
}

/// 闰年判断。
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// 每月天数（非闰年）。
const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
/// 每月天数（闰年）。
const MONTH_DAYS_LEAP: [i64; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// 判断 elements 列表中是否包含图片类型的元素。
fn has_image_element(elements: &[serde_json::Value]) -> bool {
    elements
        .iter()
        .any(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_IMAGE))
}

/// 从 elements 列表中提取 type=reply 的元素数据。
fn get_reply_element(elements: &[serde_json::Value]) -> Option<serde_json::Value> {
    elements
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_REPLY))
        .and_then(|e| e.get("data").cloned())
}

/// 将导出工具生成的图片占位符统一替换为 [图片]。
///
/// 示例:
/// - `[图片: abc123]` → `[图片]`
/// - `[图片: 1234567890abcdef.jpg]` → `[图片]`
fn clean_image_placeholders(text: &str) -> String {
    // 按 UTF-8 字符边界安全地查找并替换 [图片: ...] 占位符
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // 查找 '[' 后跟 "图片:"
        if chars[i] == '['
            && i + 3 < chars.len()
            && chars[i + 1] == '图'
            && chars[i + 2] == '片'
            && chars[i + 3] == ':'
        {
            // 找到匹配的 ']'
            if let Some(end) = chars[i..].iter().position(|&c| c == ']') {
                result.push_str("[图片]");
                i += end + 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result.trim().to_string()
}

/// 从 type_3 消息的 content.text 中提取回复正文（去掉引用头部）。
///
/// 作为降级处理，当 elements 里找不到 reply 元素时调用。
fn extract_reply_body(content_text: &str) -> String {
    // 尝试按换行分割，取第二行及之后的内容
    if let Some(pos) = content_text.find('\n') {
        let body = content_text[pos + 1..].trim();
        if !body.is_empty() {
            return body.to_string();
        }
    }

    // 尝试去掉 [回复...] 前缀
    if let Some(stripped) = content_text.strip_prefix('[')
        && let Some(end) = stripped.find(']')
    {
        let after = stripped[end + 1..].trim();
        if !after.is_empty() && after != content_text {
            return after.to_string();
        }
    }

    content_text.to_string()
}

/// 计算消息唯一指纹（SHA-256 前 16 位 hex）。
///
/// 参数:
/// - `original_ts`: 原始 Unix 毫秒时间戳。
/// - `role`: 消息角色。
/// - `content`: 消息正文。
///
/// 返回:
/// - 16 位 hex 字符串。
fn make_fingerprint(original_ts: i64, role: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{original_ts}|{role}|{content}").as_bytes());
    let result = hasher.finalize();
    // 前 8 字节 → 16 位 hex
    result[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

// =========================================================
// JSON 格式：单条消息解析
// =========================================================

/// 解析单条 JSON 原始消息，返回 ParsedMessage 或 None（跳过时）。
///
/// 解析规则（按优先级）:
/// 1. 撤回消息 → 跳过
/// 2. content.text 为空 → 跳过
/// 3. 根据 type 分流处理:
///    - type_1: 纯文本（可能含图片元素）
///    - type_3: 回复/引用消息
///    - type_6: 语音 → [语音]
///    - type_7: 卡片 → [卡片消息]
///    - type_9: 视频 → [视频]
///    - type_11: 转发 → [转发消息]
///    - 未知 type → 跳过
/// 4. 角色映射: 发送者==导出者→user，否则→assistant+[名称]前缀
fn parse_json_message(
    raw_msg: &serde_json::Value,
    self_uid: &str,
    report: &mut ImportReport,
) -> Option<ParsedMessage> {
    let timestamp = raw_msg
        .get("timestamp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let time_str = raw_msg
        .get("time")
        .and_then(|v| v.as_str())
        .unwrap_or("未知时间");
    let msg_type = raw_msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let recalled = raw_msg
        .get("recalled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = raw_msg.get("content");
    let sender = raw_msg.get("sender");

    let elements = content
        .and_then(|c| c.get("elements"))
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    let raw_text = content
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let sender_uid = sender
        .and_then(|s| s.get("uid"))
        .and_then(|u| u.as_str())
        .unwrap_or("");
    let sender_name = sender
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");

    // 规则1：撤回消息直接跳过
    if recalled {
        report.skipped_recalled += 1;
        tracing::debug!(time = %time_str, "跳过撤回消息");
        return None;
    }

    // 规则2：content.text 完全为空，跳过
    if raw_text.is_empty() {
        report.skipped_empty += 1;
        tracing::debug!(time = %time_str, "跳过空消息");
        return None;
    }

    // 规则3：根据 type 分流
    let final_text = match msg_type {
        TYPE_TEXT => {
            if has_image_element(&elements) {
                let cleaned = clean_image_placeholders(&raw_text);
                let result = if cleaned.is_empty() {
                    "[图片]".to_string()
                } else {
                    cleaned
                };
                report.success_image += 1;
                result
            } else {
                report.success_text += 1;
                raw_text.clone()
            }
        }

        TYPE_REPLY => {
            if let Some(reply_elem) = get_reply_element(&elements) {
                let quoted_sender = reply_elem
                    .get("senderName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let quoted_content = reply_elem
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let reply_body = extract_reply_body(&raw_text);

                // 截断过长的引用内容
                let quoted_display = if quoted_content.chars().count() > 30 {
                    format!("{}…", &quoted_content.chars().take(30).collect::<String>())
                } else {
                    quoted_content.to_string()
                };

                let result = format!("「回复 {quoted_sender}: {quoted_display}」{reply_body}");
                report.success_reply += 1;
                result
            } else {
                // 降级：无 reply 元素，尝试提取正文
                report.degraded_reply_fallback += 1;
                tracing::debug!(time = %time_str, "回复消息无reply元素，降级提取正文");
                extract_reply_body(&raw_text)
            }
        }

        TYPE_AUDIO => {
            report.degraded_audio += 1;
            tracing::debug!(time = %time_str, "语音消息→[语音]");
            "[语音]".to_string()
        }

        TYPE_VIDEO => {
            report.degraded_video += 1;
            tracing::debug!(time = %time_str, "视频消息→[视频]");
            "[视频]".to_string()
        }

        TYPE_FORWARD => {
            report.degraded_forward += 1;
            tracing::debug!(time = %time_str, "合并转发→[转发消息]");
            "[转发消息]".to_string()
        }

        TYPE_CARD => {
            report.degraded_card += 1;
            tracing::debug!(time = %time_str, "卡片消息→[卡片消息]");
            "[卡片消息]".to_string()
        }

        other => {
            report.skipped_unknown += 1;
            if !report.unknown_types.contains(&other.to_string()) {
                report.unknown_types.push(other.to_string());
            }
            tracing::warn!(msg_type = %other, time = %time_str, "未知消息类型，跳过");
            return None;
        }
    };

    // 规则4：role 映射
    let (role, content_final) = if sender_uid == self_uid {
        ("user".to_string(), final_text)
    } else {
        // 对方消息：加前缀
        let prefix = if sender_name.is_empty() {
            "[对方] ".to_string()
        } else {
            format!("[{sender_name}] ")
        };
        // 统计对方发言（仅纯文本和回复）
        if matches!(msg_type, TYPE_TEXT | TYPE_REPLY)
            && (msg_type != TYPE_REPLY || get_reply_element(&elements).is_some())
        {
            report.success_other_sender += 1;
        }
        ("assistant".to_string(), format!("{prefix}{final_text}"))
    };

    let fingerprint = make_fingerprint(timestamp, &role, &content_final);

    Some(ParsedMessage {
        role,
        content: content_final,
        created_at: timestamp,
        fingerprint,
    })
}

// =========================================================
// TXT 格式：消息解析
// =========================================================

/// 解析 .txt 格式的 QQ 聊天记录。
///
/// 格式示例:
/// ```text
/// 2024-01-01 12:00:00 张三
/// 这是消息内容
/// 可以有多行
///
/// 2024-01-01 12:01:00 李四
/// 这是第二条消息
/// ```
///
/// 解析规则:
/// - 以 `YYYY-MM-DD HH:MM:SS <name>` 开头的行标记新消息开始
/// - 直至下一条时间戳行之前的所有行都属于当前消息
/// - 空行保留在消息内容中
/// - 无法识别发送者时，所有消息 role=assistant
fn parse_txt_messages(
    content: &str,
    self_name_opt: Option<&str>,
    report: &mut ImportReport,
) -> Vec<ParsedMessage> {
    let mut messages: Vec<ParsedMessage> = Vec::new();
    let mut current_timestamp: Option<i64> = None;
    let mut current_sender: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();

    // 辅助函数：flush 当前消息
    let flush = |ts: i64,
                 sender: &Option<String>,
                 lines: &mut Vec<String>,
                 msgs: &mut Vec<ParsedMessage>,
                 report: &mut ImportReport| {
        if lines.is_empty() {
            return;
        }

        let body = lines.join("\n").trim().to_string();
        if body.is_empty() {
            lines.clear();
            return;
        }

        let sender_name = sender.as_deref().unwrap_or("");
        let is_self = self_name_opt.map(|n| n == sender_name).unwrap_or(false);

        let (role, content_final) = if is_self {
            ("user".to_string(), body.clone())
        } else {
            let prefix = if sender_name.is_empty() {
                "[对方] ".to_string()
            } else {
                format!("[{sender_name}] ")
            };
            ("assistant".to_string(), format!("{prefix}{body}"))
        };

        let fingerprint = make_fingerprint(ts, &role, &content_final);

        msgs.push(ParsedMessage {
            role,
            content: content_final,
            created_at: ts,
            fingerprint,
        });

        report.success_text += 1;

        lines.clear();
    };

    for line in content.lines() {
        if is_txt_timestamp_line(line) {
            // flush 当前消息
            if let Some(ts) = current_timestamp {
                flush(
                    ts,
                    &current_sender,
                    &mut current_lines,
                    &mut messages,
                    report,
                );
            }

            // 解析时间戳和发送者
            current_timestamp = parse_txt_timestamp(line);
            current_sender = parse_txt_sender(line);
        } else if current_timestamp.is_some() {
            // 当前消息的续行
            current_lines.push(line.to_string());
        }
        // 没有当前消息时忽略行（文件头部的空行等）
    }

    // flush 最后一条消息
    if let Some(ts) = current_timestamp {
        flush(
            ts,
            &current_sender,
            &mut current_lines,
            &mut messages,
            report,
        );
    }

    if messages.is_empty() {
        report.warnings.push("未解析出任何有效消息".to_string());
    }

    messages
}

/// 从 .txt 时间戳行中解析 Unix 毫秒时间戳。
///
/// 支持格式: `YYYY-MM-DD HH:MM:SS` 和 `YYYY/MM/DD HH:MM:SS`
///
/// 返回:
/// - Unix 毫秒时间戳；解析失败时返回 0。
fn parse_txt_timestamp(line: &str) -> Option<i64> {
    let line = line.trim();
    if line.len() < 19 {
        return None;
    }

    let date_str = &line[..10];
    let time_str = &line[11..19];

    // 解析年
    let year: i32 = date_str[..4].parse().ok()?;
    // 支持 - 和 / 分隔符
    let delim = date_str.chars().nth(4)?;
    let month: u32 = date_str[5..7].parse().ok()?;
    let day: u32 = date_str[8..10].parse().ok()?;
    if delim != '-' && delim != '/' {
        return None;
    }

    let hour: u32 = time_str[..2].parse().ok()?;
    let minute: u32 = time_str[3..5].parse().ok()?;
    let second: u32 = time_str[6..8].parse().ok()?;

    // 基本范围验证
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    // 计算自 epoch 以来的天数
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap(y as i64) { 366 } else { 365 };
    }
    let month_days = if is_leap(year as i64) {
        MONTH_DAYS_LEAP
    } else {
        MONTH_DAYS
    };
    for &md in month_days.iter().take((month as usize).saturating_sub(1)) {
        days += md;
    }
    days += day as i64 - 1;

    let secs = days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    Some(secs * 1000)
}

/// 从 .txt 时间戳行中提取发送者名称。
fn parse_txt_sender(line: &str) -> Option<String> {
    let line = line.trim();
    if line.len() <= 20 {
        return None;
    }
    let name = line[20..].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

// =========================================================
// Session 切割
// =========================================================

/// 按时间间隔将消息列表切割为若干 session。
///
/// 参数:
/// - `messages`: 已按时间排序的消息列表。
/// - `gap_ms`: 时间间隔阈值（毫秒），超出此间隔即切断为新 session。
///
/// 返回:
/// - 切割后的 session 列表。
fn split_into_sessions(messages: &[ParsedMessage], gap_ms: i64) -> Vec<ImportedSession> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut sessions: Vec<ImportedSession> = Vec::new();
    let mut current: Vec<ParsedMessage> = vec![messages[0].clone()];

    for msg in &messages[1..] {
        let last_ts = current.last().map(|m| m.created_at).unwrap_or(0);
        if msg.created_at - last_ts > gap_ms {
            // 时间间隔过大，切断 session
            let started_at = current.first().map(|m| m.created_at).unwrap_or(0);
            let ended_at = current.last().map(|m| m.created_at).unwrap_or(0);
            sessions.push(ImportedSession {
                messages: std::mem::take(&mut current),
                started_at,
                ended_at,
            });
            current.push(msg.clone());
        } else {
            current.push(msg.clone());
        }
    }

    // 最后一个 session
    if !current.is_empty() {
        let started_at = current.first().map(|m| m.created_at).unwrap_or(0);
        let ended_at = current.last().map(|m| m.created_at).unwrap_or(0);
        sessions.push(ImportedSession {
            messages: current,
            started_at,
            ended_at,
        });
    }

    sessions
}

// =========================================================
// 主解析函数（对外接口）
// =========================================================

/// 解析 QQ 聊天记录文件（JSON 或 .txt）。
///
/// 这是 QQ 导入器的唯一对外解析接口。
///
/// 参数:
/// - `file_path`: 文件路径。
/// - `gap_minutes`: session 切割时间间隔阈值（分钟），默认 10。
///
/// 返回:
/// - `(sessions, report)`: 解析后的 session 列表和诊断报告。
///
/// 错误:
/// - 文件不存在。
/// - 文件格式不匹配。
/// - JSON 解析失败。
/// - 没有解析出任何有效消息。
pub fn parse_qq_export(
    file_path: &Path,
    gap_minutes: u32,
) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
    if !file_path.exists() {
        return Err(error::file_not_found(&file_path.display().to_string()));
    }

    let gap_ms = (gap_minutes as i64) * 60 * 1000;

    // 读取文件内容进行格式判断
    let bytes =
        fs::read(file_path).map_err(|e| error::read_error(&file_path.display().to_string(), e))?;

    // 检测是否为 JSON 格式
    let is_json = bytes.contains(&b'{');

    if is_json {
        parse_qq_json(bytes, file_path, gap_ms)
    } else {
        parse_qq_txt(&bytes, file_path, gap_ms)
    }
}

/// 解析 JSON 格式的 QQ 聊天记录。
fn parse_qq_json(
    bytes: Vec<u8>,
    file_path: &Path,
    gap_ms: i64,
) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
    // 解码 JSON
    let json_str = decode_bytes(&bytes, file_path)?;
    let raw_data: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| error::json_parse_error(&file_path.display().to_string(), &e.to_string()))?;

    // 校验格式
    let chat_info = match raw_data.get("chatInfo") {
        Some(ci) => ci,
        None => {
            return Err(error::format_mismatch(
                "QQ Chat Exporter JSON（含 chatInfo 字段）",
                "请确认文件是由 shuakami/qq-chat-exporter 导出的 JSON 格式。",
            ));
        }
    };
    let raw_messages = match raw_data.get("messages").and_then(|m| m.as_array()) {
        Some(msgs) => msgs,
        None => {
            return Err(error::format_mismatch(
                "QQ Chat Exporter JSON（含 messages 数组）",
                "文件中缺少 messages 字段。",
            ));
        }
    };

    let self_uid = chat_info
        .get("selfUid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let self_name = chat_info
        .get("selfName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let chat_name = chat_info.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let chat_type = chat_info
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let mut report = ImportReport {
        file_path: file_path.display().to_string(),
        self_id: self_uid.to_string(),
        self_name: self_name.to_string(),
        chat_name: chat_name.to_string(),
        chat_type: chat_type.to_string(),
        total_raw: raw_messages.len(),
        gap_minutes: (gap_ms / 60_000) as u32,
        ..Default::default()
    };

    tracing::info!(
        file = %report.file_path,
        self_name = %report.self_name,
        chat_name = %report.chat_name,
        chat_type = %report.chat_type,
        total_raw = report.total_raw,
        "开始解析 QQ JSON 聊天记录"
    );

    // 文件内去重（以 id + timestamp 为联合键）
    let mut seen_keys: HashSet<(String, i64)> = HashSet::new();
    let mut deduped_msgs: Vec<&serde_json::Value> = Vec::new();
    for msg in raw_messages {
        let key = (
            msg.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            msg.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0),
        );
        if seen_keys.contains(&key) {
            report.dedup_removed += 1;
            continue;
        }
        seen_keys.insert(key);
        deduped_msgs.push(msg);
    }

    // 按时间戳排序
    deduped_msgs.sort_by_key(|m| m.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0));

    // 逐条解析
    let mut parsed_messages: Vec<ParsedMessage> = Vec::new();
    for raw_msg in &deduped_msgs {
        if let Some(parsed) = parse_json_message(raw_msg, self_uid, &mut report) {
            parsed_messages.push(parsed);
        }
    }

    // 切割 session
    let sessions = split_into_sessions(&parsed_messages, gap_ms);
    report.session_count = sessions.len();

    // 更新时间范围
    if !parsed_messages.is_empty() {
        report.time_start = ts_ms_to_date(parsed_messages.first().unwrap().created_at);
        report.time_end = ts_ms_to_date(parsed_messages.last().unwrap().created_at);
    }

    tracing::info!(
        sessions = report.session_count,
        success = report.total_success(),
        degraded = report.total_degraded(),
        skipped = report.total_skipped(),
        "QQ JSON 解析完成"
    );

    if sessions.is_empty() {
        report
            .warnings
            .push("未解析出任何有效消息（全部被跳过或不支持）".to_string());
    }

    Ok((sessions, report))
}

/// 解析 .txt 格式的 QQ 聊天记录。
fn parse_qq_txt(
    bytes: &[u8],
    file_path: &Path,
    gap_ms: i64,
) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
    let content = decode_bytes(bytes, file_path)?;

    let mut report = ImportReport {
        file_path: file_path.display().to_string(),
        self_id: String::new(),
        self_name: String::new(),
        chat_name: String::new(),
        chat_type: "txt_export".to_string(),
        gap_minutes: (gap_ms / 60_000) as u32,
        ..Default::default()
    };

    // .txt 格式尝试推导导出者名称（第一条消息的发送者通常为导出者）
    // 先扫描找出第一条消息的发送者
    let first_sender = content
        .lines()
        .find(|line| is_txt_timestamp_line(line))
        .and_then(parse_txt_sender);

    tracing::info!(
        file = %report.file_path,
        first_sender = ?first_sender,
        "开始解析 QQ TXT 聊天记录"
    );

    report.self_name = first_sender.clone().unwrap_or_default();

    let parsed_messages = parse_txt_messages(&content, first_sender.as_deref(), &mut report);
    report.total_raw = parsed_messages.len();

    // 切割 session
    let sessions = split_into_sessions(&parsed_messages, gap_ms);
    report.session_count = sessions.len();

    // 更新时间范围
    if !parsed_messages.is_empty() {
        report.time_start = ts_ms_to_date(parsed_messages.first().unwrap().created_at);
        report.time_end = ts_ms_to_date(parsed_messages.last().unwrap().created_at);
    }

    tracing::info!(
        sessions = report.session_count,
        success = report.total_success(),
        "QQ TXT 解析完成"
    );

    if sessions.is_empty() {
        report
            .warnings
            .push("未解析出任何有效消息（全部被跳过或不支持）".to_string());
    }

    Ok((sessions, report))
}

/// 多编码尝试解码字节数组为字符串。
fn decode_bytes(bytes: &[u8], _file_path: &Path) -> RamariaResult<String> {
    // UTF-8
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        return Ok(s);
    }

    // UTF-8 BOM
    if bytes.len() >= 3
        && bytes[0] == 0xEF
        && bytes[1] == 0xBB
        && bytes[2] == 0xBF
        && let Ok(s) = String::from_utf8(bytes[3..].to_vec())
    {
        return Ok(s);
    }

    // UTF-16 LE
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        if let Ok(s) = String::from_utf16(&utf16) {
            return Ok(s);
        }
    }

    // GBK
    let (decoded, _, has_errors) = encoding_rs::GBK.decode(bytes);
    if !has_errors {
        return Ok(decoded.into_owned());
    }

    // Latin-1 兜底
    let latin1: String = bytes.iter().map(|&b| b as char).collect();
    Ok(latin1)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- 时间戳行检测 --

    #[test]
    fn detect_txt_timestamp_line_valid() {
        assert!(is_txt_timestamp_line("2024-01-01 12:00:00 张三"));
        assert!(is_txt_timestamp_line("2024/01/01 12:00:00 张三"));
        assert!(is_txt_timestamp_line("2023-12-31 23:59:59 李四"));
    }

    #[test]
    fn detect_txt_timestamp_line_invalid() {
        assert!(!is_txt_timestamp_line("这是一条普通消息"));
        assert!(!is_txt_timestamp_line("2024-01-01")); // 太短
        assert!(!is_txt_timestamp_line("2024-01-01 12:00")); // 缺秒
        assert!(!is_txt_timestamp_line("2024-01-01 12-00-00 张三")); // 分隔符错误
    }

    // -- 图片占位符清理 --

    #[test]
    fn clean_image_placeholder_replaces() {
        assert_eq!(clean_image_placeholders("[图片: abc123]"), "[图片]");
        assert_eq!(
            clean_image_placeholders("[图片: 1234567890abcdef.jpg]"),
            "[图片]"
        );
    }

    #[test]
    fn clean_image_placeholder_no_placeholder() {
        assert_eq!(clean_image_placeholders("普通消息"), "普通消息");
    }

    // -- 回复正文提取 --

    #[test]
    fn extract_reply_body_with_newline() {
        let input = "回复的头部信息\n这是真正的回复正文";
        assert_eq!(extract_reply_body(input), "这是真正的回复正文");
    }

    #[test]
    fn extract_reply_body_no_newline() {
        let input = "[回复某人] 这是正文";
        assert_eq!(extract_reply_body(input), "这是正文");
    }

    #[test]
    fn extract_reply_body_empty() {
        assert_eq!(extract_reply_body(""), "");
    }

    // -- 指纹计算 --

    #[test]
    fn fingerprint_deterministic() {
        let fp1 = make_fingerprint(1700000000000, "user", "你好");
        let fp2 = make_fingerprint(1700000000000, "user", "你好");
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    #[test]
    fn fingerprint_different_content() {
        let fp1 = make_fingerprint(1700000000000, "user", "你好");
        let fp2 = make_fingerprint(1700000000000, "user", "再见");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_different_role() {
        let fp1 = make_fingerprint(1700000000000, "user", "你好");
        let fp2 = make_fingerprint(1700000000000, "assistant", "你好");
        assert_ne!(fp1, fp2);
    }

    // -- Session 切割 --

    #[test]
    fn split_sessions_single() {
        let msgs = vec![
            make_test_msg("user", "消息1", 1000),
            make_test_msg("assistant", "消息2", 2000),
        ];
        let sessions = split_into_sessions(&msgs, 60000); // 60s gap
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].messages.len(), 2);
    }

    #[test]
    fn split_sessions_multi() {
        let msgs = vec![
            make_test_msg("user", "消息1", 1000),
            make_test_msg("assistant", "消息2", 2000),
            // 10 分钟后
            make_test_msg("user", "消息3", 602000),
            make_test_msg("assistant", "消息4", 603000),
        ];
        let sessions = split_into_sessions(&msgs, 60000); // 60s gap = 60000ms gap
        // gap between msg2(2000) and msg3(602000) = 600000ms > 60000ms, so should split
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].messages.len(), 2);
        assert_eq!(sessions[1].messages.len(), 2);
    }

    #[test]
    fn split_sessions_empty() {
        let sessions = split_into_sessions(&[], 60000);
        assert!(sessions.is_empty());
    }

    // -- TXT 解析 --

    #[test]
    fn parse_txt_basic() {
        let content = "2024-01-01 12:00:00 张三\n你好\n\n2024-01-01 12:01:00 李四\n你好呀";
        let mut report = ImportReport::default();
        let msgs = parse_txt_messages(content, Some("张三"), &mut report);

        assert_eq!(msgs.len(), 2);
        // 第一条是导出者本人（张三），role=user
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "你好");
        // 第二条是对方（李四），role=assistant，带名称前缀
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "[李四] 你好呀");
    }

    #[test]
    fn parse_txt_multiline() {
        let content = "2024-01-01 12:00:00 张三\n第一行\n第二行\n第三行";
        let mut report = ImportReport::default();
        let msgs = parse_txt_messages(content, Some("张三"), &mut report);

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "第一行\n第二行\n第三行");
    }

    #[test]
    fn parse_txt_no_self_name() {
        let content = "2024-01-01 12:00:00 张三\n你好\n\n2024-01-01 12:01:00 李四\n你好呀";
        let mut report = ImportReport::default();
        let msgs = parse_txt_messages(content, None, &mut report);

        // 无法识别导出者，全部为 assistant
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[1].role, "assistant");
    }

    // -- 日期转换 --

    #[test]
    fn ts_ms_to_date_known() {
        // 2024-01-01 00:00:00 UTC = 1704067200000 ms
        let date = ts_ms_to_date(1704067200000);
        assert_eq!(date, "2024-01-01");
    }

    #[test]
    fn ts_ms_to_date_epoch() {
        let date = ts_ms_to_date(0);
        assert_eq!(date, "1970-01-01");
    }

    // -- 辅助函数 --

    fn make_test_msg(role: &str, content: &str, created_at: i64) -> ParsedMessage {
        ParsedMessage {
            role: role.to_string(),
            content: content.to_string(),
            created_at,
            fingerprint: make_fingerprint(created_at, role, content),
        }
    }
}
