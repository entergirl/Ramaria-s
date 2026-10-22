//! crates/ramaria-storage/src/repo/memory_l1.rs - L1 单次会话摘要存取模块
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
use crate::retry::with_busy_retry;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{EvidenceNote, MemoryL1};
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
    /// 发生时间（Unix 毫秒，可空）——块内最早消息时间；NULL 表示未回填
    occurred_at: Option<i64>,
    last_accessed_at: Option<i64>,
    persona_uid: Option<String>,
    context_json: Option<String>,
    situation_strength: Option<i64>,
    /// 证据线索（JSON 对象数组字符串），存量数据为 NULL
    evidence_notes: Option<String>,
    /// 与上一块的话题延续关系（v1.5 B2）："延续" | "转折" | "无关"，NULL=无上一块
    continuation: Option<String>,
}

impl L1Row {
    fn into_l1(self) -> RamariaResult<MemoryL1> {
        let id = parse_uuid_required(&self.id, "memory_l1", "id")?;
        let session_id = parse_uuid_required(&self.session_id, "memory_l1", "session_id")?;

        // evidence_notes: TEXT 存储 JSON 对象数组，反序列化为 Vec<EvidenceNote>
        // （v1.4 结构化格式：{text, time?, who?, cause?}，由 migration 一次性迁移）
        let evidence_notes = self
            .evidence_notes
            .map(|s| serde_json::from_str::<Vec<EvidenceNote>>(&s))
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
            occurred_at: self.occurred_at,
            last_accessed_at: self.last_accessed_at,
            persona_uid: self.persona_uid,
            context_json: self.context_json,
            situation_strength: self.situation_strength.map(|v| v as i32),
            evidence_notes,
            continuation: self.continuation,
        })
    }
}

pub async fn save(pool: &SqlitePool, l1: &MemoryL1) -> RamariaResult<()> {
    // evidence_notes: Vec<EvidenceNote> → JSON 对象数组字符串存储
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

    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("保存 L1 记忆", || async {
        sqlx::query(
            "INSERT INTO memory_l1 (id, session_id, summary, keywords, time_period, atmosphere,
             valence, salience, absorbed, created_at, occurred_at, last_accessed_at, persona_uid,
             context_json, situation_strength, evidence_notes, continuation)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(l1.occurred_at)
        .bind(l1.last_accessed_at)
        .bind(&l1.persona_uid)
        .bind(&l1.context_json)
        .bind(l1.situation_strength.map(|v| v as i64))
        .bind(evidence_notes_json.as_deref())
        .bind(&l1.continuation)
        .execute(pool)
        .await
    })
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
         absorbed, created_at, occurred_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes, continuation
         FROM memory_l1 WHERE session_id = ? ORDER BY COALESCE(occurred_at, created_at) ASC",
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
         absorbed, created_at, occurred_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes, continuation
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

/// 更新指定 L1 摘要的最后访问时间（检索命中接线，激活 `[decay] recent_boost_*`）。
///
/// 职责:
/// - 检索命中后把 `last_accessed_at` 刷新到当前时间，使近期被检索的记忆
///   在下次检索的衰减排序中获得访问加成（保留率不低于 `recent_boost_floor`）。
/// - 与 `mark_absorbed` 保持一致的分批事务策略，保证批量更新的原子性。
///
/// 参数:
/// - `pool`: SQLite 连接池。
/// - `l1_ids`: 命中的 L1 摘要 id 列表（空列表直接成功）。
/// - `now_ms`: 访问时间戳（Unix 毫秒）。
///
/// 返回:
/// - `Ok(())`: 更新成功。
pub async fn touch(pool: &SqlitePool, l1_ids: &[Uuid], now_ms: i64) -> RamariaResult<()> {
    if l1_ids.is_empty() {
        return Ok(());
    }

    // 分批处理：每批最多 100 条，避免 SQL 语句过长（SQLite 默认参数限制 999 个）
    const BATCH_SIZE: usize = 100;
    // 事务包裹：与 mark_absorbed 对齐，保证批量刷新访问时间的原子性
    let mut tx = pool
        .begin()
        .await
        .storage_err("开启 L1 访问时间刷新事务失败")?;

    for chunk in l1_ids.chunks(BATCH_SIZE) {
        // 命名占位符：?1 = now_ms，?2..?N = l1 id。
        // 注意：不可混用匿名 `?`（sqlx 会编号为 ?1 与手写 ?1 撞车）。
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 2)).collect();
        let sql = format!(
            "UPDATE memory_l1 SET last_accessed_at = ?1 WHERE id IN ({})",
            placeholders.join(", ")
        );

        let mut query = sqlx::query(&sql).bind(now_ms);
        for id in chunk {
            query = query.bind(id.to_string());
        }

        query
            .execute(&mut *tx)
            .await
            .storage_err(format!("刷新 {} 条 L1 访问时间失败", chunk.len()))?;
    }

    tx.commit()
        .await
        .storage_err("提交 L1 访问时间刷新事务失败")?;

    tracing::debug!(
        total = l1_ids.len(),
        batches = l1_ids.len().div_ceil(BATCH_SIZE),
        "L1 访问时间已刷新（touch）"
    );

    Ok(())
}

