//! rust/crates/ramaria-storage/src/repo/backend_config.rs - BackendConfig CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::BackendConfig;
use sqlx::SqlitePool;

pub async fn upsert(pool: &SqlitePool, config: &BackendConfig) -> RamariaResult<()> {
    let json = serde_json::to_string(config)
        .map_err(|e| RamariaError::storage_with_source("序列化后端配置失败", e))?;
    sqlx::query("INSERT OR REPLACE INTO backend_config (id, data) VALUES (1, ?)")
        .bind(&json)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存后端配置失败", e))?;
    Ok(())
}

pub async fn get(pool: &SqlitePool) -> RamariaResult<Option<BackendConfig>> {
    let json: Option<String> = sqlx::query_scalar("SELECT data FROM backend_config WHERE id = 1")
        .fetch_optional(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询后端配置失败", e))?;
    match json {
        Some(s) => {
            let cfg: BackendConfig = serde_json::from_str(&s)
                .map_err(|e| RamariaError::storage_with_source("反序列化后端配置失败", e))?;
            Ok(Some(cfg))
        }
        None => Ok(None),
    }
}
