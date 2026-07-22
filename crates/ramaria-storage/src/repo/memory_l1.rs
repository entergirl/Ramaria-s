//! rust/crates/ramaria-storage/src/repo/memory_l1.rs - L1 单次会话摘要存取模块
//!
//! 设计特点:
//! - id 使用 UUID v4（TEXT 主键），与 sessions/messages 保持 ID 类型一致
//! - 支持按 session_id 查询、按 persona_uid 过滤未吸收记录
//! - mark_absorbed 在事务中批量执行，确保 L1→L2 吸收操作的原子性
//! - absorbed 字段在 SQLite 中存为 INTEGER（0/1），读取时还原为 bool
//! - persona_uid 和 context_json 为 新增列，支持人格关联和分组键
//! - situation_strength 为 新增列（默认 NULL，等效 3），
//!   避免存量 NULL 值使加权逻辑跳过记录

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryL1;
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct L1Row {
    id: String,
    session_id: String,
    summary: String,
    keywords: Option<String>,
    time_period: Option<String>,
    atmosphere: Option<String>,
    valence: f64,
    salience: f64,
    absorbed: i64,
    created_at: i64,
    last_accessed_at: Option<i64>,
    persona_uid: Option<String>,
    context_json: Option<String>,
    situation_strength: Option<i64>,
    /// 证据片段（JSON 数组字符串），存量数据为 NULL
    evidence_notes: Option<String>,
}

impl L1Row {
    fn into_l1(self) -> RamariaResult<MemoryL1> {
        let id = parse_uuid_required(&self.id, "memory_l1", "id")?;
        let session_id = parse_uuid_required(&self.session_id, "memory_l1", "session_id")?;

        // evidence_notes: TEXT 存储 JSON 数组，反序列化为 Vec<String>
        let evidence_notes = self
            .evidence_notes
            .map(|s| serde_json::from_str::<Vec<String>>(&s))
            .transpose()
            .map_err(|e| {
                ramaria_core::error::RamariaError::validation(format!(
                    "memory_l1.evidence_notes 解析失败 (id={}): {e}",
                    self.id
                ))
            })?;

        Ok(MemoryL1 {
            id,
            session_id,
            summary: self.summary,
            keywords: self.keywords,
            time_period: self.time_period,
            atmosphere: self.atmosphere,
            valence: self.valence,
            salience: self.salience,
            absorbed: self.absorbed != 0,
            created_at: self.created_at,
            last_accessed_at: self.last_accessed_at,
            persona_uid: self.persona_uid,
            context_json: self.context_json,
            situation_strength: self.situation_strength.map(|v| v as i32),
            evidence_notes,
        })
    }
}

pub async fn save(pool: &SqlitePool, l1: &MemoryL1) -> RamariaResult<()> {
    // evidence_notes: Vec<String> → JSON 数组字符串存储
    let evidence_notes_json = l1
        .evidence_notes
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| {
            ramaria_core::error::RamariaError::validation(format!(
                "MemoryL1.evidence_notes 序列化失败: {e}"
            ))
        })?;

    sqlx::query(
        "INSERT INTO memory_l1 (id, session_id, summary, keywords, time_period, atmosphere,
         valence, salience, absorbed, created_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(l1.id.to_string())
    .bind(l1.session_id.to_string())
    .bind(&l1.summary)
    .bind(&l1.keywords)
    .bind(&l1.time_period)
    .bind(&l1.atmosphere)
    .bind(l1.valence)
    .bind(l1.salience)
    .bind(l1.absorbed as i64)
    .bind(l1.created_at)
    .bind(l1.last_accessed_at)
    .bind(&l1.persona_uid)
    .bind(&l1.context_json)
    .bind(l1.situation_strength.map(|v| v as i64))
    .bind(evidence_notes_json)
    .execute(pool)
    .await
    .storage_err("保存 L1 记忆失败")?;
    Ok(())
}

/// 删除指定 session 中 persona_uid 为 NULL 的 L1 摘要。
///
/// 用法:
/// - `regenerate_l1_no_cascade` 在重新生成 L1 前调用，仅清理旧 NULL 记录。
/// - 已有正确 persona_uid 的 L1 不会被删除（幂等安全——可重复调用）。
pub async fn delete_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<usize> {
    let result = sqlx::query("DELETE FROM memory_l1 WHERE session_id = ? AND persona_uid IS NULL")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .storage_err("删除 session L1 摘要失败")?;
    let count = result.rows_affected() as usize;
    if count > 0 {
        tracing::info!(%session_id, count, "已清理 session 的旧 NULL-persona_uid L1 摘要");
    }
    Ok(count)
}

pub async fn list_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json, situation_strength,
         evidence_notes
         FROM memory_l1 WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .storage_err("查询 L1 列表失败")?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}

pub async fn get(pool: &SqlitePool, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
    let row = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json, situation_strength,
         evidence_notes
         FROM memory_l1 WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await
    .storage_err("查询 L1 失败")?;
    row.map(|r| r.into_l1()).transpose()
}

pub async fn mark_absorbed(pool: &SqlitePool, l1_ids: &[Uuid]) -> RamariaResult<()> {
    if l1_ids.is_empty() {
        return Ok(());
    }

    // 分批处理：每批最多 100 条，避免 SQL 语句过长（SQLite 默认参数限制 999 个）
    const BATCH_SIZE: usize = 100;

    // 事务包裹：确保批量标记的原子性——全部成功或全部回滚
    let mut tx = pool.begin().await.storage_err("开启吸收标记事务失败")?;

    for chunk in l1_ids.chunks(BATCH_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "UPDATE memory_l1 SET absorbed = 1 WHERE id IN ({})",
            placeholders.join(", ")
        );

        let mut query = sqlx::query(&sql);
        for id in chunk {
            query = query.bind(id.to_string());
        }

        query
            .execute(&mut *tx)
            .await
            .storage_err(format!("标记 {} 条 L1 已吸收失败", chunk.len()))?;
    }

    tx.commit().await.storage_err("提交吸收标记事务失败")?;

    tracing::info!(
        total = l1_ids.len(),
        batches = l1_ids.len().div_ceil(BATCH_SIZE),
        "批量标记 L1 已吸收完成"
    );

    Ok(())
}

pub async fn list_unabsorbed(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json, situation_strength,
         evidence_notes
         FROM memory_l1 WHERE absorbed = 0 AND persona_uid = ? ORDER BY created_at ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询未吸收 L1 失败")?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 按创建时间降序获取指定 persona 的最近 N 条 L1 摘要。
///
/// 用法:
/// - 供跨 session 上下文注入：新 session 创建时自动加载最近对话摘要。
/// - 不区分 absorbed 状态——即使已被 L2 吸收，近期摘要仍有叙事价值。
///
/// 参数:
/// - `persona_uid`: 人格标识。
/// - `limit`: 最多返回条数。
///
/// 返回:
/// - 按 `created_at DESC` 排序的 MemoryL1 列表。
pub async fn list_recent_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    limit: u32,
) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json, situation_strength,
         evidence_notes
         FROM memory_l1 WHERE persona_uid = ? ORDER BY created_at DESC LIMIT ?",
    )
    .bind(persona_uid)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .storage_err("查询最近 L1 摘要失败")?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}
