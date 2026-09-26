//! crates/ramaria-service/src/keyword.rs - 关键词词典用例（列表 / 待确认别名 / 别名裁决）
//!
//! 设计特点:
//! - 只读列表 + 别名裁决状态机：三态（canonical / alias / pending）展示与
//!   pending → alias（确认合并）/ canonical（驳回晋升）迁移
//! - 入口差异由参数表达：confirm 且词条已是 alias 时，`already_applied_ok = false`
//!   报业务校验错误、`true` 幂等返回成功（不写库）
//! - 非法输入显式校验：关键词文本经 `KeywordToken` 标准化（空 / 超长拒绝），
//!   词条不存在 / 非 pending 均返回业务校验错误，不静默成功
//! - 日志脱敏：别名文本在日志中只保留长度与短哈希标签，正文不入日志

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::keyword::KeywordToken;

use crate::engine::Engine;
use crate::types::{
    AliasAction, AliasResolveOutcome, AliasResolveRequest, KeywordEntryView, KeywordPoolView,
    PendingAliasView,
};

// =========================================================
// 词条状态与文本校验
// =========================================================

/// 词条状态文本（keyword_pool 口径: canonical / alias / pending）。
///
/// 说明:
/// - `NULL` / `"canonical"` 及未知取值统一兜底为 `canonical`（与词表装载口径一致）。
pub(crate) fn status_of(alias_status: &Option<String>) -> &'static str {
    match alias_status.as_deref() {
        Some("alias") => "alias",
        Some("pending") => "pending",
        _ => "canonical",
    }
}

/// 校验关键词文本为合法标准化 token。
///
/// 返回:
/// - 成功时返回 trim + 英文小写后的标准化 token。
/// - 空 / 纯空白 / 超长（> 256 字节）时返回业务校验错误。
pub(crate) fn parse_keyword(raw: &str) -> RamariaResult<KeywordToken> {
    KeywordToken::new(raw).ok_or_else(|| {
        RamariaError::validation(format!("无效关键词: '{raw}'（需非空且不超过 256 字符）"))
    })
}

// =========================================================
// 关键词池列表
// =========================================================

/// 列出关键词池全部词条（含三态计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 全量词条视图（存储层稳定排序）与 canonical / alias / pending 三态计数。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<KeywordPoolView> {
    let entries = engine.storage_ref().list_keyword_pool_entries().await?;

    let mut canonical_count = 0usize;
    let mut alias_count = 0usize;
    let mut pending_count = 0usize;
    let mut keywords = Vec::with_capacity(entries.len());

    for e in entries {
        match status_of(&e.alias_status) {
            "canonical" => canonical_count += 1,
            "alias" => alias_count += 1,
            _ => pending_count += 1,
        }
        keywords.push(KeywordEntryView {
            keyword: e.keyword,
            use_count: e.use_count,
            status: status_of(&e.alias_status).to_string(),
            canonical_id: e.canonical_id,
            canonical_keyword: e.canonical_keyword,
            created_at: e.created_at,
        });
    }

    tracing::debug!(
        total = keywords.len(),
        canonical = canonical_count,
        alias = alias_count,
        pending = pending_count,
        "关键词池列表完成"
    );
    Ok(KeywordPoolView {
        total: keywords.len(),
        canonical_count,
        alias_count,
        pending_count,
        keywords,
    })
}

// =========================================================
// 待确认别名
// =========================================================

/// 列出全部待确认别名冲突（pending，别名 → 建议规范词）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 待确认别名视图列表（无待确认项时为空列表）。
pub(crate) async fn pending_aliases(engine: &Engine) -> RamariaResult<Vec<PendingAliasView>> {
    let pending = engine.storage_ref().list_pending_aliases().await?;
    let views: Vec<PendingAliasView> = pending
        .into_iter()
        .map(|p| PendingAliasView {
            alias_id: p.alias_id,
            alias: p.alias_keyword,
            canonical: p.canonical_keyword,
            created_at: p.created_at,
        })
        .collect();

    tracing::debug!(count = views.len(), "待确认别名列表完成");
    Ok(views)
}

// =========================================================
// 别名裁决（确认 / 驳回）
// =========================================================

