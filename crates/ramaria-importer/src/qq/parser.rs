//! rust/crates/ramaria-importer/src/qq/parser.rs - QQ 聊天记录解析核心
//!
//! 设计特点:
//! - 仅支持 shuakami/qq-chat-exporter v6.x JSON 格式（语义化 type 名称）
//! - 完整覆盖 qce v6.x 全部 10 种语义化消息类型（text/reply/audio/json/file/video/forward + type_10/type_19 + system）
//! - 消息指纹: SHA-256 前 16 位 hex，用于跨导入批次的重复检测
//! - 编码兼容: 支持 UTF-8/UTF-8-BOM/UTF-16-LE/GBK/Latin-1 多编码自动检测
//! - 角色映射: 双前缀模式——导出者也加 [{self_name}] 前缀，消除"用户 vs 助手"误导
//! - 对方标识: 直接从 chatInfo.peerUid/peerUin 提取，不再扫描消息列表
//! - Session 切割: 按 gap_minutes 时间间隔将消息流切割为独立会话
//! - 完整诊断: 报告包含成功/降级/跳过 三类统计

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use ramaria_core::error::RamariaResult;
use sha2::{Digest, Sha256};

use crate::error;
use crate::traits::{ImportReport, ImportedSession, ParsedMessage};

// =========================================================
// QQ JSON 消息类型常量（qce v6.x 语义化名称）
// =========================================================

/// 普通文本消息（可能含图片/表情元素）。qce v6.x: "text"
const TYPE_TEXT: &str = "text";
/// 回复/引用消息。qce v6.x: "reply"
const TYPE_REPLY: &str = "reply";
/// 语音消息。qce v6.x: "audio"
const TYPE_AUDIO: &str = "audio";
/// JSON/卡片/小程序/位置分享。qce v6.x: "json"
const TYPE_JSON: &str = "json";
/// 文件消息。qce v6.x: "file"
const TYPE_FILE: &str = "file";
/// 视频消息。qce v6.x: "video"
const TYPE_VIDEO: &str = "video";
/// 红包/转账消息。qce v6.x 保留原始编号: "type_10"
const TYPE_RED_ENVELOPE: &str = "type_10";
/// 合并转发消息。qce v6.x: "forward"
const TYPE_FORWARD: &str = "forward";
/// 通话记录。qce v6.x 保留原始编号: "type_19"
const TYPE_CALL: &str = "type_19";

/// 图片元素类型标识。
const ELEM_IMAGE: &str = "image";
/// 回复引用元素类型标识。
const ELEM_REPLY: &str = "reply";
/// JSON 卡片元素类型标识（用于提取 title/description 优化降级文本）。
const ELEM_JSON: &str = "json";

// =========================================================
// 格式检测
// =========================================================

/// 检测文件是否为 qq-chat-exporter 导出的 JSON 格式 QQ 聊天记录。
///
/// 检测方式:
/// - 读取文件内容（多编码尝试），判断是否以 `{` 开头且同时包含 `"chatInfo"` 和 `"messages"` 字段。
///
/// 返回:
/// - `true`: 文件是 qce v6.x JSON 格式。
/// - `false`: 文件格式不匹配。
pub fn detect_qq_format(file_path: &Path) -> RamariaResult<bool> {
    // 尝试以 UTF-8 读取；失败则走二进制多编码路径
    let content = match fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(_) => {
            let bytes = fs::read(file_path)
                .map_err(|e| error::read_error(&file_path.display().to_string(), e))?;

            // 检测 UTF-8 BOM → 去掉 BOM 后再转 UTF-8
            if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
                match String::from_utf8(bytes[3..].to_vec()) {
                    Ok(s) => s,
                    Err(_) => return Ok(false),
                }
            } else {
                // 尝试 GBK（部分中文环境可能以 GBK 编码保存）
                match encoding_rs::GBK.decode(&bytes) {
                    (s, _, false) => s.into_owned(),
                    _ => return Ok(false),
                }
            }
        }
    };

    let trimmed = content.trim();

    // 检测 qce JSON 格式特征：以 { 开头且同时含 chatInfo 和 messages 字段
    if trimmed.starts_with('{')
        && trimmed.contains("\"chatInfo\"")
        && trimmed.contains("\"messages\"")
    {
        return Ok(true);
    }

    Ok(false)
}

// =========================================================
// 工具函数
// =========================================================

