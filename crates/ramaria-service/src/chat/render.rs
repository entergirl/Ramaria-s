//! crates/ramaria-service/src/chat/render.rs - Ramaria 历史窗口附件渲染模块
//!
//! 设计特点:
//! - 附件行加载与渲染映射单点：供历史窗口的字符预算统计与 Prompt 注入共用
//! - 渲染口径：完成 → 描述；处理中（pending）→ 提示文本；其余状态不收录
//!   （未命中保留裸占位符）；社交通道会话转为「对方发来一张图片（…）」语义
//! - 查询失败降级为空列表（正文保留占位符原文），不阻塞生成

use std::collections::HashMap;

use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    AttachmentStatus, Message, MessageAttachment, build_render_map, image_placeholder_hash,
};
use uuid::Uuid;

// =========================================================
// 附件行加载与渲染映射
// =========================================================

/// 加载消息附件行（查询失败记 warn 并返回空列表：正文保留占位符原文）。
pub(super) async fn load_message_attachments(
    storage: &dyn StorageBackend,
    messages: &[Message],
) -> Vec<MessageAttachment> {
    let message_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();
    if message_ids.is_empty() {
        return Vec::new();
    }
    match storage.list_attachments_by_messages(&message_ids).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "历史窗口附件查询失败，正文保留占位符原文");
            Vec::new()
        }
    }
}

/// 构建历史窗口的图片渲染映射（LLM 注入路径专用）。
///
/// 口径:
/// - `done` 且有描述 → 描述文本（`social=true` 时转为「对方发来一张图片（{描述}）」）；
/// - `pending` 且 md5 非空 → 「处理中」（`social=true` 时为「对方发来一张图片（处理中）」；
///   不覆盖 done）；
/// - `failed` / `skipped` → 不收录（未命中保留裸占位符）。
///
/// 说明:
/// - 「对方发来一张图片」语义化仅作用于社交通道会话（`social=true`）；
///   本地 / MCP 等会话输出与 core 浏览口径逐字节一致（默认路径锁定）；
/// - 仅 LLM 注入路径携带该语义；浏览旁路（core `build_render_map`）与导出不注入。
pub(super) fn build_history_render_map(
    attachments: &[MessageAttachment],
    social: bool,
) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = build_render_map(attachments);
    if social {
        for description in map.values_mut() {
            let decorated = format!("对方发来一张图片（{description}）");
            *description = decorated;
        }
    }
    for attachment in attachments {
        if attachment.status != AttachmentStatus::Pending {
            continue;
        }
        let Some(md5) = attachment.md5.as_deref().filter(|md5| !md5.is_empty()) else {
            continue;
        };
        let pending_text = if social {
            "对方发来一张图片（处理中）"
        } else {
            "处理中"
        };
        map.entry(image_placeholder_hash(md5))
            .or_insert_with(|| pending_text.to_string());
    }
    map
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::InboundAttachmentKind;

    /// 构造附件行（描述与模型默认 None，测试按需覆盖）。
    fn attachment(
        md5: Option<&str>,
        status: AttachmentStatus,
        description: Option<&str>,
    ) -> MessageAttachment {
        MessageAttachment {
            id: 0,
            message_id: Uuid::new_v4(),
            kind: InboundAttachmentKind::Image,
            source_ref: "images/a.png".to_string(),
            md5: md5.map(str::to_string),
            size: None,
            width: None,
            height: None,
            sub_type: None,
            status,
            description: description.map(str::to_string),
            description_model: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// 状态梯次（社交语义化）：done → 图片语义 + 描述；pending → 图片语义 + 处理中；
    /// failed / skipped 不收录（未命中保留裸占位符）。
    #[test]
    fn history_render_map_covers_status_ladder_social() {
        let rows = vec![
            attachment(
                Some("aabbccddeeff00112233445566778899"),
                AttachmentStatus::Done,
                Some("一只橘猫"),
            ),
            attachment(
                Some("bbccddeeff00112233445566778899aa"),
                AttachmentStatus::Pending,
                None,
            ),
            attachment(
                Some("ccddeeff00112233445566778899aabb"),
                AttachmentStatus::Failed,
                None,
            ),
            attachment(
                Some("ddeeff00112233445566778899aabbcc"),
                AttachmentStatus::Skipped,
                Some("不应收录"),
            ),
        ];

        let map = build_history_render_map(&rows, true);
        assert_eq!(
            map.get("aabbccdd").map(String::as_str),
            Some("对方发来一张图片（一只橘猫）")
        );
        assert_eq!(
            map.get("bbccddee").map(String::as_str),
            Some("对方发来一张图片（处理中）")
        );
        assert!(!map.contains_key("ccddeeff"), "failed 不收录");
        assert!(!map.contains_key("ddeeff00"), "skipped 不收录");
    }

    /// 本地口径（social=false）：与 core 浏览口径逐字节一致（默认路径锁定）。
    #[test]
    fn history_render_map_local_matches_core_wording() {
        let rows = vec![
            attachment(
                Some("aabbccddeeff00112233445566778899"),
                AttachmentStatus::Done,
                Some("一只橘猫"),
            ),
            attachment(
                Some("bbccddeeff00112233445566778899aa"),
                AttachmentStatus::Pending,
                None,
            ),
        ];

        let map = build_history_render_map(&rows, false);
        assert_eq!(map.get("aabbccdd").map(String::as_str), Some("一只橘猫"));
        assert_eq!(map.get("bbccddee").map(String::as_str), Some("处理中"));
        // done 条目与 core 渲染映射同文（本地路径零变化）
        let core_map = build_render_map(&rows);
        assert_eq!(map.get("aabbccdd"), core_map.get("aabbccdd"));
    }

    /// done 优先：同占位符的 pending 行不覆盖已有描述；pending 缺 md5 不收录；
    /// 空列表返回空映射。
    #[test]
    fn history_render_map_done_wins_and_requires_md5() {
        let rows = vec![
            attachment(
                Some("aabbccddeeff00112233445566778899"),
                AttachmentStatus::Done,
                Some("完成描述"),
            ),
            attachment(
                Some("aabbccddFFFF00112233445566778899"),
                AttachmentStatus::Pending,
                None,
            ),
            attachment(None, AttachmentStatus::Pending, None),
        ];

        let map = build_history_render_map(&rows, true);
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("aabbccdd").map(String::as_str),
            Some("对方发来一张图片（完成描述）")
        );
        assert!(build_history_render_map(&[], true).is_empty());
    }
}