/// 确认（合并到规范词）或驳回（晋升独立规范词）单个待确认别名。
///
/// 流程:
/// 1. 标准化别名文本并按标准化文本定位词条（不存在 → 业务校验错误）；
/// 2. 状态判定与迁移：
///    - `pending`: 执行确认 / 驳回；条件更新未命中（状态已变化）→ 业务校验错误；
///      成功时 confirm 返回指向的规范词文本、状态 `alias`；
///      reject 规范词为 None、状态 `canonical`；
///    - confirm 且已是 `alias`: `already_applied_ok = true` 时幂等返回成功（不写库），
///      `false` 时按"非 pending"报业务校验错误；
///    - 其余（reject 且非 pending、confirm 且 canonical）: 业务校验错误。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 裁决请求（别名文本 / 动作 / 幂等开关）。
///
/// 返回:
/// - 裁决结果（别名、处理后规范词、状态、是否幂等命中）。
pub(crate) async fn resolve_alias(
    engine: &Engine,
    req: AliasResolveRequest,
) -> RamariaResult<AliasResolveOutcome> {
    let token = parse_keyword(&req.alias)?;
    let storage = engine.storage_ref();

    let entries = storage.list_keyword_pool_entries().await?;
    let entry = entries
        .iter()
        .find(|e| e.keyword == token.as_str())
        .ok_or_else(|| RamariaError::validation(format!("关键词 '{}' 不存在", req.alias)))?;

    let status = status_of(&entry.alias_status);
    let confirm = matches!(req.action, AliasAction::Confirm);

    if status != "pending" {
        // confirm 且已是 alias：按调用入口口径选择幂等成功或报错
        if confirm && status == "alias" && req.already_applied_ok {
            tracing::debug!(
                alias = %redact_text_label(token.as_str()),
                "别名裁决幂等返回（已是合并状态）"
            );
            return Ok(AliasResolveOutcome {
                alias: token.as_str().to_string(),
                canonical_keyword: entry.canonical_keyword.clone(),
                status: "alias".to_string(),
                already_applied: true,
            });
        }
        let verb = if confirm { "确认合并" } else { "驳回" };
        return Err(RamariaError::validation(format!(
            "关键词 '{}' 当前状态为 {status}，不是待确认别名（pending），无法{verb}",
            req.alias
        )));
    }

    let changed = if confirm {
        storage.confirm_keyword_alias(entry.rowid).await?
    } else {
        storage.reject_keyword_alias(entry.rowid).await?
    };
    if !changed {
        return Err(RamariaError::validation(format!(
            "词条 '{}' 状态已变化（非待确认别名），操作未执行，请刷新后重试",
            req.alias
        )));
    }

    let action = if confirm { "confirm" } else { "reject" };
    tracing::debug!(
        alias = %redact_text_label(token.as_str()),
        action,
        "别名状态迁移完成"
    );

    Ok(AliasResolveOutcome {
        alias: token.as_str().to_string(),
        canonical_keyword: if confirm {
            entry.canonical_keyword.clone()
        } else {
            None
        },
        status: if confirm { "alias" } else { "canonical" }.to_string(),
        already_applied: false,
    })
}

// =========================================================
// 日志脱敏（文件内私有）
// =========================================================

/// 生成文本脱敏标签：`<N chars>#<8 位十六进制哈希>`。
///
/// 用途:
/// - 日志记录别名等用户文本时仅保留长度与哈希，正文不出现在日志中。
fn redact_text_label(text: &str) -> String {
    format!(
        "<{} chars>#{:08x}",
        text.chars().count(),
        fnv1a32(text.as_bytes())
    )
}

