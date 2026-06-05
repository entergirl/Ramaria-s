//! rust/crates/ramaria-storage/src/repo/user_profile.rs - L3 用户画像 CRUD
//!
//! 设计特点:
//! - 画像按 field + is_current 管理版本
//! - mark_profile_historical 将旧版本标记为非 current
//! - get_current_profile 返回所有 is_current = 1 的条目

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{ProfileField, ProfileStatus, UserProfile};
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 保存画像条目。
pub async fn save_user_profile(pool: &SqlitePool, profile: &UserProfile) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO user_profile (id, field, content, source_l1_id, status, is_current, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(profile.id.to_string())
    .bind(profile.field.as_str())
    .bind(&profile.content)
    .bind(profile.source_l1_id.map(|id| id.to_string()))
    .bind(profile.status.as_str())
    .bind(profile.is_current as i32)
    .bind(profile.updated_at)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存画像失败", e))?;
    Ok(())
}

/// 获取当前生效的画像（is_current = true）。
pub async fn get_current_profile(pool: &SqlitePool) -> RamariaResult<Vec<UserProfile>> {
    let rows = sqlx::query(
        "SELECT id, field, content, source_l1_id, status, is_current, updated_at \
         FROM user_profile WHERE is_current = 1 ORDER BY field",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询当前画像失败", e))?;

    rows.iter().map(row_to_user_profile).collect()
}

/// 将指定 field 的旧版本标记为非 current。
pub async fn mark_profile_historical(pool: &SqlitePool, field: &str) -> RamariaResult<()> {
    sqlx::query("UPDATE user_profile SET is_current = 0 WHERE field = ? AND is_current = 1")
        .bind(field)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("标记画像为非 current 失败", e))?;
    Ok(())
}

// =========================================================
// 行映射
// =========================================================

fn row_to_user_profile(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<UserProfile> {
    let id_str: String = row.get("id");
    let field_str: String = row.get("field");
    let status_str: String = row.get("status");
    let is_current_int: i32 = row.get("is_current");
    let source_l1_id_str: Option<String> = row.get("source_l1_id");

    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("画像 ID 格式非法", e))?;

    let field = match field_str.as_str() {
        "basic_info" => ProfileField::BasicInfo,
        "personal_status" => ProfileField::PersonalStatus,
        "interests" => ProfileField::Interests,
        "social" => ProfileField::Social,
        "history" => ProfileField::History,
        "recent_context" => ProfileField::RecentContext,
        other => {
            return Err(RamariaError::storage(format!("未知画像字段: {other}")));
        }
    };

    let status = match status_str.as_str() {
        "approved" => ProfileStatus::Approved,
        "pending" => ProfileStatus::Pending,
        "rejected" => ProfileStatus::Rejected,
        other => {
            return Err(RamariaError::storage(format!("未知画像状态: {other}")));
        }
    };

    let source_l1_id = source_l1_id_str
        .map(|s| {
            Uuid::parse_str(&s)
                .map_err(|e| RamariaError::storage_with_source("画像 source_l1_id 格式非法", e))
        })
        .transpose()?;

    Ok(UserProfile {
        id,
        field,
        content: row.get("content"),
        source_l1_id,
        status,
        is_current: is_current_int != 0,
        updated_at: row.get("updated_at"),
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;

    #[tokio::test]
    async fn save_and_get_current_profile() {
        let pool = test_pool().await.unwrap();

        let mut p1 = UserProfile::new(ProfileField::BasicInfo, "姓名：测试用户".into(), None);
        p1.mark_current();
        save_user_profile(&pool, &p1).await.unwrap();

        let mut p2 = UserProfile::new(ProfileField::Interests, "兴趣：编程".into(), None);
        p2.mark_current();
        save_user_profile(&pool, &p2).await.unwrap();

        let current = get_current_profile(&pool).await.unwrap();
        assert_eq!(current.len(), 2);
    }

    #[tokio::test]
    async fn mark_historical_and_new_version() {
        let pool = test_pool().await.unwrap();

        // 创建旧版本
        let mut old = UserProfile::new(ProfileField::BasicInfo, "姓名：旧名".into(), None);
        old.mark_current();
        save_user_profile(&pool, &old).await.unwrap();

        // 标记为 historical
        mark_profile_historical(&pool, "basic_info").await.unwrap();

        // 创建新版本
        let mut new_p = UserProfile::new(ProfileField::BasicInfo, "姓名：新名".into(), None);
        new_p.mark_current();
        save_user_profile(&pool, &new_p).await.unwrap();

        // 当前生效的只有新版本
        let current = get_current_profile(&pool).await.unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].content, "姓名：新名");
    }
}
