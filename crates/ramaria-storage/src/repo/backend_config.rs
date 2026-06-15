//! rust/crates/ramaria-storage/src/repo/backend_config.rs - 后端配置存取模块
//!
//! 设计特点:
//! - 单行存储（id=1），将整个 BackendConfig 序列化为 JSON 存入 data 列
//! - upsert 使用 INSERT OR REPLACE 确保幂等写入
//! - 不存储 API key（密钥由 OS keychain 管理）
//! - get 返回 Option<BackendConfig>，未配置时返回 None 供上层决策

use crate::repo::StorageResultExt;
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
        .storage_err("保存后端配置失败")?;
    Ok(())
}

pub async fn get(pool: &SqlitePool) -> RamariaResult<Option<BackendConfig>> {
    let json: Option<String> = sqlx::query_scalar("SELECT data FROM backend_config WHERE id = 1")
        .fetch_optional(pool)
        .await
        .storage_err("查询后端配置失败")?;
    match json {
        Some(s) => {
            let cfg: BackendConfig = serde_json::from_str(&s)
                .map_err(|e| RamariaError::storage_with_source("反序列化后端配置失败", e))?;
            Ok(Some(cfg))
        }
        None => Ok(None),
    }
}
