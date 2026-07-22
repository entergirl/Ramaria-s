//! rust/crates/ramaria-storage/src/repo/keyword.rs - KeywordPool + keyword_refs CRUD
//!
//! 设计特点:
//! - 关键词池（keyword_pool）读写接口从裸 `String` 升级为 `KeywordToken` Newtype
//! - `upsert` 支持写入 canonical_id/alias_status，支撑别名归一化管线
//! - `keyword_refs` 倒排索引：关键词→L1/L2 文档的引用管理
//! - 所有 SQL 使用 `?` 参数绑定，杜绝注入风险
//!
//! - 激活 keyword_pool 的 canonical_id/alias_status 列（预埋）
//! - 新增 keyword_refs 表 CRUD 支持精确匹配检索

use ramaria_core::error::RamariaResult;
use ramaria_core::keyword::KeywordToken;
use sqlx::SqlitePool;

use crate::repo::StorageResultExt;

// =========================================================
// 数据库行结构体（keyword_refs）
// =========================================================

/// keyword_refs 表的行映射。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct KeywordRefRow {
    /// 自增主键
    pub id: i64,
    /// 关键词文本（引用 keyword_pool.keyword）
    pub keyword_id: String,
    /// 文档类型 ('l1' / 'l2')
    pub doc_type: String,
    /// 文档 ID（L1: UUID 字符串, L2: i64 字符串）
    pub doc_id: String,
    /// 所属 persona
    pub persona_uid: String,
    /// 关键词在此文档中的权重
    pub weight: f64,
    /// 创建时间戳（Unix 毫秒）
    pub created_at: i64,
}

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
// keyword_refs CRUD（倒排索引）
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

/// 根据关键词文本查询所有引用（倒排查询）。
///
/// 参数:
/// - `keyword_id`: 关键词文本。
///
/// 返回:
/// - 引用列表，按 created_at 降序排列（最新优先）。
pub async fn find_refs_by_keyword(
    pool: &SqlitePool,
    keyword_id: &str,
) -> RamariaResult<Vec<KeywordRefRow>> {
    let rows = sqlx::query_as::<_, KeywordRefRow>(
        "SELECT id, keyword_id, doc_type, doc_id, persona_uid, weight, created_at
         FROM keyword_refs
         WHERE keyword_id = ?
         ORDER BY created_at DESC, id DESC",
    )
    .bind(keyword_id)
    .fetch_all(pool)
    .await
    .storage_err("查询关键词引用失败")?;
    Ok(rows)
}

/// 根据文档信息查询所有引用（正排查询——找某个文档包含的所有关键词）。
///
/// 参数:
/// - `doc_type`: 文档类型（"l1" / "l2"）。
/// - `doc_id`: 文档 ID。
///
/// 返回:
/// - 引用列表，按 weight DESC 排列（高权重优先）。
pub async fn find_refs_by_doc(
    pool: &SqlitePool,
    doc_type: &str,
    doc_id: &str,
) -> RamariaResult<Vec<KeywordRefRow>> {
    let rows = sqlx::query_as::<_, KeywordRefRow>(
        "SELECT id, keyword_id, doc_type, doc_id, persona_uid, weight, created_at
         FROM keyword_refs
         WHERE doc_type = ? AND doc_id = ?
         ORDER BY weight DESC",
    )
    .bind(doc_type)
    .bind(doc_id)
    .fetch_all(pool)
    .await
    .storage_err("查询文档关键词引用失败")?;
    Ok(rows)
}

/// 删除指定文档的所有关键词引用（用于重新索引时清理）。
///
/// 参数:
/// - `doc_type`: 文档类型。
/// - `doc_id`: 文档 ID。
///
/// 返回:
/// - 删除的记录数。
pub async fn delete_refs_by_doc(
    pool: &SqlitePool,
    doc_type: &str,
    doc_id: &str,
) -> RamariaResult<u64> {
    let result = sqlx::query("DELETE FROM keyword_refs WHERE doc_type = ? AND doc_id = ?")
        .bind(doc_type)
        .bind(doc_id)
        .execute(pool)
        .await
        .storage_err("删除文档关键词引用失败")?;
    Ok(result.rows_affected())
}

/// 统计关键词引用总数（用于调试和监控）。
pub async fn count_all(pool: &SqlitePool) -> RamariaResult<i64> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM keyword_refs")
        .fetch_one(pool)
        .await
        .storage_err("统计关键词引用数失败")?;
    Ok(count.0)
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
        let pool = database::init_test_pool()
            .await
            .expect("创建测试数据库失败");
        pool
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

    // ── keyword_refs CRUD ──

    #[tokio::test]
    async fn test_keyword_ref_insert_and_find_by_keyword() {
        let pool = setup().await;

        // 先插入关键词
        let kw = KeywordToken::new("工作").unwrap();
        upsert(&pool, &kw).await.unwrap();

        // 插入两条引用
        insert_ref(&pool, "工作", "l1", "100", "persona_1", 1.0)
            .await
            .unwrap();
        insert_ref(&pool, "工作", "l2", "200", "persona_1", 0.8)
            .await
            .unwrap();

        // 按关键词查询
        let refs = find_refs_by_keyword(&pool, "工作").await.unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].doc_id, "200"); // created_at DESC, 后插入的在前
        assert_eq!(refs[1].doc_id, "100");
    }

    #[tokio::test]
    async fn test_keyword_ref_find_by_doc() {
        let pool = setup().await;

        upsert(&pool, &KeywordToken::new("压力").unwrap())
            .await
            .unwrap();
        upsert(&pool, &KeywordToken::new("焦虑").unwrap())
            .await
            .unwrap();

        insert_ref(&pool, "压力", "l1", "50", "p1", 1.0)
            .await
            .unwrap();
        insert_ref(&pool, "焦虑", "l1", "50", "p1", 0.7)
            .await
            .unwrap();
        insert_ref(&pool, "压力", "l2", "99", "p2", 1.0)
            .await
            .unwrap();

        // 查 L1 文档 50 的关键词
        let refs = find_refs_by_doc(&pool, "l1", "50").await.unwrap();
        assert_eq!(refs.len(), 2);
        // weight DESC: 压力(1.0) 先于 焦虑(0.7)
        assert_eq!(refs[0].keyword_id, "压力");
        assert_eq!(refs[1].keyword_id, "焦虑");
    }

    #[tokio::test]
    async fn test_keyword_ref_delete_by_doc() {
        let pool = setup().await;

        upsert(&pool, &KeywordToken::new("a").unwrap())
            .await
            .unwrap();
        upsert(&pool, &KeywordToken::new("b").unwrap())
            .await
            .unwrap();

        insert_ref(&pool, "a", "l1", "1", "p", 1.0).await.unwrap();
        insert_ref(&pool, "b", "l1", "1", "p", 1.0).await.unwrap();
        insert_ref(&pool, "a", "l2", "2", "p", 1.0).await.unwrap();

        let deleted = delete_refs_by_doc(&pool, "l1", "1").await.unwrap();
        assert_eq!(deleted, 2);

        let remaining = count_all(&pool).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn test_keyword_ref_count_empty() {
        let pool = setup().await;
        let count = count_all(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}
