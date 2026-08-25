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

    sqlx::query(
        "INSERT INTO memory_l1 (id, session_id, summary, keywords, time_period, atmosphere,
         valence, salience, absorbed, created_at, last_accessed_at, persona_uid, context_json,
         situation_strength, evidence_notes, continuation)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(&l1.continuation)
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
         evidence_notes, continuation
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
         evidence_notes, continuation
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
         evidence_notes, continuation
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

/// 查询未吸收的"无主"L1（`persona_uid IS NULL`，导入产生的 L1 属此类）。
///
/// 用途:
/// - 重建检索索引时加载无主 L1（按 persona 查询查不到 NULL 记录，
///   但检索侧对 NULL persona 文档不做过滤，任何画像可命中）。
pub async fn list_unabsorbed_unbound(pool: &SqlitePool) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query_as::<_, L1Row>(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, valence, salience,
         absorbed, created_at, last_accessed_at, persona_uid, context_json, situation_strength,
         evidence_notes, continuation
         FROM memory_l1 WHERE absorbed = 0 AND persona_uid IS NULL ORDER BY created_at ASC",
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
        let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
        let sql = format!(
            "UPDATE memory_l1 SET persona_uid = ? \
             WHERE persona_uid IS NULL AND absorbed = 0 AND id IN ({})",
            placeholders.join(", ")
        );

        // 参数顺序：第 1 个是 persona_uid，其余是 L1 id（与 SQL 占位符一一对应）
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
         evidence_notes, continuation
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

// =========================================================
// 测试（evidence_notes 结构化读写集成测试）
// =========================================================
//
// 说明:
// - 使用内存 SQLite 真库（database::init_test_pool 自动应用全部 migration）。
// - 迁移后读取测试手动构造"旧库（基线 schema + 旧格式数据）"再应用一次性迁移，
//   验证迁移产物可被 repo 结构化读取。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;

    /// 构造一条带完整槽位的 MemoryL1（text/time/who/cause 全填）。
    fn make_l1_with_slots(session_id: Uuid) -> MemoryL1 {
        MemoryL1 {
            id: Uuid::new_v4(),
            session_id,
            summary: "用户讨论项目延期安排".into(),
            keywords: Some("项目,延期,排期".into()),
            time_period: Some("下午".into()),
            atmosphere: Some("紧张".into()),
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: 1_700_000_000_000,
            last_accessed_at: None,
            persona_uid: Some("char-0001".into()),
            context_json: None,
            situation_strength: Some(4),
            evidence_notes: Some(vec![EvidenceNote {
                text: "用户提到项目延期到月底".into(),
                time: Some("上周三".into()),
                who: Some("用户".into()),
                cause: Some("需求变更频繁".into()),
            }]),
            continuation: None,
        }
    }

    /// 创建测试 fixture：persona + session（满足 memory_l1 的外键约束）。
    ///
    /// 说明: init_test_pool 每次创建全新内存库，必须显式插入引用数据，
    /// 否则 save 触发 FOREIGN KEY constraint failed。
    async fn setup_fixture(pool: &SqlitePool) -> Uuid {
        sqlx::query(
            "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
             VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0)",
        )
        .execute(pool)
        .await
        .expect("插入 persona fixture 应成功");
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
            .bind(session_id.to_string())
            .execute(pool)
            .await
            .expect("插入 session fixture 应成功");
        session_id
    }

    /// 新格式往返：save → get，结构化槽位完整保留（验收 1）。
    #[tokio::test]
    async fn evidence_notes_structured_roundtrip_via_get() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;
        let l1 = make_l1_with_slots(session_id);

        save(&pool, &l1).await.expect("save 应成功");

        let got = get(&pool, l1.id)
            .await
            .expect("get 应成功")
            .expect("应存在");
        assert_eq!(got.summary, l1.summary);
        let notes = got.evidence_notes.expect("evidence_notes 不应为 None");
        assert_eq!(notes.len(), 1, "1 条结构化线索应往返保留");
        assert_eq!(notes[0].text, "用户提到项目延期到月底");
        assert_eq!(notes[0].time.as_deref(), Some("上周三"));
        assert_eq!(notes[0].who.as_deref(), Some("用户"));
        assert_eq!(notes[0].cause.as_deref(), Some("需求变更频繁"));
    }

    /// 新格式往返：save → list_by_session，多条含/不含槽位的线索均完整（验收 1）。
    #[tokio::test]
    async fn evidence_notes_structured_roundtrip_via_list_by_session() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;
        let mut l1 = make_l1_with_slots(session_id);
        // 追加一条仅 text 的线索（可选槽位缺省）
        l1.evidence_notes
            .as_mut()
            .unwrap()
            .push(EvidenceNote::new("用户表示新方案还在评估中"));

        save(&pool, &l1).await.expect("save 应成功");

        let list = list_by_session(&pool, session_id)
            .await
            .expect("list_by_session 应成功");
        assert_eq!(list.len(), 1);
        let notes = list[0].evidence_notes.as_ref().unwrap();
        assert_eq!(notes.len(), 2);
        // 第一条：完整槽位
        assert_eq!(notes[0].cause.as_deref(), Some("需求变更频繁"));
        // 第二条：仅 text，可选槽位为 None
        assert_eq!(notes[1].text, "用户表示新方案还在评估中");
        assert!(notes[1].time.is_none());
        assert!(notes[1].who.is_none());
        assert!(notes[1].cause.is_none());
    }

    /// 空值往返：evidence_notes 为 None → save → get 仍为 None。
    #[tokio::test]
    async fn evidence_notes_none_roundtrip() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;
        let mut l1 = make_l1_with_slots(session_id);
        l1.evidence_notes = None;

        save(&pool, &l1).await.expect("save 应成功");

        let got = get(&pool, l1.id).await.unwrap().unwrap();
        assert!(got.evidence_notes.is_none(), "None 应保持 None（DB NULL）");
    }

    /// 空数组往返：evidence_notes 为 Some(vec![]) → save → get 读回空数组
    /// （存储为 `[]` 而非 NULL，与 None 语义区分；验收 3）。
    #[tokio::test]
    async fn evidence_notes_empty_array_roundtrip() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;
        let mut l1 = make_l1_with_slots(session_id);
        l1.evidence_notes = Some(vec![]);

        save(&pool, &l1).await.expect("save 应成功");

        let got = get(&pool, l1.id).await.unwrap().unwrap();
        let notes = got.evidence_notes.expect("空数组应读回 Some(vec![])");
        assert!(notes.is_empty(), "空数组应往返为空数组");
    }

    /// continuation 往返：Some("延续") → save → get 读回；None → 读回 None
    /// （v1.5 B2 存储层验收）。
    #[tokio::test]
    async fn continuation_roundtrip() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;

        // Some("延续") 往返
        let mut l1 = make_l1_with_slots(session_id);
        l1.continuation = Some("延续".to_string());
        save(&pool, &l1).await.expect("save 应成功");
        let got = get(&pool, l1.id).await.unwrap().unwrap();
        assert_eq!(got.continuation.as_deref(), Some("延续"));

        // 非法枚举值（防御：若上游未校验，存储层原样保存）——仅验证读写一致性
        let mut l1b = make_l1_with_slots(session_id);
        l1b.id = Uuid::new_v4();
        l1b.continuation = Some("无关".to_string());
        save(&pool, &l1b).await.expect("save 应成功");
        let got_b = get(&pool, l1b.id).await.unwrap().unwrap();
        assert_eq!(got_b.continuation.as_deref(), Some("无关"));

        // None（首块/独立摘要路径）往返
        let mut l1c = make_l1_with_slots(session_id);
        l1c.id = Uuid::new_v4();
        l1c.continuation = None;
        save(&pool, &l1c).await.expect("save 应成功");
        let got_c = get(&pool, l1c.id).await.unwrap().unwrap();
        assert!(got_c.continuation.is_none(), "None 应往返为 None");
    }

    /// 多槽位 JSON 序列化不含空槽位（skip_serializing_if）：存储紧凑且迁移幂等友好。
    #[tokio::test]
    async fn evidence_notes_serialized_json_omits_empty_slots() {
        let note = EvidenceNote::new("用户提到周末加班");
        let json = serde_json::to_string(&vec![note]).unwrap();
        assert!(
            !json.contains("time") && !json.contains("who") && !json.contains("cause"),
            "空槽位不应出现在存储 JSON 中: {json}"
        );
        assert!(json.contains("用户提到周末加班"));
    }

    // =========================================================
    // assign_persona_uid：无主 L1 归属（数据断层修复链路）
    // =========================================================

    /// 构造一条无主 L1（persona_uid = None），复用 make_l1_with_slots 的槽位。
    fn make_unbound_l1(session_id: Uuid) -> MemoryL1 {
        let mut l1 = make_l1_with_slots(session_id);
        l1.persona_uid = None;
        l1
    }

    /// 基本归属：无主未吸收 L1 被归属到目标 persona，重复归属幂等。
    #[tokio::test]
    async fn assign_persona_uid_attributes_unbound() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;

        // 3 条无主 L1（persona_uid=NULL）
        let l1a = make_unbound_l1(session_id);
        let l1b = make_unbound_l1(session_id);
        let l1c = make_unbound_l1(session_id);
        for l1 in [&l1a, &l1b, &l1c] {
            save(&pool, l1).await.expect("save 应成功");
        }

        // 归属其中 2 条到 char-0001
        let assigned = assign_persona_uid(&pool, &[l1a.id, l1b.id], "char-0001")
            .await
            .expect("归属应成功");
        assert_eq!(assigned, 2, "应归属 2 条无主 L1");

        // 校验：目标 persona 读回 2 条；无主通道读回 1 条
        let bound = list_unabsorbed(&pool, "char-0001")
            .await
            .expect("查询应成功");
        assert_eq!(bound.len(), 2, "char-0001 应读到 2 条");
        assert!(
            bound
                .iter()
                .all(|l| l.persona_uid.as_deref() == Some("char-0001"))
        );

        let unbound = list_unabsorbed_unbound(&pool).await.expect("查询应成功");
        assert_eq!(unbound.len(), 1, "无主通道应剩 1 条");
        assert_eq!(unbound[0].id, l1c.id, "剩余应为未归属的 l1c");

        // 幂等：重复归属同一批 → 不再更新（已归属）
        let assigned2 = assign_persona_uid(&pool, &[l1a.id, l1b.id], "char-0001")
            .await
            .expect("重复归属应成功");
        assert_eq!(assigned2, 0, "重复归属应为 0（幂等）");
    }

    /// 边界：不覆盖既有归属；不触碰已吸收记录；空 id 列表返回 0。
    #[tokio::test]
    async fn assign_persona_uid_respects_boundaries() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;

        // 1) 已有归属的 L1：尝试重新归属到其他 persona → 不被覆盖
        let l1_bound = make_l1_with_slots(session_id); // persona_uid=char-0001
        save(&pool, &l1_bound).await.expect("save 应成功");
        let assigned = assign_persona_uid(&pool, &[l1_bound.id], "char-9999")
            .await
            .expect("归属应成功");
        assert_eq!(assigned, 0, "既有归属不应被覆盖");
        let got = get(&pool, l1_bound.id).await.unwrap().unwrap();
        assert_eq!(got.persona_uid.as_deref(), Some("char-0001"), "原归属保持");

        // 2) 已吸收的无主 L1：不触碰（absorbed=1）
        let mut l1_absorbed = make_unbound_l1(session_id);
        l1_absorbed.id = Uuid::new_v4();
        l1_absorbed.absorbed = true;
        save(&pool, &l1_absorbed).await.expect("save 应成功");
        let assigned2 = assign_persona_uid(&pool, &[l1_absorbed.id], "char-0001")
            .await
            .expect("归属应成功");
        assert_eq!(assigned2, 0, "已吸收记录不应被触碰");
        let got2 = get(&pool, l1_absorbed.id).await.unwrap().unwrap();
        assert!(got2.persona_uid.is_none(), "已吸收记录保持无主");

        // 3) 空 id 列表 → 返回 0
        let assigned3 = assign_persona_uid(&pool, &[], "char-0001")
            .await
            .expect("空列表应成功");
        assert_eq!(assigned3, 0, "空列表应返回 0");
    }

    /// 大批量归属分批（>100 条）：全部归属成功（SQLite 参数上限保护）。
    #[tokio::test]
    async fn assign_persona_uid_batch_chunking() {
        let pool = database::init_test_pool().await.unwrap();
        let session_id = setup_fixture(&pool).await;

        // 250 条无主 L1（跨 3 批：100+100+50）
        let mut ids = Vec::with_capacity(250);
        for _ in 0..250 {
            let l1 = make_unbound_l1(session_id);
            ids.push(l1.id);
            save(&pool, &l1).await.expect("save 应成功");
        }

        let assigned = assign_persona_uid(&pool, &ids, "char-0001")
            .await
            .expect("分批归属应成功");
        assert_eq!(assigned, 250, "全部 250 条应归属成功");

        let bound = list_unabsorbed(&pool, "char-0001")
            .await
            .expect("查询应成功");
        assert_eq!(bound.len(), 250, "归属后目标 persona 应有 250 条");
    }
}
