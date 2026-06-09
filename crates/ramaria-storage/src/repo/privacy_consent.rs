//! rust/crates/ramaria-storage/src/repo/privacy_consent.rs - 隐私确认存取模块
//!
//! 设计特点:
//! - 按 provider + base_url 粒度记录用户的线上调用隐私确认
//! - persistent 字段控制是否跨重启持久化（勾选"下次不再提醒"）
//! - get_by_provider 取最新一条记录，按 timestamp DESC 排序
//! - provider 解析失败时保守回退为 LmStudio（本地，不需要 API key）

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{LlmProvider, PrivacyConsent};
use sqlx::SqlitePool;

/// 将存储的 provider 字符串解析为 LlmProvider 枚举。
/// 未知值回退为 LmStudio（本地，不需 API key）并记录 WARNING。
fn parse_provider(s: &str) -> LlmProvider {
    match s {
        "lm_studio" => LlmProvider::LmStudio,
        "deepseek" => LlmProvider::DeepSeek,
        "openai" => LlmProvider::OpenAI,
        other => {
            tracing::warn!(%other, "privacy_consent.provider 值非法，保守回退为 LmStudio（本地）");
            LlmProvider::LmStudio
        }
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
