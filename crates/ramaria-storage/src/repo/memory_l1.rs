//! rust/crates/ramaria-storage/src/repo/memory_l1.rs - L1 单次会话摘要存取模块
//!
//! 设计特点:
//! - id 使用 UUID v4（TEXT 主键），与 sessions/messages 保持 ID 类型一致
//! - 支持按 session_id 查询、按 persona_uid 过滤未吸收记录
//! - mark_absorbed 在事务中批量执行，确保 L1→L2 吸收操作的原子性
//! - absorbed 字段在 SQLite 中存为 INTEGER（0/1），读取时还原为 bool
//! - persona_uid 和 context_json 为 Phase 1.5 新增列，支持人格关联和分组键

use ramaria_core::error::{RamariaError, RamariaResult};
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
}

impl L1Row {
    fn into_l1(self) -> RamariaResult<MemoryL1> {
        let id = ramaria_core::types::uuid_from_db(&self.id)
            .inspect_err(|_| tracing::warn!(raw_id = %self.id, "memory_l1.id UUID 解析失败"))?;
        let session_id = ramaria_core::types::uuid_from_db(&self.session_id).inspect_err(
            |_| tracing::warn!(raw_id = %self.session_id, "memory_l1.session_id UUID 解析失败"),
        )?;
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
        })
    }
}

pub async fn save(pool: &SqlitePool, l1: &MemoryL1) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO memory_l1 (id, session_id, summary, keywords, time_period, atmosphere,
         valence, salience, absorbed, created_at, last_accessed_at, persona_uid, context_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存 L1 记忆失败", e))?;
    Ok(())
}

pub async fn list_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json
         FROM memory_l1 WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询 L1 列表失败", e))?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}

pub async fn get(pool: &SqlitePool, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
    let row = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json
         FROM memory_l1 WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询 L1 失败", e))?;
    row.map(|r| r.into_l1()).transpose()
}

pub async fn mark_absorbed(pool: &SqlitePool, l1_ids: &[Uuid]) -> RamariaResult<()> {
    if l1_ids.is_empty() {
        return Ok(());
    }

    // 分批处理：每批最多 100 条，避免 SQL 语句过长（SQLite 默认参数限制 999 个）
    const BATCH_SIZE: usize = 100;

    // 事务包裹：确保批量标记的原子性——全部成功或全部回滚
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| RamariaError::storage_with_source("开启吸收标记事务失败", e))?;

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

        query.execute(&mut *tx).await.map_err(|e| {
            RamariaError::storage_with_source(format!("标记 {} 条 L1 已吸收失败", chunk.len()), e)
        })?;
    }

    tx.commit()
        .await
        .map_err(|e| RamariaError::storage_with_source("提交吸收标记事务失败", e))?;

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
         absorbed, created_at, last_accessed_at, persona_uid, context_json
         FROM memory_l1 WHERE absorbed = 0 AND persona_uid = ? ORDER BY created_at ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询未吸收 L1 失败", e))?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}
