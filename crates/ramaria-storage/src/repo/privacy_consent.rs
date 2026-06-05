//! rust/crates/ramaria-storage/src/repo/privacy_consent.rs - 隐私确认 CRUD
//!
//! 设计特点:
//! - provider + base_url 作为复合主键
//! - 支持持久化确认（persistent）和临时确认
//! - 保存时使用 INSERT OR REPLACE 语义

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{LlmProvider, PrivacyConsent};
use sqlx::Row;
use sqlx::SqlitePool;

/// 保存隐私确认记录（INSERT OR REPLACE）。
pub async fn save_privacy_consent(
    pool: &SqlitePool,
    consent: &PrivacyConsent,
) -> RamariaResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO privacy_consent (provider, base_url, timestamp, persistent) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(consent.provider.as_str())
    .bind(&consent.base_url)
    .bind(consent.timestamp)
    .bind(consent.persistent as i32)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存隐私确认失败", e))?;
    Ok(())
}

/// 按 provider + base_url 查询确认记录。
pub async fn get_privacy_consent(
    pool: &SqlitePool,
    provider: &str,
    base_url: &str,
) -> RamariaResult<Option<PrivacyConsent>> {
    let row = sqlx::query(
        "SELECT provider, base_url, timestamp, persistent \
         FROM privacy_consent WHERE provider = ? AND base_url = ?",
    )
    .bind(provider)
    .bind(base_url)
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询隐私确认失败", e))?;

    match row {
        Some(r) => {
            let p_str: String = r.get("provider");
            let persistent_int: i32 = r.get("persistent");

            let provider = match p_str.as_str() {
                "lm_studio" => LlmProvider::LmStudio,
                "deepseek" => LlmProvider::DeepSeek,
                "openai" => LlmProvider::OpenAI,
                other => {
                    return Err(RamariaError::storage(format!("未知 provider: {other}")));
                }
            };

            Ok(Some(PrivacyConsent {
                provider,
                base_url: r.get("base_url"),
                timestamp: r.get("timestamp"),
                persistent: persistent_int != 0,
            }))
        }
        None => Ok(None),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;

    #[tokio::test]
    async fn save_and_get_consent() {
        let pool = test_pool().await.unwrap();

        let consent = PrivacyConsent::new(
            LlmProvider::DeepSeek,
            "https://api.deepseek.com/v1".into(),
            true,
        );

        save_privacy_consent(&pool, &consent).await.unwrap();

        let found = get_privacy_consent(&pool, "deepseek", "https://api.deepseek.com/v1")
            .await
            .unwrap();
        assert!(found.is_some());
        let c = found.unwrap();
        assert_eq!(c.provider, LlmProvider::DeepSeek);
        assert!(c.persistent);
    }

    #[tokio::test]
    async fn consent_not_found() {
        let pool = test_pool().await.unwrap();

        let result = get_privacy_consent(&pool, "openai", "https://api.openai.com/v1")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn replace_existing_consent() {
        let pool = test_pool().await.unwrap();

        let c1 = PrivacyConsent::new(
            LlmProvider::OpenAI,
            "https://api.openai.com/v1".into(),
            false,
        );
        save_privacy_consent(&pool, &c1).await.unwrap();

        // 更新为持久化
        let c2 = PrivacyConsent::new(
            LlmProvider::OpenAI,
            "https://api.openai.com/v1".into(),
            true,
        );
        save_privacy_consent(&pool, &c2).await.unwrap();

        let found = get_privacy_consent(&pool, "openai", "https://api.openai.com/v1")
            .await
            .unwrap();
        assert!(found.unwrap().persistent);
    }
}
