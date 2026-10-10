//! crates/ramaria-storage/src/repo/channel_bindings.rs - 通道绑定行存取模块
//!
//! 设计特点:
//! - 按 (channel, external_ref) upsert：同键重复写入只更新行，不重复插入
//! - 首建时间取写入时刻；更新保持 created_at、刷新 updated_at
//! - 布尔列以 0/1 读写（与既有布尔标记口径一致）
//! - 删除幂等：目标行不存在视同成功

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{ChannelBinding, now_ms};
use sqlx::SqlitePool;

/// 通道绑定数据库行（布尔列为 0/1）。
#[derive(sqlx::FromRow)]
struct BindingRow {
    id: i64,
    channel: String,
    external_ref: String,
    persona_uid: Option<String>,
    enabled: i64,
    allow_proactive: i64,
    created_at: i64,
    updated_at: i64,
}

impl BindingRow {
    fn into_binding(self) -> ChannelBinding {
        ChannelBinding {
            id: self.id,
            channel: self.channel,
            external_ref: self.external_ref,
            persona_uid: self.persona_uid,
            // SQLite 以 0/1 存储布尔列；非 0 一律读回 true
            enabled: self.enabled != 0,
            allow_proactive: self.allow_proactive != 0,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

// =========================================================
// 写入
// =========================================================

/// 写入 / 更新通道绑定（按 (channel, external_ref) upsert）。
///
/// 语义:
/// - 同键冲突时更新 `persona_uid` / `enabled` / `allow_proactive` 并刷新
///   `updated_at`；`created_at` 由首建写入后保持不变；
/// - `persona_uid` 为 None 表示"未覆盖"（不写回退值，回退在生效解析侧）。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `channel`: 通道标识（qq / telegram）。
/// - `external_ref`: 会话外部标识。
/// - `persona_uid`: 本机侧人格；None = 未覆盖。
/// - `enabled` / `allow_proactive`: 绑定开关。
///
/// 返回:
/// - 落库后的完整行（含数据库分配 id 与时间戳）。
pub async fn upsert(
    pool: &SqlitePool,
    channel: &str,
    external_ref: &str,
    persona_uid: Option<&str>,
    enabled: bool,
    allow_proactive: bool,
) -> RamariaResult<ChannelBinding> {
    let now = now_ms();
    let row = sqlx::query_as::<_, BindingRow>(
        "INSERT INTO channel_bindings \
             (channel, external_ref, persona_uid, enabled, allow_proactive, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(channel, external_ref) DO UPDATE SET \
             persona_uid     = excluded.persona_uid, \
             enabled         = excluded.enabled, \
             allow_proactive = excluded.allow_proactive, \
             updated_at      = excluded.updated_at \
         RETURNING id, channel, external_ref, persona_uid, enabled, allow_proactive, created_at, updated_at",
    )
    .bind(channel)
    .bind(external_ref)
    .bind(persona_uid)
    .bind(i64::from(enabled))
    .bind(i64::from(allow_proactive))
    .bind(now)
    .bind(now)
    .fetch_one(pool)
    .await
    .storage_err("写入通道绑定失败")?;
    Ok(row.into_binding())
}

// =========================================================
// 查询
// =========================================================

/// 按键查询通道绑定。
///
/// 返回:
/// - `Ok(Some(binding))`: 命中的绑定行。
/// - `Ok(None)`: 无绑定记录。
pub async fn find(
    pool: &SqlitePool,
    channel: &str,
    external_ref: &str,
) -> RamariaResult<Option<ChannelBinding>> {
    let row = sqlx::query_as::<_, BindingRow>(
        "SELECT id, channel, external_ref, persona_uid, enabled, allow_proactive, created_at, updated_at \
         FROM channel_bindings WHERE channel = ? AND external_ref = ?",
    )
    .bind(channel)
    .bind(external_ref)
    .fetch_optional(pool)
    .await
    .storage_err("查询通道绑定失败")?;
    Ok(row.map(BindingRow::into_binding))
}

/// 列出全部通道绑定。
///
/// 返回:
/// - 按 `channel ASC, external_ref ASC` 排序的绑定行列表。
pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<ChannelBinding>> {
    let rows = sqlx::query_as::<_, BindingRow>(
        "SELECT id, channel, external_ref, persona_uid, enabled, allow_proactive, created_at, updated_at \
         FROM channel_bindings ORDER BY channel ASC, external_ref ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("列出通道绑定失败")?;
    Ok(rows.into_iter().map(BindingRow::into_binding).collect())
}

// =========================================================
// 删除
// =========================================================

/// 删除通道绑定（幂等：不存在视同成功）。
///
/// 说明:
/// - 删除 0 行也返回成功（重复删除与未建绑定场景无需调用方分支）；
///   删除行数记 debug 日志，便于排查"删除未命中"。
pub async fn delete(pool: &SqlitePool, channel: &str, external_ref: &str) -> RamariaResult<()> {
    let result = sqlx::query("DELETE FROM channel_bindings WHERE channel = ? AND external_ref = ?")
        .bind(channel)
        .bind(external_ref)
        .execute(pool)
        .await
        .storage_err("删除通道绑定失败")?;
    // external_ref 为个人标识，仅记掩码
    tracing::debug!(
        channel,
        external_ref = %ramaria_core::privacy::mask_id(external_ref),
        rows = result.rows_affected(),
        "通道绑定删除完成"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
