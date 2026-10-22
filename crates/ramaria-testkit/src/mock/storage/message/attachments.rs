//! crates/ramaria-testkit/src/mock/storage/message/attachments.rs - MockStorage 的消息附件域实现
//!
//! 设计特点:
//! - 附件按状态扫描，按 id 升序并支持条数上限
//! - 按 md5 批量回填描述，只处理 pending 行
//! - 重扫重置把行退回 pending，并可覆盖来源引用

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{AttachmentStatus, MessageAttachment};
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn insert_message_attachments(
        &self,
        attachments: &[MessageAttachment],
    ) -> RamariaResult<()> {
        self.guard("insert_message_attachments")?;
        let mut stored = self.attachments.lock().expect("附件锁");
        for attachment in attachments {
            let id = self
                .attachment_seq
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut row = attachment.clone();
            row.id = id;
            stored.insert(id, row);
        }
        Ok(())
    }

    pub(crate) async fn list_pending_attachments_by_session(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_pending_attachments_by_session")?;
        let message_ids: Vec<Uuid> = self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .map(|list| list.iter().map(|m| m.id).collect())
            .unwrap_or_default();
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| row.status == AttachmentStatus::Pending)
            .filter(|row| message_ids.contains(&row.message_id))
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    pub(crate) async fn list_attachments_by_messages(
        &self,
        message_ids: &[Uuid],
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_attachments_by_messages")?;
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| message_ids.contains(&row.message_id))
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        Ok(rows)
    }

    pub(crate) async fn fill_attachment_done_by_md5(
        &self,
        md5: &str,
        description: &str,
        description_model: &str,
    ) -> RamariaResult<u64> {
        self.guard("fill_attachment_done_by_md5")?;
        let mut rows = self.attachments.lock().expect("附件锁");
        let mut count = 0;
        for row in rows.values_mut() {
            if row.md5.as_deref() == Some(md5) && row.status == AttachmentStatus::Pending {
                row.status = AttachmentStatus::Done;
                row.description = Some(description.to_string());
                row.description_model = Some(description_model.to_string());
                count += 1;
            }
        }
        Ok(count)
    }

    pub(crate) async fn mark_attachment_done(
        &self,
        id: i64,
        description: &str,
        description_model: &str,
    ) -> RamariaResult<()> {
        self.guard("mark_attachment_done")?;
        if let Some(row) = self.attachments.lock().expect("附件锁").get_mut(&id) {
            row.status = AttachmentStatus::Done;
            row.description = Some(description.to_string());
            row.description_model = Some(description_model.to_string());
        }
        Ok(())
    }

    pub(crate) async fn mark_attachment_status(
        &self,
        id: i64,
        status: AttachmentStatus,
    ) -> RamariaResult<()> {
        self.guard("mark_attachment_status")?;
        if let Some(row) = self.attachments.lock().expect("附件锁").get_mut(&id) {
            row.status = status;
        }
        Ok(())
    }

    pub(crate) async fn find_attachment_description_by_md5(
        &self,
        md5: &str,
    ) -> RamariaResult<Option<(String, String)>> {
        self.guard("find_attachment_description_by_md5")?;
        let rows = self.attachments.lock().expect("附件锁");
        let found = rows
            .values()
            .filter(|row| row.md5.as_deref() == Some(md5))
            .filter(|row| row.status == AttachmentStatus::Done)
            .max_by_key(|row| row.updated_at)
            .and_then(|row| {
                row.description.clone().map(|description| {
                    (
                        description,
                        row.description_model.clone().unwrap_or_default(),
                    )
                })
            });
        Ok(found)
    }

    pub(crate) async fn list_all_pending_attachments(
        &self,
        limit: u32,
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_all_pending_attachments")?;
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| row.status == AttachmentStatus::Pending)
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    pub(crate) async fn list_all_skipped_attachments(
        &self,
        limit: u32,
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_all_skipped_attachments")?;
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| row.status == AttachmentStatus::Skipped)
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    pub(crate) async fn reset_attachment_for_rescan(
        &self,
        id: i64,
        source_ref: Option<&str>,
    ) -> RamariaResult<()> {
        self.guard("reset_attachment_for_rescan")?;
        if let Some(row) = self.attachments.lock().expect("附件锁").get_mut(&id) {
            row.status = AttachmentStatus::Pending;
            if let Some(source_ref) = source_ref {
                row.source_ref = source_ref.to_string();
            }
        }
        Ok(())
    }
}
