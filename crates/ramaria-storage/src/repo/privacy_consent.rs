//! rust/crates/ramaria-storage/src/repo/privacy_consent.rs - 隐私确认存取模块
//!
//! 设计特点:
//! - 按 provider + base_url 粒度记录用户的线上调用隐私确认
//! - persistent 字段控制是否跨重启持久化（勾选"下次不再提醒"）
//! - get_by_provider 取最新一条记录，按 timestamp DESC 排序
//! - provider 存储时使用 `LlmProvider::as_str`，读取时解析回枚举
//! - 非法 provider 值 → `RamariaError::Validation`（DeserializationError），不再静默回退

use crate::repo::StorageResultExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{LlmProvider, PrivacyConsent};
use sqlx::SqlitePool;

/// 将存储的 provider 字符串解析为 LlmProvider 枚举。
///
/// 合法值: `"lm_studio"`, `"deepseek"`, `"openai"`
///
/// 非法值 → `RamariaError::Validation`，携带原始字符串以供排查。
/// 不再静默回退为 LmStudio，因为数据损坏需要明确的上层处理。
fn parse_provider(s: &str) -> RamariaResult<LlmProvider> {
    match s {
        "lm_studio" => Ok(LlmProvider::LmStudio),
        "deepseek" => Ok(LlmProvider::DeepSeek),
        "openai" => Ok(LlmProvider::OpenAI),
        other => {
            tracing::error!(%other, "privacy_consent.provider 值非法，数据库可能存在数据损坏");
            Err(RamariaError::validation(format!(
                "privacy_consent.provider 值非法: '{other}'，合法值仅限 lm_studio/deepseek/openai"
            )))
        }
    }
}

pub async fn save(pool: &SqlitePool, consent: &PrivacyConsent) -> RamariaResult<()> {
    sqlx::query("INSERT INTO privacy_consent (provider, base_url, timestamp, persistent) VALUES (?, ?, ?, ?)")
        .bind(consent.provider.as_str()).bind(&consent.base_url)
        .bind(consent.timestamp).bind(consent.persistent as i64)
        .execute(pool).await
        .storage_err("保存隐私确认失败")?;
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
        .storage_err("查询隐私确认失败")?;
    match row {
        Some(r) => {
            let parsed_provider = parse_provider(&r.provider)?;
            Ok(Some(PrivacyConsent {
                provider: parsed_provider,
                base_url: r.base_url,
                timestamp: r.timestamp,
                persistent: r.persistent != 0,
            }))
        }
        None => Ok(None),
    }
}
