//! crates/ramaria-core/src/types/attachments.rs - Ramaria 消息附件数据类型模块
//!
//! 设计特点:
//! - 消息附件行与处理状态的规范模型：附件存取与状态机的类型边界
//! - 图片占位符渲染纯函数：由附件描述合成读取文本（单点口径，供各读取口复用）
//! - 零 I/O 纯类型与纯函数，时间统一使用 Unix 毫秒

use std::collections::HashMap;

use uuid::Uuid;

use super::InboundAttachmentKind;

// =========================================================
// 附件类型与状态
// =========================================================

/// 消息附件行。
///
/// 职责:
/// - 记录一条消息所携带附件的引用、指纹、尺寸与理解结果；
/// - 供导入采集落库、图片理解回填与读取渲染消费。
///
/// 字段约定:
/// - `id`: 数据库自增主键；插入前填 0，查询返回真实值。
/// - `message_id`: 归属消息 ID。
/// - `kind`: 附件类型（image / audio / video / file）。
/// - `source_ref`: 导出根相对路径或 URL 引用。
/// - `md5`: 图片内容 md5（小写 hex，全 32 位）；平台未提供为 None。
/// - `size` / `width` / `height`: 可选的体积与尺寸信息。
/// - `sub_type`: 平台细分类型（如 photo / sticker）。
/// - `status`: 处理状态（pending / done / failed / skipped）。
/// - `description`: 理解结果文本；仅 status 为 Done 时有值。
/// - `description_model`: 产生描述的模型标识。
/// - `created_at` / `updated_at`: 写入与状态更新时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct MessageAttachment {
    pub id: i64,
    pub message_id: Uuid,
    pub kind: InboundAttachmentKind,
    pub source_ref: String,
    pub md5: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sub_type: Option<String>,
    pub status: AttachmentStatus,
    pub description: Option<String>,
    pub description_model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 附件处理状态。
///
/// 职责:
/// - 表示附件从采集到理解的流转状态（pending → done / failed / skipped）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStatus {
    Pending,
    Done,
    Failed,
    Skipped,
}

