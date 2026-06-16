//! rust/crates/ramaria-storage/src/repo/personas.rs - Persona CRUD
//!
//! 设计特点:
//! - 管理 personas 表的创建、查询、更新和软删除
//! - uid 为全局业务标识（user-0001/rama-0001 等），id 为 AUTOINCREMENT 内部索引
//! - kind 解析失败时回退到 Hist 并记录 WARNING 日志

use crate::parse_enum_fallback;
use crate::repo::StorageResultExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Persona, PersonaKind};
use sqlx::SqlitePool;

parse_enum_fallback!(
    parse_kind, PersonaKind, PersonaKind::Hist, "personas", "kind",
    "user" => User,
    "rama" => Rama,
    "char" => Char,
    "anim" => Anim,
    "oc"   => Oc,
    "hist" => Hist,
);

#[derive(sqlx::FromRow)]
struct PersonaRow {
    id: i64,
    uid: String,
    name: String,
    kind: String,
    seq: i64,
    source: String,
    ref_id: Option<String>,
    avatar: Option<String>,
    config: Option<String>,
    description: Option<String>,
    active: i64,
    created_at: i64,
    updated_at: i64,
}

impl PersonaRow {
    fn into_persona(self) -> Persona {
        Persona {
            id: self.id,
            uid: self.uid,
            name: self.name,
            kind: parse_kind(&self.kind),
            seq: self.seq,
            source: self.source,
            ref_id: self.ref_id,
            avatar: self.avatar,
            config: self.config,
            description: self.description,
            active: self.active != 0,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

pub async fn create(pool: &SqlitePool, p: &Persona) -> RamariaResult<i64> {
    let kind_str = p.kind.as_str();
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO personas (uid, name, kind, seq, source, ref_id, avatar, config, description, active, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&p.uid)
    .bind(&p.name)
    .bind(kind_str)
    .bind(p.seq)
    .bind(&p.source)
    .bind(&p.ref_id)
    .bind(&p.avatar)
    .bind(&p.config)
    .bind(&p.description)
    .bind(p.active as i64)
    .bind(p.created_at)
    .bind(p.updated_at)
    .fetch_one(pool)
    .await
    .storage_err("创建 persona 失败")
}

pub async fn get_by_uid(pool: &SqlitePool, uid: &str) -> RamariaResult<Option<Persona>> {
    let row = sqlx::query_as::<_, PersonaRow>(
        "SELECT id, uid, name, kind, seq, source, ref_id, avatar, config, description, active, created_at, updated_at
         FROM personas WHERE uid = ?",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await
    .storage_err("查询 persona 失败")?;
    Ok(row.map(|r| r.into_persona()))
}

/// 按 (kind, source, ref_id) 查找已存在的 persona。
///
/// 用于防止 `idx_personas_kind_source_ref` UNIQUE 索引冲突：
/// 同一个来源方（如 QQ 账号）可能以不同 uid 多次导入，但 ref_id 相同。
///
/// 参数:
/// - `kind_str`: persona 类型（如 "char"）
/// - `source`: 来源（如 "qq"）
/// - `ref_id`: 来源方原始 ID（如 QQ UID）
///
/// 返回:
/// - 匹配的 persona 或 None。
pub async fn get_by_kind_source_ref(
    pool: &SqlitePool,
    kind_str: &str,
    source: &str,
    ref_id: &str,
) -> RamariaResult<Option<Persona>> {
    let row = sqlx::query_as::<_, PersonaRow>(
        "SELECT id, uid, name, kind, seq, source, ref_id, avatar, config, description, active, created_at, updated_at
         FROM personas WHERE kind = ? AND source = ? AND ref_id = ?",
    )
    .bind(kind_str)
    .bind(source)
    .bind(ref_id)
    .fetch_optional(pool)
    .await
    .storage_err("按 ref_id 查询 persona 失败")?;
    Ok(row.map(|r| r.into_persona()))
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<Persona>> {
    let rows = sqlx::query_as::<_, PersonaRow>(
        "SELECT id, uid, name, kind, seq, source, ref_id, avatar, config, description, active, created_at, updated_at
         FROM personas WHERE active = 1 ORDER BY kind, seq",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询 persona 列表失败")?;
    Ok(rows.into_iter().map(|r| r.into_persona()).collect())
}

/// 更新 persona 的可变字段（name / avatar / config / description）。
///
/// 部分更新语义:
/// - `name`: 必填，始终更新。
/// - `avatar`: `Some(val)` 更新为 val，`None` **保持旧值不变**（不设 NULL）。
/// - `config`: `Some(val)` 更新为 val，`None` **保持旧值不变**。
/// - `description`: `Some(val)` 更新为 val，`None` **保持旧值不变**。
/// - `uid` 不可变更，`updated_at` 自动刷新。
///
/// 使用 `sqlx::QueryBuilder` 动态构建 SET 子句，避免将 `None` 绑定为 SQL NULL。
pub async fn update(
    pool: &SqlitePool,
    uid: &str,
    name: &str,
    avatar: Option<&str>,
    config: Option<&str>,
    description: Option<&str>,
) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();

    let mut builder = sqlx::QueryBuilder::new("UPDATE personas SET name = ");
    builder.push_bind(name);

    if let Some(av) = avatar {
        builder.push(", avatar = ");
        builder.push_bind(av);
    }
    if let Some(cfg) = config {
        builder.push(", config = ");
        builder.push_bind(cfg);
    }
    if let Some(desc) = description {
        builder.push(", description = ");
        builder.push_bind(desc);
    }

    builder.push(", updated_at = ");
    builder.push_bind(now);
    builder.push(" WHERE uid = ");
    builder.push_bind(uid);

    let rows = builder
        .build()
        .execute(pool)
        .await
        .storage_err("更新 persona 失败")?;

    if rows.rows_affected() == 0 {
        return Err(RamariaError::storage(format!("persona 不存在: uid={uid}")));
    }
    Ok(())
}
