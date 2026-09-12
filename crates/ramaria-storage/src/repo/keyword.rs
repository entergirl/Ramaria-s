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
use ramaria_core::keyword::{KeywordPoolRow, KeywordToken};
use sqlx::SqlitePool;

use crate::repo::StorageResultExt;

// =========================================================
// 别名归一化行结构（keyword-design §6.2）
// =========================================================

/// 规范词行（keyword_pool 中 canonical_id IS NULL 的词条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRow {
    /// keyword_pool.rowid（INTEGER 自增 rowid）
    pub rowid: i64,
    /// 规范词文本
    pub keyword: String,
    /// 使用次数
    pub use_count: i64,
}

/// 待确认别名冲突行（alias_status='pending'，join 出规范词文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAliasRow {
    /// 别名词条 rowid
    pub alias_id: i64,
    /// 别名文本
    pub alias_keyword: String,
    /// 建议合并到的规范词 rowid
    pub canonical_id: i64,
    /// 规范词文本
    pub canonical_keyword: String,
    /// 登记时间（Unix 毫秒）
    pub created_at: i64,
}

/// keyword_pool 全字段行视图（M3 CLI list/show 与词典装载共用）。
///
/// 说明:
/// - 供 `ramaria keyword list/show` 展示词条状态，以及词典增强分词装载
///   （keyword_pool 规范词 → 分词词典）复用同一行视图，避免重复 SQL。
/// - `alias_status` 语义: `NULL`/`"canonical"` → 规范词；`"alias"` → 已确认别名；
///   `"pending"` → 待确认别名。规范词同时满足 `canonical_id IS NULL`。
/// - `canonical_keyword` 由 LEFT JOIN 解析：alias/pending 词条携带其指向的
///   规范词文本；规范词自身为 `None`（无需二次查库即可展示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordEntryRow {
    /// 关键词文本（标准化后）
    pub keyword: String,
    /// 使用次数（每次自然出现 +1；手工种子为 0）
    pub use_count: i64,
    /// 别名状态文本（"canonical" / "alias" / "pending"，规范词可为 NULL）
    pub alias_status: Option<String>,
    /// 指向的规范词行 id（别名/待确认词条有值；规范词自身为 NULL）
    pub canonical_id: Option<i64>,
    /// 登记时间（Unix 毫秒）
    pub created_at: i64,
    /// 指向规范词的文本（LEFT JOIN 解析；规范词自身为 None）
    pub canonical_keyword: Option<String>,
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

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 列出全部规范词（canonical_id IS NULL 的词条，含 alias_status NULL 与 'canonical' 两种旧形态）。
///
/// 返回:
/// - `Vec<CanonicalRow>`，按 use_count 降序、keyword 升序（输出稳定）。
pub async fn list_canonicals(pool: &SqlitePool) -> RamariaResult<Vec<CanonicalRow>> {
    let rows = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT rowid, keyword, use_count
         FROM keyword_pool
         WHERE canonical_id IS NULL
         ORDER BY use_count DESC, keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询规范词列表失败")?;

    Ok(rows
        .into_iter()
        .map(|(rowid, keyword, use_count)| CanonicalRow {
            rowid,
            keyword,
            use_count,
        })
        .collect())
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 根据词条 rowid 返回其规范词文本。
///
/// - 词条本身是 Canonical → 返回自身 keyword。
/// - 词条是 Alias / Pending → 返回其 canonical_id 指向词条的 keyword。
/// - 词条不存在或 canonical 指向缺失 → None。
pub async fn get_canonical_name(pool: &SqlitePool, word_id: i64) -> RamariaResult<Option<String>> {
    let row: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT keyword, canonical_id FROM keyword_pool WHERE rowid = ?")
            .bind(word_id)
            .fetch_optional(pool)
            .await
            .storage_err("查询词条失败")?;

    match row {
        None => Ok(None),
        Some((keyword, None)) => Ok(Some(keyword)),
        Some((_, Some(canonical_id))) => {
            let canonical: Option<String> =
                sqlx::query_scalar("SELECT keyword FROM keyword_pool WHERE rowid = ?")
                    .bind(canonical_id)
                    .fetch_optional(pool)
                    .await
                    .storage_err("查询规范词失败")?;
            Ok(canonical)
        }
    }
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 列出全部待确认别名冲突（alias_status='pending'，join 规范词文本）。
pub async fn list_pending_aliases(pool: &SqlitePool) -> RamariaResult<Vec<PendingAliasRow>> {
    let rows = sqlx::query_as::<_, (i64, String, i64, String, i64)>(
        "SELECT a.rowid, a.keyword, a.canonical_id, c.keyword, a.created_at
         FROM keyword_pool a
         JOIN keyword_pool c ON a.canonical_id = c.rowid
         WHERE a.alias_status = 'pending'
         ORDER BY a.created_at ASC, a.keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询待确认别名失败")?;

    Ok(rows
        .into_iter()
        .map(
            |(alias_id, alias_keyword, canonical_id, canonical_keyword, created_at)| {
                PendingAliasRow {
                    alias_id,
                    alias_keyword,
                    canonical_id,
                    canonical_keyword,
                    created_at,
                }
            },
        )
        .collect())
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 确认别名：把 alias_status='pending' 的词条置为 'alias'。
///
/// 返回是否发生更新（目标不存在 / 非 pending → false）。
pub async fn confirm_alias(pool: &SqlitePool, alias_id: i64) -> RamariaResult<bool> {
    let result = sqlx::query(
        "UPDATE keyword_pool SET alias_status = 'alias'
         WHERE rowid = ? AND alias_status = 'pending' AND canonical_id IS NOT NULL",
    )
    .bind(alias_id)
    .execute(pool)
    .await
    .storage_err("确认别名失败")?;
    Ok(result.rows_affected() > 0)
}

// 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
/// 驳回别名：把 alias_status='pending' 的词条晋升为独立规范词（清除 canonical 指向）。
///
/// 返回是否发生更新（目标不存在 / 非 pending → false）。
pub async fn reject_alias(pool: &SqlitePool, alias_id: i64) -> RamariaResult<bool> {
    let result = sqlx::query(
        "UPDATE keyword_pool SET canonical_id = NULL, alias_status = NULL
         WHERE rowid = ? AND alias_status = 'pending'",
    )
    .bind(alias_id)
    .execute(pool)
    .await
    .storage_err("驳回别名失败")?;
    Ok(result.rows_affected() > 0)
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
// M3 CLI 行视图 / rowid / 幂等种子（ramaria keyword）
// =========================================================

/// 列出 keyword_pool 全部词条（全字段行视图），按 use_count 降序、keyword 升序稳定排序。
///
/// 返回:
/// - `Vec<KeywordEntryRow>`: 词条全字段 + LEFT JOIN 解析出的指向规范词文本。
///
/// 说明:
/// - 输出覆盖规范词 / 已确认别名 / 待确认别名三种形态，供
///   `ramaria keyword list` 展示与词典装载共用。
/// - 排序稳定（use_count DESC、keyword ASC），重复调用输出一致。
pub async fn list_entries(pool: &SqlitePool) -> RamariaResult<Vec<KeywordEntryRow>> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            i64,
            Option<String>,
            Option<i64>,
            i64,
            Option<String>,
        ),
    >(
        "SELECT e.keyword, e.use_count, e.alias_status, e.canonical_id, e.created_at, c.keyword
         FROM keyword_pool e
         LEFT JOIN keyword_pool c ON e.canonical_id = c.rowid
         ORDER BY e.use_count DESC, e.keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询关键词行视图失败")?;

    Ok(rows
        .into_iter()
        .map(
            |(keyword, use_count, alias_status, canonical_id, created_at, canonical_keyword)| {
                KeywordEntryRow {
                    keyword,
                    use_count,
                    alias_status,
                    canonical_id,
                    created_at,
                    canonical_keyword,
                }
            },
        )
        .collect())
}

/// 列出 keyword_pool 全部词条装载行（含 rowid / 别名状态 / 规范词指向），供服务镜像装载。
///
/// 说明:
/// - 与 `list_entries` 覆盖同一批词条，但额外带出 `rowid`——`KeywordStatus::Alias/Pending`
///   的 `canonical_id` 指向 keyword_pool.rowid，KeywordPool 三态状态机装载必须保留该字段。
/// - 输出覆盖规范词 / 已确认别名 / 待确认别名三种形态。
/// - 排序稳定（use_count DESC、keyword ASC），重复调用输出一致。
pub async fn list_pool_rows(pool: &SqlitePool) -> RamariaResult<Vec<KeywordPoolRow>> {
    let rows = sqlx::query_as::<
        _,
        (i64, String, i64, i64, Option<String>, Option<i64>, Option<String>),
    >(
        "SELECT e.rowid, e.keyword, e.use_count, e.created_at, e.alias_status, e.canonical_id, c.keyword
         FROM keyword_pool e
         LEFT JOIN keyword_pool c ON e.canonical_id = c.rowid
         ORDER BY e.use_count DESC, e.keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询关键词装载行失败")?;

    Ok(rows
        .into_iter()
        .map(
            |(
                rowid,
                keyword,
                use_count,
                created_at,
                alias_status,
                canonical_id,
                canonical_keyword,
            )| {
                KeywordPoolRow {
                    rowid,
                    keyword,
                    use_count,
                    created_at,
                    alias_status,
                    canonical_id,
                    canonical_keyword,
                }
            },
        )
        .collect())
}

/// 按词条文本取其 rowid（供 alias confirm/reject 以文本定位行）。
///
/// 参数:
/// - `keyword`: 标准化后的关键词文本（通常来自 `KeywordToken::as_str()`）。
///
/// 返回:
/// - `Some(rowid)`: 词条存在。
/// - `None`: 词条不存在。
pub async fn find_rowid(pool: &SqlitePool, keyword: &str) -> RamariaResult<Option<i64>> {
    let rowid: Option<i64> = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind(keyword)
        .fetch_optional(pool)
        .await
        .storage_err("查询词条 rowid 失败")?;
    Ok(rowid)
}

/// 幂等手工种子：向 keyword_pool 注入一个规范词（use_count=0、canonical_id NULL、alias_status NULL）。
///
/// 参数:
/// - `keyword`: 标准化后的规范词（KeywordToken Newtype）。
///
/// 返回:
/// - `true`: 本次新插入（use_count 从 0 起，表示未经自然出现累积）。
/// - `false`: 词条已存在，**保持已有行完全不动**（use_count/别名状态均不修改）。
///
/// 说明:
/// - 由主键冲突直接 DO NOTHING 保证并发幂等：并发调用同一词条恰好插入一次，
///   不存在"先查后插"两方都判定不存在、第二次插入报错的竞态窗口。
/// - 已存在词条即使为 pending/alias 也不触碰、不提示——由调用方依 `list_entries`
///   行视图判断现状（如"已是别名/待确认，可用 alias confirm/reject 处理"）。
/// - 手工种子供词典增强分词（keyword_pool 规范词 → BigramWithDictionaryNormalizer
///   词典）消费，幂等性保证重复执行不递增 use_count、不改别名状态。
pub async fn seed_canonical(pool: &SqlitePool, keyword: &KeywordToken) -> RamariaResult<bool> {
    let now = ramaria_core::types::now_ms();
    // 并发幂等：由主键冲突直接 DO NOTHING，不依赖"先查后插"
    // （后者在并发下存在两方都判定不存在、第二次插入报错的窗口）。
    let result = sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at, canonical_id, alias_status)
         VALUES (?, 0, NULL, ?, NULL, NULL)
         ON CONFLICT(keyword) DO NOTHING",
    )
    .bind(keyword.as_str())
    .bind(now)
    .execute(pool)
    .await
    .storage_err("种子注入关键词失败")?;
    Ok(result.rows_affected() > 0)
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

    /// 取词条 rowid（供别名流测试；避免依赖自增起始值假设）。
    async fn rowid_of(pool: &SqlitePool, keyword: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT rowid FROM keyword_pool WHERE keyword = ?")
            .bind(keyword)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    // ── 别名归一化 CRUD（M3 T-V20-3-005）──

    /// 造一组数据：规范词 工作压力 + pending 别名 职场焦虑。
    async fn seed_pending(pool: &SqlitePool) -> (i64, i64) {
        upsert_with_alias(
            pool,
            &KeywordToken::new("工作压力").unwrap(),
            0,
            "canonical",
        )
        .await
        .unwrap();
        let canonical_id = rowid_of(pool, "工作压力").await;
        upsert_with_alias(
            pool,
            &KeywordToken::new("职场焦虑").unwrap(),
            canonical_id,
            "pending",
        )
        .await
        .unwrap();
        let alias_id = rowid_of(pool, "职场焦虑").await;
        (canonical_id, alias_id)
    }

    #[tokio::test]
    async fn test_list_canonicals_excludes_aliases() {
        let pool = setup().await;
        let (_, _) = seed_pending(&pool).await;

        // 追加一个纯 upsert（alias_status NULL）的规范词
        upsert(&pool, &KeywordToken::new("爬山").unwrap())
            .await
            .unwrap();

        let canonicals = list_canonicals(&pool).await.unwrap();
        // pending 别名不入规范词列表；upsert 形态（alias_status NULL）与 'canonical' 形态均计入
        let texts: Vec<&str> = canonicals.iter().map(|r| r.keyword.as_str()).collect();
        assert!(texts.contains(&"工作压力"));
        assert!(texts.contains(&"爬山"));
        assert!(
            !texts.contains(&"职场焦虑"),
            "pending 别名不应出现在规范词列表"
        );
        // rowid 与 use_count 带回
        assert!(canonicals.iter().all(|r| r.rowid > 0));
    }

    #[tokio::test]
    async fn test_pending_list_and_confirm_flow() {
        let pool = setup().await;
        let (canonical_id, alias_id) = seed_pending(&pool).await;

        // 1. pending 冲突列表正确（含规范词文本）
        let pending = list_pending_aliases(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].alias_id, alias_id);
        assert_eq!(pending[0].alias_keyword, "职场焦虑");
        assert_eq!(pending[0].canonical_id, canonical_id);
        assert_eq!(pending[0].canonical_keyword, "工作压力");

        // 2. confirm → pending 清空、状态转 alias
        assert!(confirm_alias(&pool, alias_id).await.unwrap());
        assert!(list_pending_aliases(&pool).await.unwrap().is_empty());
        assert_eq!(
            get_canonical_name(&pool, alias_id)
                .await
                .unwrap()
                .as_deref(),
            Some("工作压力")
        );

        // 3. 幂等：再次 confirm 返回 false
        assert!(!confirm_alias(&pool, alias_id).await.unwrap());
    }

    #[tokio::test]
    async fn test_pending_reject_promotes_to_canonical() {
        let pool = setup().await;
        let (canonical_id, alias_id) = seed_pending(&pool).await;

        assert!(reject_alias(&pool, alias_id).await.unwrap());
        // 晋升后：pending 清空、alias 变独立规范词、canonical 指向解除
        assert!(list_pending_aliases(&pool).await.unwrap().is_empty());
        assert_eq!(
            get_canonical_name(&pool, alias_id)
                .await
                .unwrap()
                .as_deref(),
            Some("职场焦虑")
        );
        let canonicals = list_canonicals(&pool).await.unwrap();
        assert!(canonicals.iter().any(|r| r.keyword == "职场焦虑"));

        // canonical_id 仍指向旧规范词？reject 已清除 → find_canonical_id 应 None
        let cid = find_canonical_id(&pool, &KeywordToken::new("职场焦虑").unwrap())
            .await
            .unwrap();
        assert!(cid.is_none());
        let _ = canonical_id;

        // 幂等：再次 reject 返回 false
        assert!(!reject_alias(&pool, alias_id).await.unwrap());
    }

    #[tokio::test]
    async fn test_get_canonical_name_self_and_missing() {
        let pool = setup().await;
        let (canonical_id, _) = seed_pending(&pool).await;

        // 规范词自身 → 返回自身文本
        assert_eq!(
            get_canonical_name(&pool, canonical_id)
                .await
                .unwrap()
                .as_deref(),
            Some("工作压力")
        );
        // 不存在的 rowid → None
        assert!(get_canonical_name(&pool, 999_999).await.unwrap().is_none());
    }

    // ── M3 CLI 行视图 / rowid / 幂等种子 ──

    #[tokio::test]
    async fn test_list_entries_order_and_status_fields() {
        let pool = setup().await;
        // 工作压力 use_count 2、爬山 use_count 1（纯 upsert 形态规范词）
        upsert(&pool, &KeywordToken::new("工作压力").unwrap())
            .await
            .unwrap();
        upsert(&pool, &KeywordToken::new("工作压力").unwrap())
            .await
            .unwrap();
        upsert(&pool, &KeywordToken::new("爬山").unwrap())
            .await
            .unwrap();
        // 职场焦虑 → pending 别名（指向 工作压力）
        let canonical_id = rowid_of(&pool, "工作压力").await;
        upsert_with_alias(
            &pool,
            &KeywordToken::new("职场焦虑").unwrap(),
            canonical_id,
            "pending",
        )
        .await
        .unwrap();
        // 职业倦怠 → 已确认 alias（指向 工作压力）
        upsert_with_alias(
            &pool,
            &KeywordToken::new("职业倦怠").unwrap(),
            canonical_id,
            "alias",
        )
        .await
        .unwrap();

        let entries = list_entries(&pool).await.unwrap();
        // 排序：use_count DESC（工作压力 2 排第一），其余 use_count=1 按 keyword ASC
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].keyword, "工作压力");
        assert_eq!(entries[0].use_count, 2);
        // 同 use_count=1 的三条按 keyword 升序（UTF-8 字节序）：
        // 爬山(U+722C) < 职业倦怠(U+804C 4E1A) < 职场焦虑(U+804C 573A)
        let tail: Vec<&str> = entries[1..].iter().map(|r| r.keyword.as_str()).collect();
        assert_eq!(tail, vec!["爬山", "职业倦怠", "职场焦虑"]);

        // 状态字段：pending 别名携带 canonical_id 与 LEFT JOIN 出的规范词文本
        let pending = entries
            .iter()
            .find(|r| r.keyword == "职场焦虑")
            .expect("pending 别名应出现");
        assert_eq!(pending.alias_status.as_deref(), Some("pending"));
        assert_eq!(pending.canonical_id, Some(canonical_id));
        assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));

        // alias 状态携带规范词文本
        let alias = entries
            .iter()
            .find(|r| r.keyword == "职业倦怠")
            .expect("alias 应出现");
        assert_eq!(alias.alias_status.as_deref(), Some("alias"));
        assert_eq!(alias.canonical_keyword.as_deref(), Some("工作压力"));

        // 规范词自身：canonical_id/canonical_keyword 均 None，alias_status NULL
        let canonical = entries
            .iter()
            .find(|r| r.keyword == "爬山")
            .expect("规范词应出现");
        assert_eq!(canonical.canonical_id, None);
        assert_eq!(canonical.canonical_keyword, None);
        assert_eq!(canonical.alias_status, None);
        assert!(canonical.created_at > 0);
    }

    #[tokio::test]
    async fn test_find_rowid_existing_and_missing() {
        let pool = setup().await;
        upsert(&pool, &KeywordToken::new("工作压力").unwrap())
            .await
            .unwrap();

        let rid = find_rowid(&pool, "工作压力").await.unwrap();
        assert_eq!(rid, Some(rowid_of(&pool, "工作压力").await));

        // 未标准化/不存在均返回 None
        assert!(find_rowid(&pool, "Work Stress").await.unwrap().is_none());
        assert!(find_rowid(&pool, "不存在的词").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_seed_canonical_new_inserts_zero_count() {
        let pool = setup().await;
        let kw = KeywordToken::new("手工词").unwrap();

        assert!(seed_canonical(&pool, &kw).await.unwrap(), "新词应插入");

        let entries = list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].keyword, "手工词");
        assert_eq!(entries[0].use_count, 0, "种子词 use_count 从 0 起");
        assert_eq!(entries[0].alias_status, None, "种子词为规范词形态");
        assert_eq!(entries[0].canonical_id, None);
        assert_eq!(entries[0].canonical_keyword, None);
    }

    #[tokio::test]
    async fn test_seed_canonical_existing_idempotent_keeps_count() {
        let pool = setup().await;
        let kw = KeywordToken::new("工作压力").unwrap();
        // 先自然累积 2 次（use_count=2）
        upsert(&pool, &kw).await.unwrap();
        upsert(&pool, &kw).await.unwrap();
        let before = rowid_of(&pool, "工作压力").await;

        // 再次种子 → false，不递增 use_count、不改状态
        assert!(
            !seed_canonical(&pool, &kw).await.unwrap(),
            "已存在不应重复插入"
        );
        let entries = list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].use_count, 2, "幂等种子不得递增 use_count");
        assert_eq!(rowid_of(&pool, "工作压力").await, before, "rowid 不变");
    }

    #[tokio::test]
    async fn test_seed_canonical_existing_alias_untouched() {
        let pool = setup().await;
        // 工作压力（canonical）+ 职场焦虑（pending 指向它）
        let (canonical_id, alias_id) = seed_pending(&pool).await;
        let alias_kw = KeywordToken::new("职场焦虑").unwrap();
        let canonical_kw = KeywordToken::new("工作压力").unwrap();

        // 对已存在的 pending 别名词条执行种子：不得触碰状态 / canonical_id / use_count
        assert!(!seed_canonical(&pool, &alias_kw).await.unwrap());
        // 对已存在的规范词执行种子：同样保持不动
        assert!(!seed_canonical(&pool, &canonical_kw).await.unwrap());

        let entries = list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 2);
        let pending = entries
            .iter()
            .find(|r| r.keyword == "职场焦虑")
            .expect("pending 应保留");
        assert_eq!(pending.alias_status.as_deref(), Some("pending"));
        assert_eq!(pending.canonical_id, Some(canonical_id));
        assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));
        assert_eq!(pending.use_count, 1, "种子不得递增 use_count");
        // 规范词未被改名/改状态
        assert!(
            list_canonicals(&pool)
                .await
                .unwrap()
                .iter()
                .any(|r| r.keyword == "工作压力")
        );
        let _ = alias_id;
    }

    // ── list_pool_rows（服务镜像装载行）──

    /// 装载行携带 rowid + 三态状态字段；canonical_keyword 由 LEFT JOIN 解析。
    #[tokio::test]
    async fn test_list_pool_rows_carries_rowid_and_status() {
        let pool = setup().await;
        let (canonical_id, _) = seed_pending(&pool).await;
        // 追加一个已确认 alias（指向 工作压力）
        upsert_with_alias(
            &pool,
            &KeywordToken::new("职业倦怠").unwrap(),
            canonical_id,
            "alias",
        )
        .await
        .unwrap();

        let rows = list_pool_rows(&pool).await.unwrap();
        // 3 条：canonical 工作压力 + pending 职场焦虑 + alias 职业倦怠
        assert_eq!(rows.len(), 3);

        let canonical = rows.iter().find(|r| r.keyword == "工作压力").unwrap();
        assert_eq!(canonical.rowid, canonical_id);
        // 规范词形态：alias_status 为 NULL 或 "canonical"（本例 seed 写入为 "canonical"），
        // canonical_id / canonical_keyword 恒为 None
        assert!(matches!(
            canonical.alias_status.as_deref(),
            None | Some("canonical")
        ));
        assert_eq!(canonical.canonical_id, None);
        assert_eq!(canonical.canonical_keyword, None);
        assert!(canonical.created_at > 0);

        let pending = rows.iter().find(|r| r.keyword == "职场焦虑").unwrap();
        assert_eq!(pending.alias_status.as_deref(), Some("pending"));
        assert_eq!(pending.canonical_id, Some(canonical_id));
        assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));

        let alias = rows.iter().find(|r| r.keyword == "职业倦怠").unwrap();
        assert_eq!(alias.alias_status.as_deref(), Some("alias"));
        assert_eq!(alias.canonical_id, Some(canonical_id));
        assert_eq!(alias.canonical_keyword.as_deref(), Some("工作压力"));
    }

    #[tokio::test]
    async fn test_seed_canonical_concurrent_is_idempotent() {
        let pool = setup().await;
        let kw = KeywordToken::new("并发词").unwrap();
        let (a, b) = tokio::join!(seed_canonical(&pool, &kw), seed_canonical(&pool, &kw));
        let inserted = [a.unwrap(), b.unwrap()];
        assert_eq!(
            inserted.iter().filter(|x| **x).count(),
            1,
            "并发 seed 同一词条恰好插入一次"
        );
        let entries = list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].use_count, 0);
    }

    /// canonical_id 契约：别名行的 canonical_id 恒等于规范词行的隐式 rowid
    /// （keyword_pool 无显式 INTEGER 主键，故禁止对该表执行 VACUUM）。
    #[tokio::test]
    async fn test_alias_points_to_canonical_rowid() {
        let pool = setup().await;
        let (canonical_id, alias_id) = seed_pending(&pool).await;
        let stored: Option<i64> =
            sqlx::query_scalar("SELECT canonical_id FROM keyword_pool WHERE rowid = ?")
                .bind(alias_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            stored,
            Some(canonical_id),
            "别名 canonical_id 必须指向规范词 rowid"
        );
        assert_eq!(
            get_canonical_name(&pool, alias_id)
                .await
                .unwrap()
                .as_deref(),
            Some("工作压力")
        );
    }
}
