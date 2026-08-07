//! rust/crates/ramaria-storage/src/repo/examples.rs - PersonaExample CRUD
//!
//! 设计特点:
//! - 管理对话 Few-shot 示例（partner→reply），用于 System Prompt 注入
//! - list_selected 仅返回 selected=1 的示例，最多 5 条

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_optional;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::PersonaExample;
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct ExampleRow {
    id: i64,
    persona_uid: String,
    partner: String,
    reply: String,
    session_id: Option<String>,
    context: Option<String>,
    valence: f64,
    tags: Option<String>,
    selected: i64,
    length: i64,
    created_at: i64,
}

impl ExampleRow {
    fn into_example(self) -> RamariaResult<PersonaExample> {
        let session_id = parse_uuid_optional(&self.session_id, "persona_examples", "session_id")?;
        Ok(PersonaExample {
            id: self.id,
            persona_uid: self.persona_uid,
            partner: self.partner,
            reply: self.reply,
            session_id,
            context: self.context,
            valence: self.valence,
            tags: self.tags,
            selected: self.selected != 0,
            length: self.length as i32,
            created_at: self.created_at,
        })
    }
}

pub async fn save(pool: &SqlitePool, e: &PersonaExample) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO persona_examples (persona_uid, partner, reply, session_id, context, valence, tags, selected, length, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&e.persona_uid).bind(&e.partner).bind(&e.reply)
    .bind(e.session_id.map(|u| u.to_string())).bind(&e.context)
    .bind(e.valence).bind(&e.tags).bind(e.selected as i64).bind(e.length).bind(e.created_at)
    .fetch_one(pool).await
    .storage_err("保存示例失败")
}

pub async fn list_selected(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<PersonaExample>> {
    let rows = sqlx::query_as::<_, ExampleRow>(
        "SELECT id, persona_uid, partner, reply, session_id, context, valence, tags, selected, length, created_at
         FROM persona_examples WHERE persona_uid = ? AND selected = 1 ORDER BY created_at DESC LIMIT 5"
    ).bind(persona_uid).fetch_all(pool).await
        .storage_err("查询示例列表失败")?;
    rows.into_iter()
        .map(|r| r.into_example())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 候选池上限：防止长期使用后评分轮换退化（注入只取前 N 条，全量无意义）。
const LIST_ALL_LIMIT: u32 = 500;

/// 查询 persona 的全部示例候选（不区分 selected，供评分轮换）。
///
/// 说明:
/// - 按创建时间降序，最多 [`LIST_ALL_LIMIT`] 条（候选池防御上限）。
/// - `list_selected` 保留不动（v1.3 兼容路径，`examples.enabled=false` 时使用）。
pub async fn list_all(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<PersonaExample>> {
    let rows = sqlx::query_as::<_, ExampleRow>(
        "SELECT id, persona_uid, partner, reply, session_id, context, valence, tags, selected, length, created_at
         FROM persona_examples WHERE persona_uid = ? ORDER BY created_at DESC LIMIT ?"
    ).bind(persona_uid).bind(LIST_ALL_LIMIT).fetch_all(pool).await
        .storage_err("查询示例候选池失败")?;
    rows.into_iter()
        .map(|r| r.into_example())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 按 (partner, reply) 精确查重：抽取入库前判定是否已存在。
///
/// 用途:
/// - examples 写侧激活（v1.4）：重复回复对不重复入库（幂等）。
///
/// 返回:
/// - `Ok(Some(ex))`: 已存在同对示例。
/// - `Ok(None)`: 不存在（可入库）。
pub async fn find_by_pair(
    pool: &SqlitePool,
    persona_uid: &str,
    partner: &str,
    reply: &str,
) -> RamariaResult<Option<PersonaExample>> {
    let row = sqlx::query_as::<_, ExampleRow>(
        "SELECT id, persona_uid, partner, reply, session_id, context, valence, tags, selected, length, created_at
         FROM persona_examples WHERE persona_uid = ? AND partner = ? AND reply = ? LIMIT 1"
    )
    .bind(persona_uid)
    .bind(partner)
    .bind(reply)
    .fetch_optional(pool)
    .await
    .storage_err("查询示例查重失败")?;
    row.map(|r| r.into_example()).transpose()
}
