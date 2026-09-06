//! crates/ramaria-storage/src/repo/keyword.rs - KeywordPool + keyword_refs 写入
//!
//! 设计特点:
//! - 关键词池（keyword_pool）读写接口从裸 `String` 升级为 `KeywordToken` Newtype
//! - `upsert` 支持写入 canonical_id/alias_status，支撑别名归一化管线
//! - `keyword_refs` 倒排索引仅保留写入（insert_ref）；精确匹配消费路径留 M3
//! - 所有 SQL 使用 `?` 参数绑定，杜绝注入风险
//!
//! - 激活 keyword_pool 的 canonical_id/alias_status 列（预埋）

use ramaria_core::error::RamariaResult;
use ramaria_core::keyword::KeywordToken;
use sqlx::SqlitePool;

use crate::repo::StorageResultExt;

// =========================================================
// keyword_pool CRUD
// =========================================================

/// 插入或更新关键词（使用计数递增）。
///
/// 参数:
/// - `keyword`: 标准化后的关键词（KeywordToken Newtype）。
///
/// 说明:
/// - 冲突时递增 `use_count` + 更新 `last_used_at`。
/// - `canonical_id` 和 `alias_status` 默认为 NULL（Canonical 状态）。
pub async fn upsert(pool: &SqlitePool, keyword: &KeywordToken) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at)
         VALUES (?, 1, ?, ?)
         ON CONFLICT(keyword) DO UPDATE SET
             use_count = use_count + 1,
             last_used_at = ?",
    )
    .bind(keyword.as_str())
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .storage_err("upsert 关键词失败")?;
    Ok(())
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 插入或更新关键词，并写入别名状态。
///
/// 参数:
/// - `keyword`: 标准化后的关键词。
/// - `canonical_id`: 规范词在 keyword_pool 中的 id（自身为规范词时填 0 或 NULL）。
/// - `alias_status`: 别名状态标识（"canonical" / "alias" / "pending"）。
///
/// 说明:
/// - 用于别名系统：注册别名时调用此方法写入 canonical_id 和 alias_status。
/// - `canonical_id` 为 0 时写入 NULL（自身为规范词）。
pub async fn upsert_with_alias(
    pool: &SqlitePool,
    keyword: &KeywordToken,
    canonical_id: i64,
    alias_status: &str,
) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    let cid: Option<i64> = if canonical_id == 0 {
        None
    } else {
        Some(canonical_id)
    };

    sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at, canonical_id, alias_status)
         VALUES (?, 1, ?, ?, ?, ?)
         ON CONFLICT(keyword) DO UPDATE SET
             use_count = use_count + 1,
             last_used_at = ?,
             canonical_id = COALESCE(?, canonical_id),
             alias_status = COALESCE(?, alias_status)"
    )
    .bind(keyword.as_str())
    .bind(now)
    .bind(now)
    .bind(cid)
    .bind(alias_status)
    .bind(now)
    .bind(cid)
    .bind(alias_status)
    .execute(pool)
    .await
    .storage_err("upsert 关键词（含别名）失败")?;
    Ok(())
}

/// 查询所有关键词，按使用频率降序排列。
///
/// 返回:
/// - `Vec<KeywordToken>`: 去重后的标准化关键词列表。
pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<KeywordToken>> {
    let rows =
        sqlx::query_scalar::<_, String>("SELECT keyword FROM keyword_pool ORDER BY use_count DESC")
            .fetch_all(pool)
            .await
            .storage_err("查询关键词列表失败")?;

    // 过滤无效条目（理论上不应存在，但防御处理）
    let tokens: Vec<KeywordToken> = rows
        .into_iter()
        .filter_map(|s| KeywordToken::new(&s))
        .collect();
    Ok(tokens)
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 根据规范词 ID 查找所有别名。
///
/// 参数:
/// - `canonical_id`: 规范词在 keyword_pool 中的 id。
///
/// 返回:
/// - `Vec<String>`: 别名词文本列表。
///
/// 说明:
/// - 仅返回 alias_status='alias' 的条目。
pub async fn list_aliases(pool: &SqlitePool, canonical_id: i64) -> RamariaResult<Vec<String>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT keyword FROM keyword_pool
         WHERE canonical_id = ? AND alias_status = 'alias'
         ORDER BY use_count DESC",
    )
    .bind(canonical_id)
    .fetch_all(pool)
    .await
    .storage_err("查询别名列表失败")?;
    Ok(rows)
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 根据关键词文本查询其 canonical_id。
///
/// 返回:
/// - `Option<i64>`: 规范词 ID（自身为规范词时返回自己的 ID，无法识别时返回 None）。
///
/// 说明:
/// - 返回自身 ID 的逻辑：如果关键词 alias_status='canonical' 或无 alias_status，
///   但其 canonical_id 不为 NULL，则返回 canonical_id。
/// - 简化版本：直接返回 canonical_id 列（NULL 表示无法识别）。
pub async fn find_canonical_id(
    pool: &SqlitePool,
    keyword: &KeywordToken,
) -> RamariaResult<Option<i64>> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT canonical_id FROM keyword_pool WHERE keyword = ?")
            .bind(keyword.as_str())
            .fetch_optional(pool)
            .await
            .storage_err("查询规范词 ID 失败")?;

    Ok(row.and_then(|r| r.0))
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 更新关键词的别名状态。
///
/// 参数:
/// - `keyword`: 目标关键词。
/// - `canonical_id`: 新规范词 ID（设为 0 表示清除）。
/// - `alias_status`: 新状态（"canonical" / "alias" / "pending"）。
pub async fn update_alias_status(
    pool: &SqlitePool,
    keyword: &KeywordToken,
    canonical_id: i64,
    alias_status: &str,
) -> RamariaResult<()> {
    let cid: Option<i64> = if canonical_id == 0 {
        None
    } else {
        Some(canonical_id)
    };
    sqlx::query("UPDATE keyword_pool SET canonical_id = ?, alias_status = ? WHERE keyword = ?")
        .bind(cid)
        .bind(alias_status)
        .bind(keyword.as_str())
        .execute(pool)
        .await
        .storage_err("更新关键词别名状态失败")?;
    Ok(())
}