/// 将 Unix 毫秒时间戳格式化为日期字符串（YYYY-MM-DD）。
///
/// 说明:
/// - 基于自 epoch（1970-01-01）以来的天数手动计算年月日，不依赖 chrono 时区。
/// - 严格按公历闰年规则计算。
fn ts_ms_to_date(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
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

/// 公历闰年判断。
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
fn reply_element(elements: &[serde_json::Value]) -> Option<serde_json::Value> {
    elements
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_REPLY))
        .and_then(|e| e.get("data").cloned())
}

/// 从 elements 列表中提取 JSON 卡片元素的描述文本。
///
/// 优先级:
/// 1. `data.description` — 卡片的描述摘要（如"牛脑发力！动画区玩谁是卧底..."）
/// 2. `data.title` — 卡片标题（如"[QQ小程序]牛脑发力！动画区玩..."）
///
/// 返回:
/// - `Some(description)` — 提取到的描述文本
/// - `None` — elements 中无 json 元素或 data 中无 description/title
fn json_element_description(elements: &[serde_json::Value]) -> Option<String> {
    elements
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_JSON))
        .and_then(|e| e.get("data"))
        .and_then(|data| {
            data.get("description")
                .and_then(|d| d.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    data.get("title")
                        .and_then(|t| t.as_str())
                        .filter(|s| !s.is_empty())
                })
        })
        .map(|s| s.to_string())
}

