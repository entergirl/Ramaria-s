//! crates/ramaria-importer/src/qq/parser/elements.rs - 消息元素助手与指纹计算
//!
//! 设计特点:
//! - 统一从 `content.elements` 提取图片/回复/JSON 卡片语义
//! - 图片占位符渲染为 `[图片#hash]`：hash 取图片 md5 前 8 位小写，
//!   保证跨批次指纹一致、去重更准确，并供读取侧按 hash 替换为图片描述
//! - 回复正文提取含三级降级（换行 → 去前缀 → 原文），最坏情况保留信息
//! - JSON 卡片描述优先 `description`，回退 `title`，保留语义信息
//! - 指纹取 SHA-256 前 8 字节（16 hex 字符），含 role 维度区分不同角色

use sha2::{Digest, Sha256};

use ramaria_core::types::image_placeholder_hash;

// =========================================================
// 元素类型常量
// =========================================================

/// 图片元素类型标识。
const ELEM_IMAGE: &str = "image";
/// 回复引用元素类型标识。
const ELEM_REPLY: &str = "reply";
/// JSON 卡片元素类型标识（用于提取 title/description 优化降级文本）。
const ELEM_JSON: &str = "json";

/// 图片占位符前缀（导出工具形态 `[图片: ...]`）。
const IMAGE_PLACEHOLDER_PREFIX: &str = "[图片:";

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

// =========================================================
// 图片元素提取与占位符渲染
// =========================================================

/// image 元素的提取信息（解析用中间结构）。
///
/// 字段约定:
/// - `md5`: 图片内容 md5（小写 hex）；缺失或为空为 None。
/// - `filename` / `url` / `local_path` / `sub_type`: 字段缺失或为空为 None。
/// - `size` / `width` / `height`: 导出提供时读取，缺失为 None。
pub(super) struct ImageElementInfo {
    /// 图片内容 md5（小写 hex）
    pub md5: Option<String>,
    /// 导出文件名（如 `HASH.jpg`，HASH 为 md5 大写）
    pub filename: Option<String>,
    /// 导出根相对路径（含 `resources/` 前缀；旧导出为服务器下载链接）
    pub url: Option<String>,
    /// resources 目录相对路径（不含 `resources/` 前缀）
    pub local_path: Option<String>,
    /// 文件字节数
    pub size: Option<u64>,
    /// 图片宽度（像素）
    pub width: Option<u32>,
    /// 图片高度（像素）
    pub height: Option<u32>,
    /// 平台细分类型（如 photo / sticker）
    pub sub_type: Option<String>,
}

/// 按序提取全部 image 元素信息（字段缺失一律 None，不报错）。
///
/// 参数:
/// - `elements`: 消息的 `content.elements` 数组。
///
/// 返回:
/// - 与 elements 中 image 元素同序的提取信息列表。
pub(super) fn image_element_infos(elements: &[serde_json::Value]) -> Vec<ImageElementInfo> {
    elements
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some(ELEM_IMAGE))
        .map(|e| {
            let data = e.get("data");
            let opt_str = |key: &str| {
                data.and_then(|d| d.get(key))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            };
            ImageElementInfo {
                md5: opt_str("md5").map(|md5| md5.to_lowercase()),
                filename: opt_str("filename"),
                url: opt_str("url"),
                local_path: opt_str("localPath"),
                size: data.and_then(|d| d.get("size")).and_then(|v| v.as_u64()),
                width: data
                    .and_then(|d| d.get("width"))
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u32::try_from(n).ok()),
                height: data
                    .and_then(|d| d.get("height"))
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u32::try_from(n).ok()),
                sub_type: opt_str("subType"),
            }
        })
        .collect()
}

