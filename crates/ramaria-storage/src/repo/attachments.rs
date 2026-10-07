//! crates/ramaria-storage/src/repo/attachments.rs - 消息附件行存取模块
//!
//! 设计特点:
//! - 附件表读写单一落点：采集批量写入 / 按状态扫描 / 描述回填 / md5 去重查询
//! - 枚举列（kind / status）非法值读取时降级并记录 WARNING，不阻塞读取
//! - 描述回填按 md5 批量化：同一图片的多个附件共享描述，避免重复理解
//! - 按消息批量取附件时分片 IN 查询，单片不超过 500 个

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{AttachmentStatus, InboundAttachmentKind, MessageAttachment};
use sqlx::SqlitePool;
use uuid::Uuid;

// 使用共享宏生成枚举解析函数：非法值记录 WARNING 并回退默认变体
parse_enum_fallback!(
    parse_kind, InboundAttachmentKind, InboundAttachmentKind::File, "message_attachments", "kind",
    "image" => Image,
    "audio" => Audio,
    "video" => Video,
    "file"  => File,
);
parse_enum_fallback!(
    parse_status, AttachmentStatus, AttachmentStatus::Pending, "message_attachments", "status",
    "pending" => Pending,
    "done"    => Done,
    "failed"  => Failed,
    "skipped" => Skipped,
);

/// 单片 IN 查询的最大消息 id 数。
const MESSAGE_ID_CHUNK: usize = 500;

/// 查询 SELECT 列（与行结构字段一一对应）。
const COLUMNS: &str = "id, message_id, kind, source_ref, md5, size, width, height, sub_type, \
     status, description, description_model, created_at, updated_at";

