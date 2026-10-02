//! crates/ramaria-importer/src/qq/parser/message.rs - 单条 QQ 消息解析与角色映射
//!
//! 设计特点:
//! - 覆盖 qce v6.x 全部 10 种语义化消息类型的分流与降级
//! - 跳过规则: 撤回 / 系统 / 空文本（type_19 空文本防御性降级为 [通话记录]）
//! - 角色映射双前缀模式: 双方均加 `[名称]` 前缀，消除"用户 vs 助手"误导
//! - 引用与卡片描述截断统一走 `ramaria_core::text::truncate_chars`（字符边界安全）
//! - 未知类型计入报告 `unknown_types` 并跳过，不中断整体解析

use crate::traits::{ImportReport, ParsedMessage};

use super::elements::{
    clean_image_placeholders, extract_reply_body, has_image_element, json_element_description,
    make_fingerprint, reply_element,
};

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
pub(super) fn parse_json_message(
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

                // 截断过长的引用内容（统一字符边界工具，预算内含省略号）
                let quoted_display = ramaria_core::text::truncate_chars(quoted_content, 30);

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
                // 截断过长的描述（统一字符边界工具，预算内含省略号）
                let truncated = ramaria_core::text::truncate_chars(&desc, 40);
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
