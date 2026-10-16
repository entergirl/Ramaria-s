//! crates/ramaria-storage/src/repo/sticker_assets.rs - 表情资产行存取模块
//!
//! 设计特点:
//! - 表情资产表读写单一落点：插入 / 全量列表 / 单行读取 / 部分字段更新 / 删除
//! - tags 以 JSON 文本数组落库，读取解析失败降级为空列表并记录 WARNING
//! - 更新对 description / tags 支持 None 保持现值（条件覆盖），空串清空描述（存 NULL），
//!   命中时刷新 updated_at

use crate::repo::StorageResultExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::StickerAsset;
use sqlx::SqlitePool;

/// 查询 SELECT 列（与行结构字段一一对应）。
const COLUMNS: &str =
    "id, file_name, file_path, md5, size, description, tags, created_at, updated_at";

#[derive(sqlx::FromRow)]
struct StickerAssetRow {
    id: i64,
    file_name: String,
    file_path: String,
    md5: Option<String>,
    size: Option<i64>,
    description: Option<String>,
    tags: String,
    created_at: i64,
    updated_at: i64,
}

impl StickerAssetRow {
    fn into_asset(self) -> StickerAsset {
        StickerAsset {
            id: self.id,
            file_name: self.file_name,
            file_path: self.file_path,
            md5: self.md5,
            size: self.size.map(|v| v.max(0) as u64),
            description: self.description,
            tags: parse_tags(&self.tags),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

// =========================================================
// 标签序列化
// =========================================================

/// 解析 tags JSON 文本；解析失败或非字符串数组时记录 WARNING 并回退空列表。
fn parse_tags(raw: &str) -> Vec<String> {
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(tags) => tags,
        Err(_) => {
            tracing::warn!(raw, "sticker_assets.tags 解析失败，回退为空列表");
            Vec::new()
        }
    }
}

/// 序列化标签列表为 JSON 文本。
fn serialize_tags(tags: &[String]) -> RamariaResult<String> {
    serde_json::to_string(tags)
        .map_err(|e| RamariaError::storage(format!("表情资产标签序列化失败: {e}")))
}
// =========================================================
// 写入
// =========================================================

/// 插入一条表情资产。
///
/// 口径:
/// - id 由数据库自增分配，结构内 id 不参与写入；
/// - tags 序列化为 JSON 文本数组落库；
/// - created_at / updated_at 取结构内字段值（由调用方决定）。
///
/// 返回:
/// - 新行的自增 id。
pub async fn insert(pool: &SqlitePool, asset: &StickerAsset) -> RamariaResult<i64> {
    let tags_json = serialize_tags(&asset.tags)?;
    let result = sqlx::query(
        "INSERT INTO sticker_assets \
             (file_name, file_path, md5, size, description, tags, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&asset.file_name)
    .bind(&asset.file_path)
    .bind(&asset.md5)
    .bind(asset.size.map(|v| v as i64))
    .bind(&asset.description)
    .bind(tags_json)
    .bind(asset.created_at)
    .bind(asset.updated_at)
    .execute(pool)
    .await
    .storage_err("写入表情资产失败")?;
    Ok(result.last_insert_rowid())
}

/// 更新表情资产的描述与标签。
///
/// 口径:
/// - `description` 为 Some 时覆盖描述（空串清空为 NULL），None 保持现值；
/// - `tags` 为 Some 时整体覆盖标签，None 保持现值；
/// - 命中时刷新 updated_at。
///
/// 返回:
/// - `Ok(true)`: 命中并更新；`Ok(false)`: 未命中。
pub async fn update(
    pool: &SqlitePool,
    id: i64,
    description: Option<&str>,
    tags: Option<&[String]>,
) -> RamariaResult<bool> {
    let tags_json = match tags {
        Some(list) => Some(serialize_tags(list)?),
        None => None,
    };
    let result = sqlx::query(
        "UPDATE sticker_assets \
         SET description = CASE WHEN ?1 IS NULL THEN description ELSE NULLIF(?1, '') END, \
             tags = COALESCE(?2, tags), \
             updated_at = ?3 \
         WHERE id = ?4",
    )
    .bind(description)
    .bind(tags_json)
    .bind(ramaria_core::types::now_ms())
    .bind(id)
    .execute(pool)
    .await
    .storage_err("更新表情资产失败")?;
    Ok(result.rows_affected() > 0)
}

/// 按 id 删除表情资产。
///
/// 返回:
/// - `Ok(true)`: 命中并删除；`Ok(false)`: 未命中。
pub async fn delete(pool: &SqlitePool, id: i64) -> RamariaResult<bool> {
    let result = sqlx::query("DELETE FROM sticker_assets WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .storage_err("删除表情资产失败")?;
    Ok(result.rows_affected() > 0)
}
// =========================================================
// 查询
// =========================================================

/// 列出全部表情资产（按 id 升序）。
pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<StickerAsset>> {
    let rows = sqlx::query_as::<_, StickerAssetRow>(&format!(
        "SELECT {COLUMNS} FROM sticker_assets ORDER BY id ASC"
    ))
    .fetch_all(pool)
    .await
    .storage_err("查询表情资产列表失败")?;
    Ok(rows.into_iter().map(|r| r.into_asset()).collect())
}

/// 按 id 查询单条表情资产。
///
/// 返回:
/// - 命中时返回资产；未命中返回 `Ok(None)`。
pub async fn get(pool: &SqlitePool, id: i64) -> RamariaResult<Option<StickerAsset>> {
    let row = sqlx::query_as::<_, StickerAssetRow>(&format!(
        "SELECT {COLUMNS} FROM sticker_assets WHERE id = ? LIMIT 1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .storage_err("按 id 查询表情资产失败")?;
    Ok(row.map(|r| r.into_asset()))
}

#[cfg(test)]
mod tests;
