//! rust/crates/ramaria-storage/src/repo/backend_config.rs - 后端配置存储
//!
//! 设计特点:
//! - BackendConfig 以 JSON 方式存入单行表（id = 1）
//! - 不存储 API key，只存非敏感配置
//! - 使用 INSERT OR REPLACE 保证始终只有一行

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::BackendConfig;
use sqlx::Row;
use sqlx::SqlitePool;

/// 保存后端配置（单行覆盖）。
pub async fn save_backend_config(pool: &SqlitePool, config: &BackendConfig) -> RamariaResult<()> {
    let json = serde_json::to_string(config)
        .map_err(|e| RamariaError::storage_with_source("序列化 BackendConfig 失败", e))?;

    sqlx::query("INSERT OR REPLACE INTO backend_config (id, config_json) VALUES (1, ?)")
        .bind(&json)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存后端配置失败", e))?;
    Ok(())
}

/// 获取当前后端配置。
pub async fn get_backend_config(pool: &SqlitePool) -> RamariaResult<Option<BackendConfig>> {
    let row = sqlx::query("SELECT config_json FROM backend_config WHERE id = 1")
        .fetch_optional(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询后端配置失败", e))?;

    match row {
        Some(r) => {
            let json: String = r.get("config_json");
            let config: BackendConfig = serde_json::from_str(&json)
                .map_err(|e| RamariaError::storage_with_source("反序列化 BackendConfig 失败", e))?;
            Ok(Some(config))
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
    async fn save_and_get_backend_config() {
        let pool = test_pool().await.unwrap();

        let config = BackendConfig::deepseek_default();
        save_backend_config(&pool, &config).await.unwrap();

        let fetched = get_backend_config(&pool).await.unwrap();
        assert!(fetched.is_some());
        let c = fetched.unwrap();
        assert_eq!(c.provider, ramaria_core::types::LlmProvider::DeepSeek);
        assert_eq!(c.model_id, "deepseek-chat");
    }

    #[tokio::test]
    async fn empty_config_returns_none() {
        let pool = test_pool().await.unwrap();
        let result = get_backend_config(&pool).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn replace_config_overwrites() {
        let pool = test_pool().await.unwrap();

        save_backend_config(&pool, &BackendConfig::lm_studio_default())
            .await
            .unwrap();
        save_backend_config(&pool, &BackendConfig::openai_default())
            .await
            .unwrap();

        let fetched = get_backend_config(&pool).await.unwrap().unwrap();
        assert_eq!(fetched.provider, ramaria_core::types::LlmProvider::OpenAI);
    }
}
