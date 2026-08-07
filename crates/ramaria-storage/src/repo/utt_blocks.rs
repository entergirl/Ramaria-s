//! rust/crates/ramaria-storage/src/repo/utt_blocks.rs - utt 话语块存取模块
//!
//! 设计特点:
//! - 管理 `utt_blocks` 表（v1.4 新增），原文话语块的持久化读写
//! - insert 返回自增 id；embedding 以 f32 小端 BLOB 存储（None 表示未生成）
//! - 查询按 persona_uid 严格隔离（原文是最高敏感层，不跨 persona 暴露）
//! - get_latest_block_by_session 供跨会话桥接取"上一会话尾部原文"
//! - delete_by_session 供会话清理（与 sessions 级联删除配合使用）

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::UttBlock;
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct UttBlockRow {
    id: i64,
    persona_uid: String,
    session_id: String,
    start_msg_id: String,
    end_msg_id: String,
    block_text: String,
    msg_count: i64,
    time_span_ms: i64,
    embedding: Option<Vec<u8>>,
    created_at: i64,
}

impl UttBlockRow {
    fn into_block(self) -> RamariaResult<UttBlock> {
        Ok(UttBlock {
            id: self.id,
            persona_uid: self.persona_uid,
            session_id: parse_uuid_required(&self.session_id, "utt_blocks", "session_id")?,
            start_msg_id: parse_uuid_required(&self.start_msg_id, "utt_blocks", "start_msg_id")?,
            end_msg_id: parse_uuid_required(&self.end_msg_id, "utt_blocks", "end_msg_id")?,
            block_text: self.block_text,
            msg_count: self.msg_count as u32,
            time_span_ms: self.time_span_ms,
            embedding: self.embedding,
            created_at: self.created_at,
        })
    }
}

const BLOCK_COLUMNS: &str = "id, persona_uid, session_id, start_msg_id, end_msg_id, \
     block_text, msg_count, time_span_ms, embedding, created_at";

/// 插入一条 utt 话语块。
///
/// 参数:
/// - `block`: 待插入的话语块（id 会被忽略，由数据库回填）。
///
/// 返回:
/// - 新插入行的自增 id。
///
/// 说明:
/// - 幂等由调用方保证（构建层按 start_msg_id 去重），此处不做业务去重。
pub async fn insert(pool: &SqlitePool, block: &UttBlock) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO utt_blocks (persona_uid, session_id, start_msg_id, end_msg_id, \
         block_text, msg_count, time_span_ms, embedding, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&block.persona_uid)
    .bind(block.session_id.to_string())
    .bind(block.start_msg_id.to_string())
    .bind(block.end_msg_id.to_string())
    .bind(&block.block_text)
    .bind(block.msg_count as i64)
    .bind(block.time_span_ms)
    .bind(&block.embedding)
    .bind(block.created_at)
    .fetch_one(pool)
    .await
    .storage_err("保存 utt 话语块失败")
}

