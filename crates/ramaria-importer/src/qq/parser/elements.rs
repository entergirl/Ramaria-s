//! crates/ramaria-importer/src/qq/parser/elements.rs - 消息元素助手与指纹计算
//!
//! 设计特点:
//! - 统一从 `content.elements` 提取图片/回复/JSON 卡片语义
//! - 图片占位符统一为 `[图片]`，保证跨批次指纹一致、去重更准确
//! - 回复正文提取含三级降级（换行 → 去前缀 → 原文），最坏情况保留信息
//! - JSON 卡片描述优先 `description`，回退 `title`，保留语义信息
//! - 指纹取 SHA-256 前 8 字节（16 hex 字符），含 role 维度区分不同角色

use sha2::{Digest, Sha256};

// =========================================================
// 元素类型常量
// =========================================================

/// 图片元素类型标识。
const ELEM_IMAGE: &str = "image";
/// 回复引用元素类型标识。
const ELEM_REPLY: &str = "reply";
/// JSON 卡片元素类型标识（用于提取 title/description 优化降级文本）。
const ELEM_JSON: &str = "json";

// =========================================================
// 工具函数
// =========================================================

/// 判断 elements 列表中是否包含图片类型的元素。
pub(super) fn has_image_element(elements: &[serde_json::Value]) -> bool {
    elements
        .iter()
        .any(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_IMAGE))
}

/// 从 elements 列表中提取 type=reply 的元素数据。
pub(super) fn reply_element(elements: &[serde_json::Value]) -> Option<serde_json::Value> {
    elements
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_REPLY))
        .and_then(|e| e.get("data").cloned())
}

/// 从 elements 列表中提取 JSON 卡片元素的描述文本。
///
/// 优先级:
/// 1. `data.description` — 卡片的描述摘要（如"示例活动：动画区答题互动..."）
/// 2. `data.title` — 卡片标题（如"[QQ小程序]示例活动：动画答题..."）
///
/// 返回:
/// - `Some(description)` — 提取到的描述文本
/// - `None` — elements 中无 json 元素或 data 中无 description/title
pub(super) fn json_element_description(elements: &[serde_json::Value]) -> Option<String> {
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
pub(super) fn clean_image_placeholders(text: &str) -> String {
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
pub(super) fn extract_reply_body(content_text: &str) -> String {
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
pub(super) fn make_fingerprint(original_ts: i64, role: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{original_ts}|{role}|{content}").as_bytes());
    let result = hasher.finalize();
    result[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}