/// 将导出工具生成的图片占位符统一替换为 [图片]。
///
/// 动机:
/// - qce 将图片替换为 `[图片: HASH.jpg]` 格式的占位符，文件名因导出批次不同而变化。
/// - 统一为 [图片] 后，跨批次指纹一致，去重更准确。
///
/// 示例:
/// - `[图片: abc123]` → `[图片]`
/// - `[图片: 1234567890abcdef.jpg]` → `[图片]`
fn clean_image_placeholders(text: &str) -> String {
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

/// 从回复消息的 content.text 中提取回复正文（去掉引用头部）。
///
/// 作为降级处理，当 elements 里找不到 reply 元素时调用。
///
/// 策略（按优先级）:
/// 1. 按 '\n' 分割取第二行及之后 → 非空则返回
/// 2. 去掉 "[回复...]" 前缀取 ']' 后内容 → 非空且不等于原文则返回
/// 3. 返回原文（最坏情况，保留信息）
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
/// 输入: `{original_ts}|{role}|{content}`
///
/// 设计考量:
/// - 取前 8 字节（16 hex 字符）减少存储开销，碰撞概率极低
/// - 包含 role 维度，同一消息不同角色（自己/对方）的指纹不同
/// - 图片占位符已在调用前统一为 [图片]，确保跨批次一致
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
/// 1. `recalled == true` → 跳过（skipped_recalled）
/// 2. `system == true` → 跳过（skipped_system）
/// 3. `content.text` 为空 → 进一步检查 elements 和 type（防御性处理）
/// 4. 根据 type 分流处理（覆盖全部 10 种类型）:
///    - text: 纯文本（可能含图片/表情元素）
///    - reply: 回复/引用消息（有 reply element → 格式化；无 → 降级提取）
///    - audio: 语音 → [语音]
///    - json: JSON/卡片/小程序 → [卡片消息] 或提取 description
///    - file: 文件 → [文件: filename]
///    - video: 视频 → [视频]
///    - type_10: 红包/转账 → [红包/转账]
///    - forward: 合并转发 → [转发消息]
///    - type_19: 通话记录 → [通话记录]
///    - 未知 type → 跳过（skipped_unknown）
/// 5. 角色映射: 发送者==导出者→user，否则→assistant+[名称]前缀
fn parse_json_message(
    raw_msg: &serde_json::Value,
    self_uid: &str,
    self_name: &str,
    report: &mut ImportReport,
) -> Option<ParsedMessage> {
    // ── 提取常规字段 ──
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
    let is_system = raw_msg
        .get("system")
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
    let sender_uin = sender
        .and_then(|s| s.get("uin"))
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty());
    let sender_name = sender
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");

    // ── 规则1：撤回消息直接跳过 ──
    if recalled {
        report.skipped_recalled += 1;
        tracing::debug!(time = %time_str, "跳过撤回消息");
        return None;
    }

    // ── 规则2：系统消息直接跳过 ──
    // 仅依赖 system==true 判断，不可依赖 sender 字段
    // （system 消息的 sender 为 {"uid": "未知", "name": "系统消息"}）
    if is_system {
        report.skipped_system += 1;
        tracing::debug!(time = %time_str, "跳过系统消息");
        return None;
    }

    // ── 规则3：content.text 空消息防御性处理 ──
    // qce v6.x 中 type_10 和 type_19 的 content.text 已为非空值，
    // 此分支仅在导出工具异常或未来格式变动时生效
    if raw_text.is_empty() {
        if !elements.is_empty() {
            report.skipped_empty += 1;
            tracing::debug!(time = %time_str, msg_type = %msg_type,
                "text 为空且 elements 无法提取文本，跳过");
            return None;
        }
        // elements 也为空：type_19 通话记录特殊处理为降级
        if msg_type == TYPE_CALL {
            report.degraded_qce_unsupported += 1;
            tracing::debug!(time = %time_str, "通话记录(text为空)→[通话记录]");
            let (role, content_final) = make_role_content(
                sender_uid,
                sender_name,
                self_uid,
                self_name,
                msg_type,
                &elements,
                report,
                "[通话记录]",
            );
            let fingerprint = make_fingerprint(timestamp, &role, &content_final);
            return Some(ParsedMessage {
                role,
                content: content_final,
                created_at: timestamp,
                fingerprint,
                sender_uid: sender_uid.to_string(),
                sender_uin: sender_uin.map(|s| s.to_string()),
                sender_name: sender_name.to_string(),
            });
        }
        report.skipped_empty += 1;
        tracing::debug!(time = %time_str, msg_type = %msg_type, "跳过空消息");
        return None;
    }

    // ── 规则4：根据 type 分流处理 ──
    let final_text = match msg_type {
        // "text": 普通文本消息（可能含图片或表情元素）
        TYPE_TEXT => {
            if has_image_element(&elements) {
                // 含图片元素：清理图片占位符，统一为 [图片]
                let cleaned = clean_image_placeholders(&raw_text);
                let result = if cleaned.is_empty() {
                    "[图片]".to_string()
                } else {
                    cleaned
                };
                report.success_image += 1;
                result
            } else {
                // 纯文本（或含表情，表情已在 content.text 中以 /表情名 表示）
                report.success_text += 1;
                raw_text.clone()
            }
        }

        // "reply": 回复/引用消息
        TYPE_REPLY => {
            if let Some(reply_elem) = reply_element(&elements) {
                // 有 reply 元素：格式化「回复 sender: content」引用头部
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

                // 截断过长的引用内容（超过 30 字符加 "…"）
                let quoted_display = if quoted_content.chars().count() > 30 {
                    format!("{}…", quoted_content.chars().take(30).collect::<String>())
                } else {
                    quoted_content.to_string()
                };

                report.success_reply += 1;
                format!("「回复 {quoted_sender}: {quoted_display}」{reply_body}")
            } else {
                // 无 reply 元素：降级提取正文
                report.degraded_reply_fallback += 1;
                tracing::debug!(time = %time_str, "回复消息无reply元素，降级提取正文");
                extract_reply_body(&raw_text)
            }
        }

        // "audio": 语音消息 → 降级为文本占位符
        TYPE_AUDIO => {
            report.degraded_audio += 1;
            tracing::debug!(time = %time_str, "语音消息→[语音]");
            "[语音]".to_string()
        }

        // "json": JSON/卡片/小程序/位置分享 → 降级为文本占位符
        // 优先提取 data.description 或 data.title 以保留语义信息
        TYPE_JSON => {
            report.degraded_card += 1;
            if let Some(desc) = json_element_description(&elements) {
                // 截断过长的描述（保留前 40 字符）
                let truncated = if desc.chars().count() > 40 {
                    format!("{}…", desc.chars().take(40).collect::<String>())
                } else {
                    desc
                };
                tracing::debug!(time = %time_str, description = %truncated,
                    "JSON卡片→提取描述");
                format!("[卡片: {truncated}]")
            } else {
                tracing::debug!(time = %time_str, "JSON卡片→[卡片消息]");
                "[卡片消息]".to_string()
            }
        }

        // "file": 文件消息 → 降级为 [文件: filename]
        TYPE_FILE => {
            report.degraded_file += 1;
            tracing::debug!(time = %time_str, "文件消息→[文件: ...]");
            raw_text.clone()
        }

        // "video": 视频消息 → 降级为文本占位符
        TYPE_VIDEO => {
            report.degraded_video += 1;
            tracing::debug!(time = %time_str, "视频消息→[视频]");
            "[视频]".to_string()
        }

        // "type_10": 红包/转账消息 → 降级为 [红包/转账]
        TYPE_RED_ENVELOPE => {
            report.degraded_red_envelope += 1;
            tracing::debug!(time = %time_str, "红包/转账消息→[红包/转账]");
            "[红包/转账]".to_string()
        }

        // "forward": 合并转发消息 → 降级为文本占位符
        TYPE_FORWARD => {
            report.degraded_forward += 1;
            tracing::debug!(time = %time_str, "合并转发→[转发消息]");
            "[转发消息]".to_string()
        }

        // "type_19": 通话记录
        // qce v6.x 中 content.text 为 "通话 - 已在其他设备处理"（非空），
        // 上方空文本防御分支仅在异常情况下触发
        TYPE_CALL => {
            report.degraded_qce_unsupported += 1;
            tracing::debug!(time = %time_str, "通话记录→[通话记录]");
            "[通话记录]".to_string()
        }

        // 未知 type：跳过并记录
        other => {
            report.skipped_unknown += 1;
            if !report.unknown_types.contains(&other.to_string()) {
                report.unknown_types.push(other.to_string());
            }
            tracing::warn!(msg_type = %other, time = %time_str, "未知消息类型，跳过");
            return None;
        }
    };

    // ── 规则5：角色映射 ──
    let (role, content_final) = make_role_content(
        sender_uid,
        sender_name,
        self_uid,
        self_name,
        msg_type,
        &elements,
        report,
        &final_text,
    );

    let fingerprint = make_fingerprint(timestamp, &role, &content_final);

    Some(ParsedMessage {
        role,
        content: content_final,
        created_at: timestamp,
        fingerprint,
        sender_uid: sender_uid.to_string(),
        sender_uin: sender_uin.map(|s| s.to_string()),
        sender_name: sender_name.to_string(),
    })
}