/// 将文本中 `[图片: xxx]` 占位符渲染为 `[图片#{hash}]`。
///
/// 幂等: 已是 `[图片#hash]` 的片段不受影响（扫描只匹配带冒号的导出形态）。
///
/// 配对与回退（对每个占位符，按出现顺序）:
/// 1. 优先匹配 `filename == xxx` 的未使用元素 → 取该元素 md5 的 hash；
/// 2. 未命中 → 按序取下一个未使用元素（其 md5 缺失时落入回退 3）；
/// 3. 无元素可用或元素无 md5 → 从 xxx 提取恰好 32 位连续 hex → 前 8 位小写；
/// 4. 提取失败 → `sha256(xxx)` 前 8 位小写；
/// 5. 找不到 `]` 的畸形 `[图片:` 原样保留（不做任何替换）。
///
/// 参数:
/// - `text`: 待渲染文本。
/// - `infos`: 本条消息的图片元素信息（与占位符按序配对）。
///
/// 返回:
/// - 渲染后的文本（首尾空白同旧口径去除）。
pub(super) fn render_image_placeholders(text: &str, infos: &[ImageElementInfo]) -> String {
    let mut used = vec![false; infos.len()];
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let prefix: Vec<char> = IMAGE_PLACEHOLDER_PREFIX.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // 查找 '[' 后跟 "图片:"
        if i + prefix.len() <= chars.len() && chars[i..i + prefix.len()] == prefix[..] {
            // 找到匹配的 ']'；找不到（畸形占位符）时原样保留
            if let Some(end) = chars[i..].iter().position(|&c| c == ']') {
                let raw: String = chars[i + prefix.len()..i + end].iter().collect();
                result.push_str(&render_one_placeholder(raw.trim(), infos, &mut used));
                i += end + 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result.trim().to_string()
}

/// 单个占位符的渲染替换文本（消费元素并执行回退链）。
fn render_one_placeholder(key: &str, infos: &[ImageElementInfo], used: &mut [bool]) -> String {
    // 1. filename 匹配的未使用元素
    let matched = infos
        .iter()
        .enumerate()
        .find(|(idx, info)| !used[*idx] && info.filename.as_deref() == Some(key))
        .map(|(idx, _)| idx);
    // 2. 按序取下一个未使用元素
    let picked = matched.or_else(|| used.iter().position(|used| !used));
    let hash = picked.and_then(|idx| {
        used[idx] = true;
        infos[idx].md5.as_deref().map(image_placeholder_hash)
    });
    if let Some(hash) = hash {
        return format!("[图片#{hash}]");
    }
    // 3. 从占位符内容提取 32 位 hex；4. sha256 散列兜底
    match extract_md5_prefix(key) {
        Some(hash) => format!("[图片#{hash}]"),
        None => format!("[图片#{}]", sha256_prefix8(key)),
    }
}

/// 从字符串中提取恰好 32 位的连续 hex 段（大小写不敏感），返回其前 8 位小写。
///
/// 返回:
/// - `Some(hash)`: 命中 32 位连续 hex 段（如导出文件名形态 `HASH.jpg`）；
/// - `None`: 不存在长度恰好 32 位的连续 hex 段。
fn extract_md5_prefix(raw: &str) -> Option<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut start: Option<usize> = None;
    for (idx, ch) in chars.iter().enumerate() {
        if ch.is_ascii_hexdigit() {
            if start.is_none() {
                start = Some(idx);
            }
        } else if let Some(seg_start) = start.take() {
            if idx - seg_start == 32 {
                return Some(
                    chars[seg_start..seg_start + 8]
                        .iter()
                        .collect::<String>()
                        .to_lowercase(),
                );
            }
        }
    }
    if let Some(seg_start) = start {
        if chars.len() - seg_start == 32 {
            return Some(
                chars[seg_start..seg_start + 8]
                    .iter()
                    .collect::<String>()
                    .to_lowercase(),
            );
        }
    }
    None
}

/// `sha256(value)` 的前 8 位小写 hex。
fn sha256_prefix8(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let result = hasher.finalize();
    result[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// 无文本纯图场景的占位符。
///
/// 口径:
/// - 首个元素有 md5 → `[图片#{hash}]`；
/// - 否则（无元素或 md5 缺失）→ `[图片]`。
///
/// 参数:
/// - `infos`: 本条消息的图片元素信息。
pub(super) fn fallback_image_placeholder(infos: &[ImageElementInfo]) -> String {
    match infos.first().and_then(|info| info.md5.as_deref()) {
        Some(md5) => format!("[图片#{}]", image_placeholder_hash(md5)),
        None => "[图片]".to_string(),
    }
}

/// 规范化 source_ref 为导出根相对路径。
///
/// 规则:
/// - `url` 存在且非空 → 原样使用（相对路径直接可用；服务器链接 / 绝对路径
///   同样原样保留，由写入侧据此判不可定位）；
/// - `url` 缺失/为空 → `localPath` 补 `resources/` 前缀（已有前缀时原样）；
/// - 都没有 → 空串。
///
/// 参数:
/// - `info`: 单条图片元素信息。
pub(super) fn normalize_source_ref(info: &ImageElementInfo) -> String {
    if let Some(url) = info.url.as_deref() {
        return url.to_string();
    }
    match info.local_path.as_deref() {
        Some(local_path) if local_path.starts_with("resources/") => local_path.to_string(),
        Some(local_path) => format!("resources/{local_path}"),
        None => String::new(),
    }
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
/// - 图片占位符已在调用前渲染为 `[图片#hash]`（hash 由 md5 稳定生成），
///   确保跨批次一致
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
