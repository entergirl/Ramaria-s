//! rust/crates/ramaria-storage/src/repo/personas.rs - Persona CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Persona, PersonaKind};
use sqlx::SqlitePool;

fn parse_kind(s: &str) -> PersonaKind {
    match s {
        "user" => PersonaKind::User,
        "rama" => PersonaKind::Rama,
        "char" => PersonaKind::Char,
        "anim" => PersonaKind::Anim,
        "oc" => PersonaKind::Oc,
        _ => PersonaKind::Hist,
    }
}

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
            active: self.active != 0,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

pub async fn create(pool: &SqlitePool, p: &Persona) -> RamariaResult<i64> {
    let kind_str = p.kind.as_str();
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, ref_id, avatar, config, active, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&p.uid)
    .bind(&p.name)
    .bind(kind_str)
    .bind(p.seq)
    .bind(&p.source)
    .bind(&p.ref_id)
    .bind(&p.avatar)
    .bind(&p.config)
    .bind(p.active as i64)
    .bind(p.created_at)
    .bind(p.updated_at)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("创建 persona 失败", e))?;

    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("获取 persona id 失败", e))?;
    Ok(id)
}

pub async fn get_by_uid(pool: &SqlitePool, uid: &str) -> RamariaResult<Option<Persona>> {
    let row = sqlx::query_as::<_, PersonaRow>(
        "SELECT id, uid, name, kind, seq, source, ref_id, avatar, config, active, created_at, updated_at
         FROM personas WHERE uid = ?",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询 persona 失败", e))?;
    Ok(row.map(|r| r.into_persona()))
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<Persona>> {
    let rows = sqlx::query_as::<_, PersonaRow>(
        "SELECT id, uid, name, kind, seq, source, ref_id, avatar, config, active, created_at, updated_at
         FROM personas WHERE active = 1 ORDER BY kind, seq",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询 persona 列表失败", e))?;
    Ok(rows.into_iter().map(|r| r.into_persona()).collect())
}