/// 根据发送者信息计算角色映射和最终内容。
///
/// 双前缀模式规则:
/// - `sender_uid == self_uid` → role="user"，加 `[{self_name}]` 前缀
/// - `sender_uid != self_uid` → role="assistant"，加 `[{sender_name}]` 前缀
/// - 对方纯文本/回复消息额外计入 `success_other_sender`
///
/// 设计动机:
/// - 双方均按姓名显示，准确反映两个独立人格之间的对话。
#[allow(clippy::too_many_arguments)]
fn make_role_content(
    sender_uid: &str,
    sender_name: &str,
    self_uid: &str,
    self_name: &str,
    msg_type: &str,
    elements: &[serde_json::Value],
    report: &mut ImportReport,
    final_text: &str,
) -> (String, String) {
    if sender_uid == self_uid {
        // 自己的消息：加 [{self_name}] 前缀
        let prefix = if self_name.is_empty() {
            "[我] ".to_string()
        } else {
            format!("[{self_name}] ")
        };
        ("user".to_string(), format!("{prefix}{final_text}"))
    } else {
        // 对方消息：加 [{sender_name}] 前缀
        let prefix = if sender_name.is_empty() {
            "[对方] ".to_string()
        } else {
            format!("[{sender_name}] ")
        };
        // 统计对方发言（仅 text 纯文本和 reply 成功回复）
        if matches!(msg_type, TYPE_TEXT | TYPE_REPLY)
            && (msg_type != TYPE_REPLY || reply_element(elements).is_some())
        {
            report.success_other_sender += 1;
        }
        ("assistant".to_string(), format!("{prefix}{final_text}"))
    }
}

// =========================================================
// Session 切割
// =========================================================

/// 按时间间隔将消息列表切割为若干 session。
///
/// 算法: 单次遍历 O(n)，严守时间阈值语义。
///
/// 关键性质:
/// - **单调性**：输入已排序，输出 session 时间不重叠。
/// - **无回溯**：不跨 session 合并。
/// - **空安全**：输入为空返回空 Vec，不 panic。
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
            // 时间间隔超出阈值 → 切断为新 session
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

    // flush 最后一个 session
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

