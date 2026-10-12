//! crates/ramaria-service/src/chat/render.rs - Ramaria 历史窗口附件渲染模块
//!
//! 设计特点:
//! - 附件行加载与渲染映射单点：供历史窗口的字符预算统计与 Prompt 注入共用
//! - 渲染口径：完成 → 描述（core 映射口径）；处理中（pending）→ 提示文本；
//!   其余状态不收录（未命中保留裸占位符）
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
/// - `done` 且有描述 → 复用 core 渲染映射的描述文本（done 优先）；
/// - `pending` 且 md5 非空 → 补缺为「处理中」（不覆盖 done）；
/// - `failed` / `skipped` → 不收录（未命中保留裸占位符）。
pub(super) fn build_history_render_map(
    attachments: &[MessageAttachment],
) -> HashMap<String, String> {
    let mut map = build_render_map(attachments);
    for attachment in attachments {
        if attachment.status != AttachmentStatus::Pending {
            continue;
        }
        let Some(md5) = attachment.md5.as_deref().filter(|md5| !md5.is_empty()) else {
            continue;
        };
        map.entry(image_placeholder_hash(md5))
            .or_insert_with(|| "处理中".to_string());
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

    /// 状态梯次：done → 描述；pending → 处理中；failed / skipped 不收录
    /// （未命中保留裸占位符）。
    #[test]
    fn history_render_map_covers_status_ladder() {
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

        let map = build_history_render_map(&rows);
        assert_eq!(map.get("aabbccdd").map(String::as_str), Some("一只橘猫"));
        assert_eq!(map.get("bbccddee").map(String::as_str), Some("处理中"));
        assert!(!map.contains_key("ccddeeff"), "failed 不收录");
        assert!(!map.contains_key("ddeeff00"), "skipped 不收录");
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

        let map = build_history_render_map(&rows);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("aabbccdd").map(String::as_str), Some("完成描述"));
        assert!(build_history_render_map(&[]).is_empty());
    }
}