/// JOIN 查询 SELECT 列（限定附件表别名，避免与 messages 同名列歧义）。
const COLUMNS_QUALIFIED: &str = "a.id, a.message_id, a.kind, a.source_ref, a.md5, a.size, \
     a.width, a.height, a.sub_type, a.status, a.description, a.description_model, a.created_at, \
     a.updated_at";

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: i64,
    message_id: String,
    kind: String,
    source_ref: String,
    md5: Option<String>,
    size: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    sub_type: Option<String>,
    status: String,
    description: Option<String>,
    description_model: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl AttachmentRow {
    fn into_attachment(self) -> RamariaResult<MessageAttachment> {
        Ok(MessageAttachment {
            id: self.id,
            message_id: parse_uuid_required(&self.message_id, "message_attachments", "message_id")?,
            kind: parse_kind(&self.kind),
            source_ref: self.source_ref,
            md5: self.md5,
            size: self.size.map(|v| v as u64),
            width: self.width.map(|v| v as u32),
            height: self.height.map(|v| v as u32),
            sub_type: self.sub_type,
            status: parse_status(&self.status),
            description: self.description,
            description_model: self.description_model,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// =========================================================
// 写入
// =========================================================

/// 批量写入消息附件行（单事务逐行 INSERT）。
///
/// 职责:
/// - 附件采集写入的单一入口：导入采集层把解析出的附件引用落库。
///
/// 口径:
/// - 空列表直接成功（不开启事务）；
/// - id 由数据库自增分配，结构内 id 不参与写入；
/// - `created_at` / `updated_at` 取结构内字段值（由调用方决定）。
///
/// 参数:
/// - `attachments`: 附件行列表。
///
/// 返回:
/// - `Ok(())`: 全部写入成功（事务已提交）。
/// - `Err(...)`: 任一条失败时整批回滚。
pub async fn insert_batch(
    pool: &SqlitePool,
    attachments: &[MessageAttachment],
) -> RamariaResult<()> {
    if attachments.is_empty() {
        return Ok(());
    }

    let mut txn = pool.begin().await.storage_err("开启消息附件事务失败")?;
    for attachment in attachments {
        sqlx::query(
            "INSERT INTO message_attachments \
                 (message_id, kind, source_ref, md5, size, width, height, sub_type, \
                  status, description, description_model, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(attachment.message_id.to_string())
        .bind(attachment.kind.as_str())
        .bind(&attachment.source_ref)
        .bind(&attachment.md5)
        .bind(attachment.size.map(|v| v as i64))
        .bind(attachment.width.map(|v| v as i64))
        .bind(attachment.height.map(|v| v as i64))
        .bind(&attachment.sub_type)
        .bind(attachment.status.as_str())
        .bind(&attachment.description)
        .bind(&attachment.description_model)
        .bind(attachment.created_at)
        .bind(attachment.updated_at)
        .execute(&mut *txn)
        .await
        .storage_err("写入消息附件失败")?;
    }
    txn.commit().await.storage_err("提交消息附件事务失败")?;
    Ok(())
}

/// 把某 md5 的全部 pending 行置 done（描述与模型一并写入）。
///
/// 职责:
/// - 图片理解完成后的批量回填：同一图片（同 md5）的多个附件共享一条描述，
///   含本次理解对应的当前行。
///
/// 口径:
/// - 仅命中 `status = 'pending'` 的行；已 done / failed / skipped 的行不动；
/// - `updated_at` 取当前时间。
///
/// 参数:
/// - `md5`: 图片内容 md5（小写 hex）。
/// - `description`: 理解结果文本。
/// - `description_model`: 产生描述的模型标识。
///
/// 返回:
/// - 受影响行数（无命中时为 0）。
pub async fn fill_done_by_md5(
    pool: &SqlitePool,
    md5: &str,
    description: &str,
    description_model: &str,
) -> RamariaResult<u64> {
    let result = sqlx::query(
        "UPDATE message_attachments \
         SET status = 'done', description = ?, description_model = ?, updated_at = ? \
         WHERE md5 = ? AND status = 'pending'",
    )
    .bind(description)
    .bind(description_model)
    .bind(ramaria_core::types::now_ms())
    .bind(md5)
    .execute(pool)
    .await
    .storage_err("按 md5 回填附件描述失败")?;
    Ok(result.rows_affected())
}

/// 单行置 done（md5 缺失的附件兜底路径）。
///
/// 口径:
/// - 无条件覆盖指定行的状态与描述（幂等）；`updated_at` 取当前时间。
///
/// 参数:
/// - `id`: 附件行 id。
/// - `description`: 理解结果文本。
/// - `description_model`: 产生描述的模型标识。
pub async fn mark_done(
    pool: &SqlitePool,
    id: i64,
    description: &str,
    description_model: &str,
) -> RamariaResult<()> {
    sqlx::query(
        "UPDATE message_attachments \
         SET status = 'done', description = ?, description_model = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(description)
    .bind(description_model)
    .bind(ramaria_core::types::now_ms())
    .bind(id)
    .execute(pool)
    .await
    .storage_err("标记附件描述完成失败")?;
    Ok(())
}

/// 单行置 failed / skipped（description 不动）。
///
/// 口径:
/// - 仅迁移状态列；既有描述文本保持原样；`updated_at` 取当前时间。
///
/// 参数:
/// - `id`: 附件行 id。
/// - `status`: 目标状态。
pub async fn mark_status(
    pool: &SqlitePool,
    id: i64,
    status: AttachmentStatus,
) -> RamariaResult<()> {
    sqlx::query("UPDATE message_attachments SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status.as_str())
        .bind(ramaria_core::types::now_ms())
        .bind(id)
        .execute(pool)
        .await
        .storage_err("标记附件状态失败")?;
    Ok(())
}

// =========================================================
// 查询
// =========================================================

/// 按会话扫描 pending 附件（JOIN messages 过滤会话）。
///
/// 口径:
/// - 仅返回 `status = 'pending'` 的附件，按附件 id 升序（采集先后）；
/// - `limit = 0` 表示不限；`limit > 0` 时取前 limit 条。
///
/// 参数:
/// - `session_id`: 目标会话。
/// - `limit`: 返回条数上限（0 = 不限）。
pub async fn list_pending_by_session(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: u32,
) -> RamariaResult<Vec<MessageAttachment>> {
    let base = format!(
        "SELECT {COLUMNS_QUALIFIED} FROM message_attachments a \
         JOIN messages m ON m.id = a.message_id \
         WHERE m.session_id = ? AND a.status = 'pending' \
         ORDER BY a.id ASC"
    );
    let rows = if limit == 0 {
        sqlx::query_as::<_, AttachmentRow>(&base)
            .bind(session_id.to_string())
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as::<_, AttachmentRow>(&format!("{base} LIMIT ?"))
            .bind(session_id.to_string())
            .bind(limit)
            .fetch_all(pool)
            .await
    }
    .storage_err("查询会话待处理附件失败")?;

    rows.into_iter().map(|row| row.into_attachment()).collect()
}

/// 按消息 id 批量查询附件（分片 IN，单片不超过 500 个）。
///
/// 口径:
/// - 空输入直接返回空列表；
/// - 按附件 id 升序（写入先后）返回命中行。
///
/// 参数:
/// - `message_ids`: 消息 id 列表（可含重复，重复不影响结果）。
pub async fn list_by_messages(
    pool: &SqlitePool,
    message_ids: &[Uuid],
) -> RamariaResult<Vec<MessageAttachment>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut attachments = Vec::new();
    for chunk in message_ids.chunks(MESSAGE_ID_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {COLUMNS} FROM message_attachments \
             WHERE message_id IN ({placeholders}) ORDER BY id ASC"
        );
        let mut query = sqlx::query_as::<_, AttachmentRow>(&sql);
        for message_id in chunk {
            query = query.bind(message_id.to_string());
        }
        let rows = query
            .fetch_all(pool)
            .await
            .storage_err("按消息批量查询附件失败")?;
        attachments.extend(
            rows.into_iter()
                .map(|row| row.into_attachment())
                .collect::<RamariaResult<Vec<_>>>()?,
        );
    }
    Ok(attachments)
}

/// 按 md5 查询最近一次已完成的描述。
///
/// 职责:
/// - 图片理解去重：同 md5 的附件已有描述时直接复用，不重复调用模型。
///
/// 口径:
/// - 仅取 `status = 'done'` 且描述非空的行，`updated_at` 降序取第一条；
/// - 描述模型缺失时以空串返回。
///
/// 参数:
/// - `md5`: 图片内容 md5（小写 hex）。
///
/// 返回:
/// - `Ok(Some((description, description_model)))`: 命中；
/// - `Ok(None)`: 无已完成描述。
pub async fn find_done_description_by_md5(
    pool: &SqlitePool,
    md5: &str,
) -> RamariaResult<Option<(String, String)>> {
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT description, description_model FROM message_attachments \
         WHERE md5 = ? AND status = 'done' AND description IS NOT NULL AND description <> '' \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(md5)
    .fetch_optional(pool)
    .await
    .storage_err("按 md5 查询附件描述失败")?;
    Ok(row.map(|(description, model)| (description, model.unwrap_or_default())))
}

#[cfg(test)]
mod tests;