/// 解析 qq-chat-exporter v6.x JSON 聊天记录文件。
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
/// - 文件不存在 → `file_not_found`
/// - JSON 解析失败 → `json_parse_error`
/// - 格式不匹配 → `format_mismatch`
pub fn parse_qq_export(
    file_path: &Path,
    gap_minutes: u32,
) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
    if !file_path.exists() {
        return Err(error::file_not_found(&file_path.display().to_string()));
    }

    let gap_ms = (gap_minutes as i64) * 60 * 1000;

    // 读取并解码文件（多编码自动检测）
    let bytes =
        fs::read(file_path).map_err(|e| error::read_error(&file_path.display().to_string(), e))?;
    let json_str = decode_bytes(&bytes)?;
    let raw_data: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| error::json_parse_error(&file_path.display().to_string(), &e.to_string()))?;

    // ── 校验 JSON 顶层结构 ──
    let chat_info = match raw_data.get("chatInfo") {
        Some(ci) => ci,
        None => {
            return Err(error::format_mismatch(
                "QQ Chat Exporter JSON（含 chatInfo 字段）",
                "请确认文件是由 shuakami/qq-chat-exporter v6.x 导出的 JSON 格式。",
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

    // ── 提取 meta 信息 ──
    let self_uid = chat_info
        .get("selfUid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let self_name = chat_info
        .get("selfName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let self_uin = chat_info
        .get("selfUin")
        .and_then(|v| v.as_str())
        .filter(|u| !u.is_empty());
    let chat_name = chat_info.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let chat_type = chat_info
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // v6.x: 直接使用 chatInfo.peerUid/peerUin 作为对方标识，无需扫描消息列表
    let peer_uid = chat_info
        .get("peerUid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let peer_uin = chat_info
        .get("peerUin")
        .and_then(|v| v.as_str())
        .filter(|u| !u.is_empty());

    let mut report = ImportReport {
        file_path: file_path.display().to_string(),
        self_id: self_uid.to_string(),
        self_name: self_name.to_string(),
        self_uin: self_uin.map(|s| s.to_string()),
        chat_name: chat_name.to_string(),
        chat_type: chat_type.to_string(),
        // 对方标识直接从 chatInfo 提取
        other_uid: peer_uid.to_string(),
        other_uin: peer_uin.map(|s| s.to_string()),
        other_name: chat_name.to_string(),
        total_raw: raw_messages.len(),
        gap_minutes: (gap_ms / 60_000) as u32,
        ..Default::default()
    };

    tracing::info!(
        file = %report.file_path,
        self_name = %report.self_name,
        chat_name = %report.chat_name,
        chat_type = %report.chat_type,
        peer_uid = %report.other_uid,
        total_raw = report.total_raw,
        "开始解析 QQ JSON 聊天记录 (v6.x)"
    );

    // ── Layer 1 去重：文件内 (id, timestamp) 联合键 ──
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

    // ── 按时间戳升序排列 ──
    deduped_msgs.sort_by_key(|m| m.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0));

    // ── 逐条解析 ──
    let mut parsed_messages: Vec<ParsedMessage> = Vec::new();
    for raw_msg in &deduped_msgs {
        if let Some(parsed) = parse_json_message(raw_msg, self_uid, self_name, &mut report) {
            parsed_messages.push(parsed);
        }
    }

    // ── Session 切割 ──
    let sessions = split_into_sessions(&parsed_messages, gap_ms);
    report.session_count = sessions.len();

    // ── 时间范围 ──
    if !parsed_messages.is_empty() {
        report.time_start = ts_ms_to_date(parsed_messages.first().unwrap().created_at);
        report.time_end = ts_ms_to_date(parsed_messages.last().unwrap().created_at);
    }

    tracing::info!(
        sessions = report.session_count,
        success = report.total_success(),
        degraded = report.total_degraded(),
        skipped = report.total_skipped(),
        skipped_system = report.skipped_system,
        dedup_removed = report.dedup_removed,
        "QQ JSON 解析完成"
    );

    if sessions.is_empty() {
        report
            .warnings
            .push("未解析出任何有效消息（全部被跳过或不支持）".to_string());
    }

    Ok((sessions, report))
}

/// 多编码尝试解码字节数组为字符串。
///
/// 五级降级链（按优先级）:
/// 1. UTF-8 ── 绝大多数 qce 导出文件的编码
/// 2. UTF-8 BOM ── 部分编辑器添加 BOM 头
/// 3. UTF-16 LE ── Windows 某些版本 QQ 的默认编码
/// 4. GBK ── 简体中文 Windows 的旧版默认编码
/// 5. Latin-1 兜底 ── 永不失败，单字节映射（可能乱码但不会 panic）
fn decode_bytes(bytes: &[u8]) -> RamariaResult<String> {
    // 1. UTF-8
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        return Ok(s);
    }

    // 2. UTF-8 BOM
    if bytes.len() >= 3
        && bytes[0] == 0xEF
        && bytes[1] == 0xBB
        && bytes[2] == 0xBF
        && let Ok(s) = String::from_utf8(bytes[3..].to_vec())
    {
        return Ok(s);
    }

    // 3. UTF-16 LE (BOM: 0xFF 0xFE)
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        if let Ok(s) = String::from_utf16(&utf16) {
            return Ok(s);
        }
    }

    // 4. GBK
    let (decoded, _, has_errors) = encoding_rs::GBK.decode(bytes);
    if !has_errors {
        return Ok(decoded.into_owned());
    }

    // 5. Latin-1 兜底（永不失败）
    let latin1: String = bytes.iter().map(|&b| b as char).collect();
    Ok(latin1)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

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

    /// extract_reply_body 各输入参数化验证。
    #[test]
    fn extract_reply_body_cases() {
        let cases = [
            ("回复的头部信息\n这是真正的回复正文", "这是真正的回复正文"),
            ("[回复某人] 这是正文", "这是正文"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(extract_reply_body(input), expected, "input={input:?}");
        }
    }

    // -- 指纹计算 --

    /// make_fingerprint 确定性与区分度验证。
    #[test]
    fn fingerprint_properties() {
        // 同输入 → 同指纹，长度 16
        let fp1 = make_fingerprint(1700000000000, "user", "你好");
        let fp2 = make_fingerprint(1700000000000, "user", "你好");
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
        // content 不同 → 指纹不同
        let fp3 = make_fingerprint(1700000000000, "user", "再见");
        assert_ne!(fp1, fp3);
        // role 不同 → 指纹不同
        let fp4 = make_fingerprint(1700000000000, "assistant", "你好");
        assert_ne!(fp1, fp4);
    }

    // -- Session 切割 --

    /// split_into_sessions 各消息序列参数化验证。
    #[test]
    fn split_sessions_cases() {
        // 单会话（间隔 < 60s）
        let msgs = vec![
            make_test_msg("user", "消息1", 1000),
            make_test_msg("assistant", "消息2", 2000),
        ];
        let sessions = split_into_sessions(&msgs, 60000);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].messages.len(), 2);
        // 多会话（间隔 > 60s → 拆为 2 组）
        let msgs = vec![
            make_test_msg("user", "消息1", 1000),
            make_test_msg("assistant", "消息2", 2000),
            make_test_msg("user", "消息3", 602000),
            make_test_msg("assistant", "消息4", 603000),
        ];
        let sessions = split_into_sessions(&msgs, 60000);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].messages.len(), 2);
        assert_eq!(sessions[1].messages.len(), 2);
        // 空输入
        assert!(split_into_sessions(&[], 60000).is_empty());
    }

    // -- 日期转换 --

    /// ts_ms_to_date 各时间戳参数化验证。
    #[test]
    fn ts_ms_to_date_cases() {
        let cases = [(1704067200000i64, "2024-01-01"), (0i64, "1970-01-01")];
        for (ts, expected) in cases {
            assert_eq!(ts_ms_to_date(ts), expected, "ts={ts}");
        }
    }

    // -- JSON 元素描述提取 --

    /// json_element_description 各元素参数化验证（description > title > None）。
    #[test]
    fn json_element_description_cases() {
        let cases = [
            (
                serde_json::json!({"type": "json", "data": {"title": "[QQ小程序]牛脑发力！", "description": "牛脑发力！动画区玩谁是卧底..."}}),
                Some("牛脑发力！动画区玩谁是卧底..."),
            ),
            (
                serde_json::json!({"type": "json", "data": {"title": "[QQ小程序]标题文本"}}),
                Some("[QQ小程序]标题文本"),
            ),
            (
                serde_json::json!({"type": "text", "data": {"text": "你好"}}),
                None,
            ),
        ];
        for (element, expected) in cases {
            let desc = json_element_description(&[element]);
            assert_eq!(desc.as_deref(), expected);
        }
    }

    // -- 辅助函数 --

    fn make_test_msg(role: &str, content: &str, created_at: i64) -> ParsedMessage {
        ParsedMessage {
            role: role.to_string(),
            content: content.to_string(),
            created_at,
            fingerprint: make_fingerprint(created_at, role, content),
            sender_uid: String::new(),
            sender_uin: None,
            sender_name: String::new(),
        }
    }
}
