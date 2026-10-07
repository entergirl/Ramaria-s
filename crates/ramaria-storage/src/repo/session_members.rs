//! crates/ramaria-storage/src/repo/session_members.rs - 会话成员行存取模块
//!
//! 设计特点:
//! - upsert 语义：同一 (session_id, platform_ref) 重复写入只更新行，不重复插入
//! - 首见时间取小、末见时间取大：乱序批次与重复导入不破坏时间单调性
//! - 名称保留非空值：解析缺失（空名）不清空已记录名称

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{MemberRole, SessionMember};
use sqlx::SqlitePool;
use uuid::Uuid;

// =========================================================
// 写入
// =========================================================

/// 批量 upsert 会话成员行（单事务）。
///
/// 职责:
/// - 会话成员身份写入的单一入口：导入写入层与后续社交入口共用。
///
/// 合并口径（同键冲突时）:
/// - `name` 取新值仅当非空；群名片 / 角色取新值仅当 Some（不清空已记录信息）；
/// - `first_seen_at` 取小、`last_seen_at` 取大（乱序与重复导入下保持单调）。
///
/// 参数:
/// - `members`: 成员行列表；空列表直接成功（不开启事务）。
///
/// 返回:
/// - `Ok(())`: 全部写入成功（事务已提交）。
/// - `Err(...)`: 任一条失败时整批回滚。
pub async fn upsert_batch(pool: &SqlitePool, members: &[SessionMember]) -> RamariaResult<()> {
    if members.is_empty() {
        return Ok(());
    }

    let mut txn = pool.begin().await.storage_err("开启会话成员事务失败")?;
    for member in members {
        sqlx::query(
            "INSERT INTO session_members \
                 (session_id, platform_ref, name, group_nickname, role, first_seen_at, last_seen_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(session_id, platform_ref) DO UPDATE SET \
                 name           = CASE WHEN excluded.name <> '' THEN excluded.name \
                                       ELSE session_members.name END, \
                 group_nickname = COALESCE(excluded.group_nickname, session_members.group_nickname), \
                 role           = COALESCE(excluded.role, session_members.role), \
                 first_seen_at  = MIN(session_members.first_seen_at, excluded.first_seen_at), \
                 last_seen_at   = MAX(session_members.last_seen_at, excluded.last_seen_at)",
        )
        .bind(member.session_id.to_string())
        .bind(&member.platform_ref)
        .bind(&member.name)
        .bind(&member.group_nickname)
        .bind(member.role.map(|r| r.as_str()))
        .bind(member.first_seen_at)
        .bind(member.last_seen_at)
        .execute(&mut *txn)
        .await
        .storage_err("写入会话成员失败")?;
    }
    txn.commit().await.storage_err("提交会话成员事务失败")?;
    Ok(())
}

// =========================================================
// 查询
// =========================================================

/// 按会话列出成员行。
///
/// 口径:
/// - 按 `first_seen_at` 升序（首见先后），同时间按 `platform_ref` 稳定排序。
pub async fn list_by_session(
    pool: &SqlitePool,
    session_id: Uuid,
) -> RamariaResult<Vec<SessionMember>> {
    #[derive(sqlx::FromRow)]
    struct MemberRow {
        session_id: String,
        platform_ref: String,
        name: String,
        group_nickname: Option<String>,
        role: Option<String>,
        first_seen_at: i64,
        last_seen_at: i64,
    }

    let rows = sqlx::query_as::<_, MemberRow>(
        "SELECT session_id, platform_ref, name, group_nickname, role, first_seen_at, last_seen_at \
         FROM session_members WHERE session_id = ? \
         ORDER BY first_seen_at ASC, platform_ref ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .storage_err("查询会话成员失败")?;

    rows.into_iter()
        .map(|row| {
            Ok(SessionMember {
                session_id: parse_uuid_required(&row.session_id, "session_members", "session_id")?,
                platform_ref: row.platform_ref,
                name: row.name,
                group_nickname: row.group_nickname,
                role: parse_role(row.role.as_deref()),
                first_seen_at: row.first_seen_at,
                last_seen_at: row.last_seen_at,
            })
        })
        .collect()
}

/// 解析成员角色；NULL 或非法值均回 None（非法值记录 WARNING）。
fn parse_role(raw: Option<&str>) -> Option<MemberRole> {
    let value = raw?;
    match MemberRole::parse(value) {
        Some(role) => Some(role),
        None => {
            tracing::warn!(
                raw = %value,
                "session_members.role 值非法，按未知角色（None）处理"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
