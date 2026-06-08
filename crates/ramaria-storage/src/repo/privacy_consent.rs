//! rust/crates/ramaria-storage/src/repo/privacy_consent.rs - PrivacyConsent CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{LlmProvider, PrivacyConsent};
use sqlx::SqlitePool;

/// 将存储的 provider 字符串解析为 LlmProvider 枚举。
fn parse_provider(s: &str) -> LlmProvider {
    match s {
        "lm_studio" => LlmProvider::LmStudio,
        "deepseek" => LlmProvider::DeepSeek,
        "openai" => LlmProvider::OpenAI,
        _ => LlmProvider::LmStudio, // 未知值保守回退为本地
    }
}

pub async fn save(pool: &SqlitePool, consent: &PrivacyConsent) -> RamariaResult<()> {
    sqlx::query("INSERT INTO privacy_consent (provider, base_url, timestamp, persistent) VALUES (?, ?, ?, ?)")
        .bind(consent.provider.as_str()).bind(&consent.base_url)
        .bind(consent.timestamp).bind(consent.persistent as i64)
        .execute(pool).await
        .map_err(|e| RamariaError::storage_with_source("保存隐私确认失败", e))?;
    Ok(())
}

pub async fn get_by_provider(
    pool: &SqlitePool,
    provider: &str,
    base_url: &str,
) -> RamariaResult<Option<PrivacyConsent>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        provider: String,
        base_url: String,
        timestamp: i64,
        persistent: i64,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT provider, base_url, timestamp, persistent FROM privacy_consent WHERE provider = ? AND base_url = ? ORDER BY timestamp DESC LIMIT 1"
    ).bind(provider).bind(base_url).fetch_optional(pool).await
        .map_err(|e| RamariaError::storage_with_source("查询隐私确认失败", e))?;
    Ok(row.map(|r| PrivacyConsent {
        provider: parse_provider(&r.provider),
        base_url: r.base_url,
        timestamp: r.timestamp,
        persistent: r.persistent != 0,
    }))
}