/// 查询所有 keyword_pool 条目的使用量映射（文本 → 使用次数）。
///
/// 返回:
/// - `Vec<(String, u32)>`: (关键词文本, use_count) 列表，按 use_count DESC 排序。
///
/// 说明:
/// - 供 AliasManager::load_use_counts() 批量加载使用。
/// - 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）。
pub async fn list_all_with_counts(pool: &SqlitePool) -> RamariaResult<Vec<(String, u32)>> {
    let rows = sqlx::query_as::<_, (String, u32)>(
        "SELECT keyword, use_count FROM keyword_pool ORDER BY use_count DESC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询关键词使用量失败")?;
    Ok(rows)
}

// =========================================================
// keyword_refs 写入（倒排索引；精确匹配消费路径留 M3）
// =========================================================

/// 插入一条关键词引用记录。
///
/// 参数:
/// - `keyword_id`: 关键词文本（引用 keyword_pool.keyword）。
/// - `doc_type`: 文档类型（"l1" / "l2"）。
/// - `doc_id`: 文档 ID。
/// - `persona_uid`: 所属人格 UID。
/// - `weight`: 关键词在此文档中的权重（默认 1.0）。
///
/// 说明:
/// - 不检查重复——同一关键词在同一文档中出现多次时分别记录（权重可不同）。
/// - 由调用方确保 `keyword_id` 在 keyword_pool 中已存在。
pub async fn insert_ref(
    pool: &SqlitePool,
    keyword_id: &str,
    doc_type: &str,
    doc_id: &str,
    persona_uid: &str,
    weight: f64,
) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query(
        "INSERT INTO keyword_refs (keyword_id, doc_type, doc_id, persona_uid, weight, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(keyword_id)
    .bind(doc_type)
    .bind(doc_id)
    .bind(persona_uid)
    .bind(weight)
    .bind(now)
    .execute(pool)
    .await
    .storage_err("插入关键词引用失败")?;
    Ok(())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;
    use ramaria_core::keyword::KeywordToken;

    /// 创建测试数据库连接池（内存 SQLite，自动运行 migration）
    async fn setup() -> SqlitePool {
        database::init_test_pool()
            .await
            .expect("创建测试数据库失败")
    }

    // ── keyword_pool CRUD ──

    #[tokio::test]
    async fn test_upsert_and_list() {
        let pool = setup().await;

        let kw = KeywordToken::new("工作压力").unwrap();
        upsert(&pool, &kw).await.unwrap();
        upsert(&pool, &kw).await.unwrap(); // 重复 upsert 递增 use_count

        let list = list_all(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].as_str(), "工作压力");
    }

    #[tokio::test]
    async fn test_upsert_with_alias() {
        let pool = setup().await;

        let canonical = KeywordToken::new("工作压力").unwrap();
        let alias = KeywordToken::new("职场焦虑").unwrap();

        // 注册规范词
        upsert_with_alias(&pool, &canonical, 0, "canonical")
            .await
            .unwrap();
        // 注册别名（假设规范词 ID 为 1——自增主键从 1 开始）
        upsert_with_alias(&pool, &alias, 1, "alias").await.unwrap();

        // 查询别名列表
        let aliases = list_aliases(&pool, 1).await.unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0], "职场焦虑");
    }

    #[tokio::test]
    async fn test_find_canonical_id() {
        let pool = setup().await;

        let kw = KeywordToken::new("压力").unwrap();
        upsert_with_alias(&pool, &kw, 0, "canonical").await.unwrap();

        let cid = find_canonical_id(&pool, &kw).await.unwrap();
        // canonical_id=0 被转为 None 写入 DB，读回应为 None
        assert!(cid.is_none());
    }

    #[tokio::test]
    async fn test_update_alias_status() {
        let pool = setup().await;

        let kw = KeywordToken::new("压力").unwrap();
        upsert(&pool, &kw).await.unwrap();

        // 更新为别名状态
        update_alias_status(&pool, &kw, 1, "alias").await.unwrap();

        let cid = find_canonical_id(&pool, &kw).await.unwrap();
        assert_eq!(cid, Some(1));
    }

    #[tokio::test]
    async fn test_list_all_with_counts() {
        let pool = setup().await;

        upsert(&pool, &KeywordToken::new("A").unwrap())
            .await
            .unwrap();
        upsert(&pool, &KeywordToken::new("A").unwrap())
            .await
            .unwrap();
        upsert(&pool, &KeywordToken::new("B").unwrap())
            .await
            .unwrap();

        let counts = list_all_with_counts(&pool).await.unwrap();
        // A 使用 2 次，B 使用 1 次，按 use_count DESC 排列
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0], ("a".to_string(), 2));
        assert_eq!(counts[1], ("b".to_string(), 1));
    }
}