/// 64 位 FNV-1a 哈希取低 32 位（仅用于日志标签，不用于安全用途）。
fn fnv1a32(bytes: &[u8]) -> u32 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (hash & 0xffff_ffff) as u32
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::engine_with_db;
    use ramaria_storage::repo::keyword as kw_repo;

    /// 打开与引擎同一库文件的连接池（造 pending 别名等测试数据用）。
    async fn open_pool(dir: &std::path::Path) -> sqlx::SqlitePool {
        ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
            .await
            .expect("打开测试库连接池应成功")
    }

    /// 造一个规范词「工作压力」+ 若干 pending 别名，返回规范词 rowid。
    async fn seed_pending_aliases(pool: &sqlx::SqlitePool, aliases: &[&str]) -> i64 {
        kw_repo::upsert_with_alias(
            pool,
            &KeywordToken::new("工作压力").unwrap(),
            0,
            "canonical",
        )
        .await
        .expect("写入规范词应成功");
        let canonical_id = kw_repo::find_rowid(pool, "工作压力")
            .await
            .expect("查询 rowid 应成功")
            .expect("规范词应存在");
        for alias in aliases {
            kw_repo::upsert_with_alias(
                pool,
                &KeywordToken::new(alias).unwrap(),
                canonical_id,
                "pending",
            )
            .await
            .expect("写入待确认别名应成功");
        }
        canonical_id
    }

    /// 三态映射：None / canonical / alias / pending / 未知取值兜底。
    #[test]
    fn status_mapping_covers_three_states() {
        assert_eq!(status_of(&None), "canonical");
        assert_eq!(status_of(&Some("canonical".to_string())), "canonical");
        assert_eq!(status_of(&Some("alias".to_string())), "alias");
        assert_eq!(status_of(&Some("pending".to_string())), "pending");
        assert_eq!(status_of(&Some("weird".to_string())), "canonical");
    }

    /// 文本校验：空 / 纯空白 / 超长拒绝；正常文本标准化。
    #[test]
    fn keyword_token_validation_rejects_invalid() {
        assert!(parse_keyword("").is_err());
        assert!(parse_keyword("   ").is_err());
        assert!(parse_keyword(&"x".repeat(300)).is_err());
        assert_eq!(parse_keyword("  工作压力  ").unwrap().as_str(), "工作压力");
        assert_eq!(
            parse_keyword("Work Stress").unwrap().as_str(),
            "work stress",
            "英文应小写化"
        );
    }

    /// 日志脱敏标签：只保留长度与哈希，不出现正文。
    #[test]
    fn redact_label_keeps_length_and_hash_only() {
        let label = redact_text_label("职场焦虑");
        assert!(
            label.starts_with("<4 chars>#"),
            "标签应只含长度与哈希: {label}"
        );
        assert!(!label.contains("职场"), "标签不应包含正文: {label}");
        assert_eq!(label, redact_text_label("职场焦虑"), "同一文本标签应稳定");
        assert_ne!(label, redact_text_label("职业倦怠"), "不同文本标签应可区分");
    }

    /// 列表：三态计数与词条字段（pending 携带规范词指向）。
    #[tokio::test]
    async fn list_reports_three_state_counts() {
        let (engine, _storage, dir) = engine_with_db("keyword-list").await;
        let pool = open_pool(&dir).await;
        let canonical_id = seed_pending_aliases(&pool, &["职场焦虑"]).await;
        kw_repo::upsert_with_alias(
            &pool,
            &KeywordToken::new("职业倦怠").unwrap(),
            canonical_id,
            "alias",
        )
        .await
        .expect("写入已确认别名应成功");

        let view = engine.keyword_list().await.expect("关键词列表应成功");
        assert_eq!(view.total, 3);
        assert_eq!(view.canonical_count, 1);
        assert_eq!(view.alias_count, 1);
        assert_eq!(view.pending_count, 1);

        let pending = view
            .keywords
            .iter()
            .find(|k| k.keyword == "职场焦虑")
            .expect("pending 别名应出现");
        assert_eq!(pending.status, "pending");
        assert_eq!(pending.canonical_id, Some(canonical_id));
        assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));

        let canonical = view
            .keywords
            .iter()
            .find(|k| k.keyword == "工作压力")
            .expect("规范词应出现");
        assert_eq!(canonical.status, "canonical");
        assert_eq!(canonical.canonical_id, None);
        assert_eq!(canonical.canonical_keyword, None);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 待确认别名列表形状：alias_id / alias / canonical / created_at。
    #[tokio::test]
    async fn pending_alias_list_shape() {
        let (engine, _storage, dir) = engine_with_db("keyword-pending").await;
        let pool = open_pool(&dir).await;
        seed_pending_aliases(&pool, &["职场焦虑", "职业倦怠"]).await;

        let list = engine
            .keyword_pending_aliases()
            .await
            .expect("待确认别名列表应成功");
        assert_eq!(list.len(), 2);
        let anxious = list
            .iter()
            .find(|p| p.alias == "职场焦虑")
            .expect("职场焦虑应出现");
        assert!(anxious.alias_id > 0);
        assert_eq!(anxious.canonical, "工作压力");
        assert!(anxious.created_at > 0);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 裁决全流程：确认成功 → 再次确认（桌面报错 / 幂等口径成功）→ 驳回成功 → 再次驳回报错。
    #[tokio::test]
    async fn resolve_alias_confirm_then_reject() {
        let (engine, _storage, dir) = engine_with_db("keyword-resolve").await;
        let pool = open_pool(&dir).await;
        seed_pending_aliases(&pool, &["职场焦虑", "职业倦怠"]).await;

        // 确认合并成功
        let outcome = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "职场焦虑".to_string(),
                action: AliasAction::Confirm,
                already_applied_ok: false,
            })
            .await
            .expect("确认合并应成功");
        assert_eq!(outcome.alias, "职场焦虑");
        assert_eq!(outcome.canonical_keyword.as_deref(), Some("工作压力"));
        assert_eq!(outcome.status, "alias");
        assert!(!outcome.already_applied);

        // 再次确认：桌面口径（already_applied_ok=false）报业务校验错误
        let err = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "职场焦虑".to_string(),
                action: AliasAction::Confirm,
                already_applied_ok: false,
            })
            .await
            .expect_err("已是 alias 时桌面口径应报错");
        assert_eq!(err.category(), "validation");

        // 再次确认：幂等口径成功且不写库
        let idempotent = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "职场焦虑".to_string(),
                action: AliasAction::Confirm,
                already_applied_ok: true,
            })
            .await
            .expect("幂等口径应成功");
        assert!(idempotent.already_applied);
        assert_eq!(idempotent.status, "alias");
        assert_eq!(idempotent.canonical_keyword.as_deref(), Some("工作压力"));

        // 驳回晋升成功
        let rejected = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "职业倦怠".to_string(),
                action: AliasAction::Reject,
                already_applied_ok: true,
            })
            .await
            .expect("驳回应成功");
        assert_eq!(rejected.status, "canonical");
        assert_eq!(rejected.canonical_keyword, None);
        assert!(!rejected.already_applied);

        // 再次驳回：非 pending 一律报错
        let err = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "职业倦怠".to_string(),
                action: AliasAction::Reject,
                already_applied_ok: true,
            })
            .await
            .expect_err("非 pending 驳回应报错");
        assert_eq!(err.category(), "validation");

        // 落库状态核对：pending 清空、alias 1 条、规范词 2 条
        let view = engine.keyword_list().await.expect("关键词列表应成功");
        assert_eq!(view.pending_count, 0);
        assert_eq!(view.alias_count, 1);
        assert_eq!(view.canonical_count, 2);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不存在 / 非 pending / 非法文本：一律业务校验错误，不写库。
    #[tokio::test]
    async fn resolve_alias_rejects_missing_and_non_pending() {
        let (engine, _storage, dir) = engine_with_db("keyword-invalid").await;
        let pool = open_pool(&dir).await;
        kw_repo::upsert_with_alias(
            &pool,
            &KeywordToken::new("工作压力").unwrap(),
            0,
            "canonical",
        )
        .await
        .expect("写入规范词应成功");

        // 规范词（非 pending）确认
        let err = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "工作压力".to_string(),
                action: AliasAction::Confirm,
                already_applied_ok: true,
            })
            .await
            .expect_err("非 pending 确认应报错");
        assert_eq!(err.category(), "validation");

        // 词条不存在
        let err = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "不存在的词".to_string(),
                action: AliasAction::Confirm,
                already_applied_ok: true,
            })
            .await
            .expect_err("词条不存在应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.to_string().contains("不存在"),
            "错误应提示不存在: {err}"
        );

        // 非法文本（纯空白）
        let err = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "   ".to_string(),
                action: AliasAction::Reject,
                already_applied_ok: true,
            })
            .await
            .expect_err("空别名应报错");
        assert_eq!(err.category(), "validation");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 竞争失败路径：pending 行缺规范词指向（数据异常）时条件更新未命中，
    /// 返回「状态已变化」业务校验错误。
    #[tokio::test]
    async fn resolve_alias_reports_changed_state() {
        let (engine, _storage, dir) = engine_with_db("keyword-changed").await;
        let pool = open_pool(&dir).await;
        // alias_status='pending' 但缺 canonical_id：行视图按 pending 展示，条件更新不会命中
        kw_repo::upsert_with_alias(&pool, &KeywordToken::new("孤儿别名").unwrap(), 0, "pending")
            .await
            .expect("写入异常别名行应成功");

        let err = engine
            .keyword_resolve_alias(AliasResolveRequest {
                alias: "孤儿别名".to_string(),
                action: AliasAction::Confirm,
                already_applied_ok: false,
            })
            .await
            .expect_err("条件更新未命中应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.to_string().contains("状态已变化"),
            "错误应提示状态已变化: {err}"
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