pub async fn list_unabsorbed(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, occurred_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes, continuation
         FROM memory_l1 WHERE absorbed = 0 AND persona_uid = ? ORDER BY COALESCE(occurred_at, created_at) ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询未吸收 L1 失败")?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 查询未吸收的"无主"L1（`persona_uid IS NULL`，导入产生的 L1 属此类）。
///
/// 用途:
/// - 重建检索索引时加载无主 L1（按 persona 查询查不到 NULL 记录，
///   但检索侧对 NULL persona 文档不做过滤，任何画像可命中）。
pub async fn list_unabsorbed_unbound(pool: &SqlitePool) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, occurred_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes, continuation
         FROM memory_l1 WHERE absorbed = 0 AND persona_uid IS NULL ORDER BY COALESCE(occurred_at, created_at) ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询未吸收的无主 L1 失败")?;
    rows.into_iter()
        .map(|r| r.into_l1())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 批量把"无主"L1（`persona_uid IS NULL` 且未吸收）归属到指定 persona。
///
/// 语义:
/// - 导入产生的 L1 固定 NULL 归属，导致 L2 按 persona 的触发查询无法命中；
///   本方法在 L2 触发时把候选组归属到来源 session 的 persona，打通 L1→L2 链路。
/// - 幂等：仅更新 `persona_uid IS NULL AND absorbed = 0` 的记录，
///   重复归属不会覆盖既有归属，也不触碰已吸收数据。
///
/// 返回:
/// - 实际更新的条数（可能小于 `l1_ids.len()`：部分已归属/已吸收时）。
pub async fn assign_persona_uid(
    pool: &SqlitePool,
    l1_ids: &[Uuid],
    persona_uid: &str,
) -> RamariaResult<usize> {
    if l1_ids.is_empty() {
        return Ok(0);
    }

    // 分批处理：每批最多 100 条，避免 SQL 语句过长（SQLite 默认参数限制 999 个）。
    // 与 mark_absorbed 保持一致的分批策略。
    const BATCH_SIZE: usize = 100;
    let mut assigned = 0usize;

    for chunk in l1_ids.chunks(BATCH_SIZE) {
        // 命名占位符：?1 = persona_uid，?2..?N = l1 id。
        // 不可混用匿名 `?`（sqlx 会自行为其编号，与手写编号撞车）。
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 2)).collect();
        let sql = format!(
            "UPDATE memory_l1 SET persona_uid = ?1 \
             WHERE persona_uid IS NULL AND absorbed = 0 AND id IN ({})",
            placeholders.join(", ")
        );

        // 参数顺序：第 1 个是 persona_uid（?1），其余是 L1 id（?2..?N）
        let mut q = sqlx::query(&sql).bind(persona_uid);
        for id in chunk {
            q = q.bind(id.to_string());
        }
        let res = q
            .execute(pool)
            .await
            .storage_err("批量归属无主 L1 到 persona 失败")?;
        assigned += res.rows_affected() as usize;
    }

    tracing::info!(
        total = l1_ids.len(),
        assigned,
        persona_uid,
        "无主 L1 批量归属到 persona 完成"
    );
    Ok(assigned)
}

/// 按发生时间降序获取指定 persona 的最近 N 条 L1 摘要。
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
/// - 按 `COALESCE(occurred_at, created_at) DESC` 排序的 MemoryL1 列表。
pub async fn list_recent_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    limit: u32,
) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, occurred_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes, continuation
         FROM memory_l1 WHERE persona_uid = ? ORDER BY COALESCE(occurred_at, created_at) DESC LIMIT ?",
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

// =========================================================
// 测试（evidence_notes 结构化读写集成测试）
// =========================================================
//
// 说明:
// - 使用内存 SQLite 真库（database::init_test_pool 自动应用全部 migration）。
// - 迁移产物（新增列 / 旧格式数据）可被 repo 结构化读取。

#[cfg(test)]
mod tests;