/// 按 persona 查询全部话语块（按创建时间升序）。
///
/// 安全约束:
/// - 仅返回指定 persona 的块，原文按 persona_uid 严格隔离。
pub async fn list_by_persona(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<UttBlock>> {
    let rows = sqlx::query_as::<_, UttBlockRow>(&format!(
        "SELECT {BLOCK_COLUMNS} FROM utt_blocks WHERE persona_uid = ? ORDER BY created_at ASC"
    ))
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询 utt 话语块列表失败")?;
    rows.into_iter()
        .map(|r| r.into_block())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 获取指定会话的最新一个话语块（按 created_at DESC 取第一条）。
///
/// 用途:
/// - 跨会话桥接：新会话创建时取最近一个已关闭会话的尾部原文。
/// - 会话内增量构建：跳过已切分消息时定位最后一块的边界。
///
/// 返回:
/// - `Ok(Some(block))`: 该会话至少有一个话语块。
/// - `Ok(None)`: 该会话尚无话语块。
pub async fn get_latest_block_by_session(
    pool: &SqlitePool,
    session_id: Uuid,
) -> RamariaResult<Option<UttBlock>> {
    let row = sqlx::query_as::<_, UttBlockRow>(&format!(
        "SELECT {BLOCK_COLUMNS} FROM utt_blocks \
         WHERE session_id = ? ORDER BY created_at DESC, id DESC LIMIT 1"
    ))
    .bind(session_id.to_string())
    .fetch_optional(pool)
    .await
    .storage_err("查询会话最近 utt 话语块失败")?;
    row.map(|r| r.into_block()).transpose()
}

/// 删除单个话语块（按主键）。
///
/// 用途:
/// - utt 增量构建：重切后删除过期尾块（仅最后一块会被删除，更早的块原样保留）。
///
/// 返回:
/// - `Ok(())`: 删除成功（块不存在时视为成功，幂等）。
pub async fn delete_by_id(pool: &SqlitePool, id: i64) -> RamariaResult<()> {
    sqlx::query("DELETE FROM utt_blocks WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .storage_err("删除 utt 话语块失败")?;
    Ok(())
}

/// 删除指定会话的全部话语块。
///
/// 用途:
/// - 会话删除/清理时同步清理原文块（原文随会话生命周期管理）。
/// - 全量重建 utt 索引前的会话级清理。
///
/// 返回:
/// - 删除的行数（无块时为 0）。
pub async fn delete_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<usize> {
    let result = sqlx::query("DELETE FROM utt_blocks WHERE session_id = ?")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .storage_err("删除会话 utt 话语块失败")?;
    let count = result.rows_affected() as usize;
    if count > 0 {
        tracing::info!(%session_id, count, "已删除会话的 utt 话语块");
    }
    Ok(count)
}

// =========================================================
// 单元测试（行映射与 SQL 语义，无 DB 依赖）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_mapping_parses_uuids() {
        let row = UttBlockRow {
            id: 7,
            persona_uid: "char-0001".to_string(),
            session_id: Uuid::new_v4().to_string(),
            start_msg_id: Uuid::new_v4().to_string(),
            end_msg_id: Uuid::new_v4().to_string(),
            block_text: "原文内容".to_string(),
            msg_count: 5,
            time_span_ms: 1_800_000,
            embedding: Some(vec![0u8, 0, 128, 63]),
            created_at: 1_700_000_000_000,
        };
        let block = row.into_block().expect("合法 UUID 应解析成功");
        assert_eq!(block.id, 7);
        assert_eq!(block.persona_uid, "char-0001");
        assert_eq!(block.msg_count, 5);
        assert_eq!(block.time_span_ms, 1_800_000);
        assert!(block.embedding.is_some());
    }

    #[test]
    fn row_mapping_invalid_uuid_returns_error() {
        let row = UttBlockRow {
            id: 1,
            persona_uid: "char-0001".to_string(),
            session_id: "not-a-uuid".to_string(),
            start_msg_id: Uuid::new_v4().to_string(),
            end_msg_id: Uuid::new_v4().to_string(),
            block_text: "原文".to_string(),
            msg_count: 1,
            time_span_ms: 0,
            embedding: None,
            created_at: 1_700_000_000_000,
        };
        let err = row.into_block().expect_err("非法 UUID 应返回错误");
        assert_eq!(err.category(), "validation");
    }

    #[test]
    fn new_block_defaults() {
        let session = Uuid::new_v4();
        let start = Uuid::new_v4();
        let end = Uuid::new_v4();
        let block = UttBlock::new(
            "char-0001".to_string(),
            session,
            start,
            end,
            "你好，最近怎么样？".to_string(),
            3,
            90_000,
        );
        assert_eq!(block.id, 0);
        assert!(block.embedding.is_none());
        assert!(block.created_at > 0);
    }
}