impl AttachmentStatus {
    /// 返回存储口径的小写字符串（pending / done / failed / skipped）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// 解析存储口径字符串；非法值返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

// =========================================================
// 本地相对引用判定
// =========================================================

/// 判定 source_ref 是否为可安全用于本地路径拼接的相对引用。
///
/// 口径:
/// - 非空、不以 '/' 开头、不以 http:// 或 https:// 开头；
/// - 不含反斜杠；不含 ".." 路径组件（防路径穿越）。
pub fn is_local_relative_ref(source_ref: &str) -> bool {
    !source_ref.is_empty()
        && !source_ref.starts_with('/')
        && !source_ref.starts_with("http://")
        && !source_ref.starts_with("https://")
        && !source_ref.contains('\\')
        && !source_ref.split('/').any(|segment| segment == "..")
}

// =========================================================
// 图片占位符渲染
// =========================================================

/// 由 md5（hex）生成占位符 hash：取前 8 位并转小写。
///
/// 参数:
/// - `md5_hex`: 图片 md5 十六进制字符串（通常为全 32 位）。
///
/// 返回:
/// - 前 8 位的小写形式；输入不足 8 位时返回其可用的前导部分（同样小写）。
pub fn image_placeholder_hash(md5_hex: &str) -> String {
    md5_hex.chars().take(8).collect::<String>().to_lowercase()
}

/// 从附件列表构建渲染映射：key 为占位符 hash，value 为描述文本。
///
/// 口径:
/// - 仅收录 status 为 Done 且 description 与 md5 均非空的附件；
/// - 同一 key 重复出现时后者覆盖前者。
///
/// 参数:
/// - `attachments`: 附件行列表（通常为同一批消息的附件）。
///
/// 返回:
/// - `image_placeholder_hash(md5)` → 描述的映射。
pub fn build_render_map(attachments: &[MessageAttachment]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for attachment in attachments {
        if attachment.status != AttachmentStatus::Done {
            continue;
        }
        let Some(md5) = attachment.md5.as_deref().filter(|md5| !md5.is_empty()) else {
            continue;
        };
        let Some(description) = attachment
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
        else {
            continue;
        };
        map.insert(image_placeholder_hash(md5), description.to_string());
    }
    map
}

/// 将文本中的 `[图片#{hash}]` 占位符替换为 `[图片: {描述}]`。
///
/// 扫描规则:
/// - 遇到 `[图片#` 后读取连续的 8 个 ASCII 字符再遇 `]` 视为占位符（严格 8 位）；
/// - 命中 `render_map`（key 为 8 位小写 hex）→ 替换为 `[图片: {描述}]`；
/// - 未命中 / 格式不完整 → 原文保留（逐字符复制，不做任何其他改动）。
///
/// 参数:
/// - `text`: 待渲染文本（库内消息正文保留占位符形态）。
/// - `render_map`: [`build_render_map`] 产出的 hash → 描述映射。
///
/// 返回:
/// - 替换后的文本；空文本或空映射时返回原文。
pub fn replace_image_placeholders(text: &str, render_map: &HashMap<String, String>) -> String {
    const PREFIX: &str = "[图片#";

    if text.is_empty() || render_map.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(PREFIX) {
        let after_prefix = &rest[pos + PREFIX.len()..];
        let hash = after_prefix.get(..8).filter(|hash| hash.is_ascii());
        let closer = after_prefix.get(8..9);
        let hit = match (hash, closer) {
            (Some(hash), Some("]")) => render_map.get(hash),
            _ => None,
        };
        match hit {
            Some(description) => {
                out.push_str(&rest[..pos]);
                out.push_str("[图片: ");
                out.push_str(description);
                out.push(']');
                rest = &after_prefix[9..];
            }
            None => {
                out.push_str(&rest[..pos + PREFIX.len()]);
                rest = after_prefix;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造附件行（描述与模型默认 None，测试按需覆盖）。
    fn attachment(
        kind: InboundAttachmentKind,
        md5: Option<&str>,
        status: AttachmentStatus,
    ) -> MessageAttachment {
        MessageAttachment {
            id: 0,
            message_id: Uuid::new_v4(),
            kind,
            source_ref: "images/a.png".to_string(),
            md5: md5.map(|s| s.to_string()),
            size: Some(1024),
            width: Some(640),
            height: Some(480),
            sub_type: Some("photo".to_string()),
            status,
            description: None,
            description_model: None,
            created_at: 100,
            updated_at: 100,
        }
    }

    /// 全 32 位 md5 取前 8 位小写；大小写输入输出一致为小写。
    #[test]
    fn placeholder_hash_takes_first_8_lowercase() {
        let hash = image_placeholder_hash("d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hash, "d41d8cd9");
        assert_eq!(image_placeholder_hash("D41D8CD98F00B204"), "d41d8cd9");
    }

    /// 不足 8 位的输入返回可用前导部分（同样小写）。
    #[test]
    fn placeholder_hash_defends_short_input() {
        assert_eq!(image_placeholder_hash("AbC"), "abc");
        assert_eq!(image_placeholder_hash(""), "");
    }

    /// done + 描述 + md5 三条件齐备才收录；同 key 后者覆盖前者。
    #[test]
    fn render_map_collects_only_renderable_rows() {
        let mut done = attachment(
            InboundAttachmentKind::Image,
            Some("aabbccddeeff00112233445566778899"),
            AttachmentStatus::Done,
        );
        done.description = Some("一只橘猫".to_string());
        done.description_model = Some("mock-vision".to_string());

        // pending 行有描述也不收录
        let mut pending = attachment(
            InboundAttachmentKind::Image,
            Some("bbccddeeff00112233445566778899aa"),
            AttachmentStatus::Pending,
        );
        pending.description = Some("未完成".to_string());
        // 空描述不收录
        let mut empty_desc = attachment(
            InboundAttachmentKind::Image,
            Some("ccddeeff00112233445566778899aabb"),
            AttachmentStatus::Done,
        );
        empty_desc.description = Some(String::new());
        // md5 缺失不收录
        let mut no_md5 = attachment(InboundAttachmentKind::Image, None, AttachmentStatus::Done);
        no_md5.description = Some("无指纹".to_string());

        let map = build_render_map(&[done, pending, empty_desc, no_md5]);
        assert_eq!(map.len(), 1, "仅 done + 描述 + md5 齐备的行应收录");
        assert_eq!(map.get("aabbccdd").map(String::as_str), Some("一只橘猫"));
    }

    /// 同 key 重复出现：后者覆盖前者。
    #[test]
    fn render_map_later_row_overrides_same_key() {
        let mut first = attachment(
            InboundAttachmentKind::Image,
            Some("aabbccddeeff00112233445566778899"),
            AttachmentStatus::Done,
        );
        first.description = Some("旧描述".to_string());
        let mut second = attachment(
            InboundAttachmentKind::Image,
            Some("aabbccddFFFFFFFFFFFFFFFFFFFFFFFF"),
            AttachmentStatus::Done,
        );
        second.description = Some("新描述".to_string());

        let map = build_render_map(&[first, second]);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("aabbccdd").map(String::as_str), Some("新描述"));
    }

    /// 空 md5（空串）同样不收录。
    #[test]
    fn render_map_skips_empty_md5() {
        let mut row = attachment(
            InboundAttachmentKind::Image,
            Some(""),
            AttachmentStatus::Done,
        );
        row.description = Some("空指纹".to_string());
        assert!(build_render_map(&[row]).is_empty());
    }

    /// 单个命中替换；多个命中依次替换；未命中保留原文。
    #[test]
    fn replace_hits_multiple_and_keeps_misses() {
        let mut map = HashMap::new();
        map.insert("aabbccdd".to_string(), "一只橘猫".to_string());
        map.insert("11223344".to_string(), "窗外的雪".to_string());

        let text = "看这个 [图片#aabbccdd] 还有 [图片#11223344]，[图片#ffffffff] 没命中";
        let rendered = replace_image_placeholders(text, &map);
        assert_eq!(
            rendered,
            "看这个 [图片: 一只橘猫] 还有 [图片: 窗外的雪]，[图片#ffffffff] 没命中"
        );
    }

    /// 非严格 8 位（过长 / 过短 / 含非 ASCII）不替换。
    #[test]
    fn replace_requires_exactly_8_ascii_chars() {
        let mut map = HashMap::new();
        map.insert("aabbccdd".to_string(), "描述".to_string());
        map.insert("aabbccd".to_string(), "短".to_string());

        // 9 位不收窄成 8 位命中
        assert_eq!(
            replace_image_placeholders("[图片#aabbccdd1]", &map),
            "[图片#aabbccdd1]"
        );
        // 7 位不命中
        assert_eq!(
            replace_image_placeholders("[图片#aabbccd]", &map),
            "[图片#aabbccd]"
        );
        // 8 位但含非 ASCII 字符
        assert_eq!(
            replace_image_placeholders("[图片#aabbcc中]", &map),
            "[图片#aabbcc中]"
        );
        // 缺闭合括号
        assert_eq!(
            replace_image_placeholders("[图片#aabbccdd", &map),
            "[图片#aabbccdd"
        );
        // 大小写与 map key 不同 → 未命中保留
        assert_eq!(
            replace_image_placeholders("[图片#AABBCCDD]", &map),
            "[图片#AABBCCDD]"
        );
    }

    /// 混排文本（中文、换行、其他方括号）不破坏；空 map 零改动。
    #[test]
    fn replace_preserves_mixed_text_and_empty_map() {
        let mut map = HashMap::new();
        map.insert("aabbccdd".to_string(), "图一".to_string());
        map.insert("99887766".to_string(), "图二\n第二行".to_string());

        let text = "【提醒】\n[图片#aabbccdd] 之后\n[引用] 普通方括号 [图片#99887766] 结束";
        let rendered = replace_image_placeholders(text, &map);
        assert_eq!(
            rendered,
            "【提醒】\n[图片: 图一] 之后\n[引用] 普通方括号 [图片: 图二\n第二行] 结束"
        );

        let empty = HashMap::new();
        assert_eq!(replace_image_placeholders(text, &empty), text);
        assert_eq!(replace_image_placeholders("", &map), "");
    }

    /// 描述文本本身含方括号形态时原样落入结果，不递归替换。
    #[test]
    fn replace_pushes_description_verbatim() {
        let mut map = HashMap::new();
        map.insert(
            "aabbccdd".to_string(),
            "框架 [图片#deadbeef] 截图".to_string(),
        );
        let rendered = replace_image_placeholders("前 [图片#aabbccdd] 后", &map);
        assert_eq!(rendered, "前 [图片: 框架 [图片#deadbeef] 截图] 后");
    }

    /// 附件状态 as_str / parse 往返；非法值返回 None。
    #[test]
    fn attachment_status_roundtrips_and_rejects_invalid() {
        for status in [
            AttachmentStatus::Pending,
            AttachmentStatus::Done,
            AttachmentStatus::Failed,
            AttachmentStatus::Skipped,
        ] {
            assert_eq!(AttachmentStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(AttachmentStatus::parse("unknown"), None);
        assert_eq!(AttachmentStatus::parse(""), None);
    }

    /// 本地相对引用判定五分支：正常相对路径放行；`..` / 反斜杠 / http(s) / 绝对 / 空串拒绝。
    #[test]
    fn local_relative_ref_accepts_only_safe_relative_paths() {
        // 放行：常规相对路径（含多级目录）
        assert!(is_local_relative_ref("resources/images/a.jpg"));
        assert!(is_local_relative_ref("a.png"));
        // 拒绝：路径穿越组件
        assert!(!is_local_relative_ref("resources/../secret.jpg"));
        assert!(!is_local_relative_ref(".."));
        // 拒绝：反斜杠（Windows 分隔符不受信）
        assert!(!is_local_relative_ref("resources\\images\\a.jpg"));
        // 拒绝：http(s) 链接
        assert!(!is_local_relative_ref("http://example.com/a.jpg"));
        assert!(!is_local_relative_ref("https://example.com/a.jpg"));
        // 拒绝：绝对路径与空串
        assert!(!is_local_relative_ref("/download?fileid=EXAMPLE"));
        assert!(!is_local_relative_ref(""));
    }
}
