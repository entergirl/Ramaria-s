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

// 待确认别名冲突行的定义位于 ramaria-core 关键词类型模块（存储 trait 与展示层共用同一
// 数据形态），此处再导出供存储侧调用点按原路径引用。
pub use ramaria_core::keyword::PendingAliasRow;

// =========================================================
// 别名归一化行结构（keyword-design §6.2）
// =========================================================

/// 词条行（keyword_pool 的 rowid / keyword / use_count 三元组）。
///
/// 说明:
/// - `list_canonicals` 返回规范词子集（canonical_id IS NULL）；
/// - `list_established` 返回已确认词表（canonical + 已确认 alias）子集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRow {
    /// keyword_pool.rowid（INTEGER 自增 rowid）
    pub rowid: i64,
    /// 词条文本
    pub keyword: String,
    /// 使用次数（alias 行为该 alias 自身的使用量）
    pub use_count: i64,
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

/// 列出 keyword_pool 已确认词表（canonical + 已确认 alias，排除 pending）。
///
/// 返回:
/// - `Vec<CanonicalRow>`，按 use_count 降序、keyword 升序（输出稳定）；
///   alias 行的 `rowid` / `use_count` 为 alias 自身行值。
///
/// 说明:
/// - 与内存侧 `KeywordPool::established_terms` 的口径对应关系:
///   canonical = `alias_status` 为 NULL / `'canonical'`；
///   alias = `alias_status = 'alias'` 且 `canonical_id IS NOT NULL`（已确认归一指向）；
///   pending（`alias_status = 'pending'`）排除。
/// - 缺 `canonical_id` 的 alias 属数据异常：本查询按未确认处理并排除；
///   内存侧装载时对该异常兜底为 Canonical（防御不丢词），正常数据两口径一致。
/// - 与 `list_canonicals`（仅 canonical_id IS NULL）并存，后者另作规范词专用用途。
pub async fn list_established(pool: &SqlitePool) -> RamariaResult<Vec<CanonicalRow>> {
    let rows = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT rowid, keyword, use_count
         FROM keyword_pool
         WHERE alias_status IS NULL
            OR alias_status = 'canonical'
            OR (alias_status = 'alias' AND canonical_id IS NOT NULL)
         ORDER BY use_count DESC, keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询已确认词表失败")?;

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

/// 幂等登记待确认别名（`alias_status='pending'`，指向建议合并的规范词）。
///
/// 参数:
/// - `alias`: 标准化后的别名文本（KeywordToken Newtype）。
/// - `canonical_id`: 建议合并到的规范词 rowid（调用方保证指向真实词条）。
/// - `use_count`: 观测使用计数（仅首次插入写入，取 ≥1）。
///
/// 返回:
/// - `true`: 本次新插入（携带观测计数与登记时间）；
/// - `false`: 词条已存在（任意状态，**保持已有行完全不动**）。
///
/// 说明:
/// - 由主键冲突直接 DO NOTHING 保证并发幂等：并发登记同一别名恰好插入一次，
///   已存在词条（canonical / alias / pending）不被改写为 pending。
/// - 与 `upsert_with_alias` 的差异：后者是通用写入口（冲突时递增 use_count 并
///   COALESCE 更新状态），本函数是"建议落库"专用入口，冲突时零改动。
pub async fn upsert_pending(
    pool: &SqlitePool,
    alias: &KeywordToken,
    canonical_id: i64,
    use_count: u32,
) -> RamariaResult<bool> {
    let now = ramaria_core::types::now_ms();
    let result = sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at, canonical_id, alias_status)
         VALUES (?, ?, ?, ?, ?, 'pending')
         ON CONFLICT(keyword) DO NOTHING",
    )
    .bind(alias.as_str())
    .bind(i64::from(use_count.max(1)))
    .bind(now)
    .bind(now)
    .bind(canonical_id)
    .execute(pool)
    .await
    .storage_err("登记待确认别名失败")?;
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

/// 按文本批量查询词条别名状态（供增量镜像与持久化状态保持一致）。
///
/// 参数:
/// - `keywords`: 待查询的标准化词条文本（不存在的文本不出现在结果中）。
///
/// 返回:
/// - `(keyword, alias_status)` 列表：`None` / `Some("canonical")` 为规范词，
///   `Some("alias")` 已确认别名，`Some("pending")` 待确认别名；
///   结果顺序不保证（调用方按文本查找消费）。
///
/// 说明:
/// - 空输入直接返回空（不访问数据库）；
/// - 按 500 分片查询（SQLite 变量上限保护），动态占位符只拼接 `?`，值全部绑定。
pub async fn list_keyword_statuses(
    pool: &SqlitePool,
    keywords: &[String],
) -> RamariaResult<Vec<(String, Option<String>)>> {
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    const CHUNK_SIZE: usize = 500;

    let mut result: Vec<(String, Option<String>)> = Vec::with_capacity(keywords.len());
    for chunk in keywords.chunks(CHUNK_SIZE) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT keyword, alias_status FROM keyword_pool WHERE keyword IN ({})",
            placeholders.join(", ")
        );
        let mut query = sqlx::query_as::<_, (String, Option<String>)>(&sql);
        for keyword in chunk {
            query = query.bind(keyword);
        }
        let rows = query
            .fetch_all(pool)
            .await
            .storage_err("查询词条状态失败")?;
        result.extend(rows);
    }
    Ok(result)
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

#[cfg(test)]
mod tests;
