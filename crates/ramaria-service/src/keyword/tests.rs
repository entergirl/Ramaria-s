//! crates/ramaria-service/src/keyword/tests.rs - 关键词用例测试
//!
//! 设计特点:
//! - 由 keyword.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖三态列表 / 别名裁决 /
//!   幂等注入 / 别名建议（扫描 → 建议 → 落库 → 裁决）四条路径
//! - 真实 SQLite（临时文件库 + 全量 migration）：直接断言落库结果与入口返回口径
//! - 别名建议语料经 L1 增量镜像或镜像 token 注入构造（确定性，无 LLM / 嵌入依赖）
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::*;
use crate::test_support::{engine_with_db, seed_l1, seed_persona};
use ramaria_core::traits::StoreCrud;
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

// ---- seed（幂等注入） ----

/// 新词注入：use_count 从 0 起；二次注入幂等（保持现状、不递增计数）。
#[tokio::test]
async fn seed_inserts_then_is_idempotent() {
    let (engine, _storage, dir) = engine_with_db("keyword-seed-idempotent").await;

    let outcome = engine
        .keyword_seed(&["工作压力".to_string(), "加班".to_string()])
        .await
        .expect("seed 应成功");
    assert_eq!(outcome.seeded, 2);
    assert_eq!(outcome.skipped, 0);
    assert_eq!(outcome.results.len(), 2);
    assert!(
        outcome
            .results
            .iter()
            .all(|item| item.inserted && item.status == "canonical"),
        "新词条应全部为 canonical 且标记新插入"
    );

    // 二次注入同一词条：幂等保持现状
    let outcome = engine
        .keyword_seed(&["工作压力".to_string()])
        .await
        .expect("seed 应成功");
    assert_eq!(outcome.seeded, 0);
    assert_eq!(outcome.skipped, 1);
    assert!(!outcome.results[0].inserted);

    let view = engine.keyword_list().await.expect("列表应成功");
    assert_eq!(view.total, 2);
    let work = view
        .keywords
        .iter()
        .find(|k| k.keyword == "工作压力")
        .expect("词条应存在");
    assert_eq!(work.use_count, 0, "重复注入不得递增 use_count");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 已存在别名词条：回传现状状态且不触碰（不递增 use_count、不改别名状态）。
#[tokio::test]
async fn seed_keeps_existing_alias_state() {
    let (engine, _storage, dir) = engine_with_db("keyword-seed-existing").await;
    let pool = open_pool(&dir).await;
    seed_pending_aliases(&pool, &["职场焦虑"]).await;

    let outcome = engine
        .keyword_seed(&["职场焦虑".to_string()])
        .await
        .expect("seed 应成功");
    assert_eq!(outcome.seeded, 0);
    assert_eq!(outcome.skipped, 1);
    assert!(!outcome.results[0].inserted);
    assert_eq!(outcome.results[0].status, "pending");

    // 现状未被触碰：仍为 pending 且指向规范词
    let view = engine.keyword_list().await.expect("列表应成功");
    let item = view
        .keywords
        .iter()
        .find(|k| k.keyword == "职场焦虑")
        .expect("词条应存在");
    assert_eq!(item.status, "pending");
    assert_eq!(item.canonical_keyword.as_deref(), Some("工作压力"));

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 去重与空输入：重复文本只处理一次、文本经标准化；空输入返回空结果。
#[tokio::test]
async fn seed_dedupes_and_accepts_empty_input() {
    let (engine, _storage, dir) = engine_with_db("keyword-seed-dedupe").await;

    let outcome = engine
        .keyword_seed(&[
            "工作压力".to_string(),
            "工作压力".to_string(),
            " 加班 ".to_string(),
        ])
        .await
        .expect("seed 应成功");
    assert_eq!(outcome.results.len(), 2, "重复文本应去重");
    assert_eq!(outcome.results[1].keyword, "加班", "文本应经标准化");

    let empty = engine.keyword_seed(&[]).await.expect("空输入应成功");
    assert_eq!(empty.seeded, 0);
    assert_eq!(empty.skipped, 0);
    assert!(empty.results.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 非法词条：整体报错且不部分写入（其余合法词条也不会被注入）。
#[tokio::test]
async fn seed_rejects_invalid_without_partial_write() {
    let (engine, _storage, dir) = engine_with_db("keyword-seed-invalid").await;

    let err = engine
        .keyword_seed(&["工作压力".to_string(), "   ".to_string()])
        .await
        .expect_err("空白词条应报错");
    assert_eq!(err.category(), "validation");

    let view = engine.keyword_list().await.expect("列表应成功");
    assert_eq!(view.total, 0, "整体校验失败不得部分写入");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 别名建议（扫描 → 建议 → 落库 → 裁决）
// =========================================================

/// 直接向关键词镜像注入 token 计数（模拟封存链路的内存镜像累积，不落库）。
fn inject_mirror_tokens(engine: &Engine, texts: &[&str], times: usize) {
    let tokens: Vec<KeywordToken> = texts.iter().filter_map(|t| KeywordToken::new(t)).collect();
    let mirror = engine.keyword_mirror();
    let mut guard = ramaria_core::lock::write_recover(&*mirror, "keyword.tests.mirror");
    for _ in 0..times {
        guard.upsert_pool_tokens(&tokens, 1_700_000_000_000);
    }
}

/// 造若干条 L1 并经增量镜像写入关键词计数（模拟封存链路；词不落 keyword_pool）。
async fn seed_l1_mirror(
    engine: &Engine,
    storage: &ramaria_storage::SqliteStorage,
    persona: &str,
    keywords: &str,
    count: usize,
) {
    for i in 0..count {
        let l1_id = seed_l1(
            storage,
            persona,
            "镜像语料",
            Some(keywords),
            1_700_000_000_000 + i as i64,
        )
        .await;
        let l1 = storage
            .get_memory_l1(l1_id)
            .await
            .expect("读取 L1 应成功")
            .expect("L1 应存在");
        crate::index::index_l1_into_mirrors(engine, &l1).await;
    }
}

/// 端到端：L1 增量镜像产生使用量 → 建议落库为 pending → 列表可读 → 裁决转 alias。
#[tokio::test]
async fn suggest_pending_aliases_end_to_end_then_confirm() {
    let (engine, storage, dir) = engine_with_db("keyword-suggest-e2e").await;
    seed_persona(&storage, "char-0001").await;
    // 工作压力 4 次、职场压力 3 次（相似对，方向由使用量决定）
    seed_l1_mirror(&engine, &storage, "char-0001", "工作压力,职场压力", 3).await;
    seed_l1_mirror(&engine, &storage, "char-0001", "工作压力,加班", 1).await;

    let outcome = engine
        .keyword_suggest_pending_aliases(None)
        .await
        .expect("建议用例应成功");
    assert_eq!(outcome.scanned_tokens, 3, "镜像词条去重后为 3 个");
    assert_eq!(outcome.suggestions, 1);
    assert_eq!(outcome.inserted, 1);
    assert_eq!(outcome.skipped, 0);
    assert_eq!(outcome.truncated, 0);

    // 列表：pending 指向种子注入的规范词
    let pending = engine
        .keyword_pending_aliases()
        .await
        .expect("待确认列表应成功");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].alias, "职场压力");
    assert_eq!(pending[0].canonical, "工作压力");

    // 词条视图：规范词（种子形态）+ pending 携带观测计数
    let view = engine.keyword_list().await.expect("列表应成功");
    assert_eq!(view.canonical_count, 1);
    assert_eq!(view.pending_count, 1);
    let alias_entry = view
        .keywords
        .iter()
        .find(|k| k.keyword == "职场压力")
        .expect("pending 词条应存在");
    assert_eq!(alias_entry.use_count, 3, "pending 携带观测计数");
    assert_eq!(alias_entry.canonical_keyword.as_deref(), Some("工作压力"));

    // 裁决：confirm → alias（三态计数正确）
    let resolved = engine
        .keyword_resolve_alias(AliasResolveRequest {
            alias: "职场压力".to_string(),
            action: AliasAction::Confirm,
            already_applied_ok: false,
        })
        .await
        .expect("确认合并应成功");
    assert_eq!(resolved.status, "alias");
    assert_eq!(resolved.canonical_keyword.as_deref(), Some("工作压力"));

    let view = engine.keyword_list().await.expect("列表应成功");
    assert_eq!(view.canonical_count, 1);
    assert_eq!(view.alias_count, 1);
    assert_eq!(view.pending_count, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 幂等：重复运行不重复登记（已存在词条不被改写），待确认项数量与指向不变。
#[tokio::test]
async fn suggest_pending_aliases_rerun_is_idempotent() {
    let (engine, storage, dir) = engine_with_db("keyword-suggest-idempotent").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1_mirror(&engine, &storage, "char-0001", "工作压力,职场压力", 3).await;
    seed_l1_mirror(&engine, &storage, "char-0001", "工作压力,加班", 1).await;

    let first = engine
        .keyword_suggest_pending_aliases(None)
        .await
        .expect("首次建议应成功");
    assert_eq!(first.inserted, 1);

    let second = engine
        .keyword_suggest_pending_aliases(None)
        .await
        .expect("重复建议应成功");
    assert_eq!(second.inserted, 0, "重复运行不得新增待确认项");
    assert!(second.skipped >= 1, "已存在词条应计入跳过");

    let pending = engine
        .keyword_pending_aliases()
        .await
        .expect("待确认列表应成功");
    assert_eq!(pending.len(), 1, "待确认项数量不变");
    assert_eq!(pending[0].canonical, "工作压力", "既有指向不被改写");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 高阈值：低于 min_use 的词不参与建议（无建议、无写入）。
#[tokio::test]
async fn suggest_pending_aliases_respects_min_use() {
    let (engine, storage, dir) = engine_with_db("keyword-suggest-min-use").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1_mirror(&engine, &storage, "char-0001", "工作压力,职场压力", 3).await;

    let outcome = engine
        .keyword_suggest_pending_aliases(Some(100))
        .await
        .expect("建议用例应成功");
    assert_eq!(outcome.suggestions, 0, "高阈值下无建议");
    assert_eq!(outcome.inserted, 0);
    assert!(
        engine
            .keyword_pending_aliases()
            .await
            .expect("待确认列表应成功")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 已建立词表：相似词都已是规范词时不产生建议（已登记词条不重复登记）。
#[tokio::test]
async fn suggest_pending_aliases_skips_established_canonicals() {
    let (engine, storage, dir) = engine_with_db("keyword-suggest-established").await;
    for _ in 0..3 {
        storage
            .upsert_keyword("工作压力")
            .await
            .expect("写入规范词应成功");
        storage
            .upsert_keyword("职场压力")
            .await
            .expect("写入规范词应成功");
    }

    let outcome = engine
        .keyword_suggest_pending_aliases(None)
        .await
        .expect("建议用例应成功");
    assert_eq!(outcome.scanned_tokens, 2);
    assert_eq!(outcome.suggestions, 0, "已登记规范词不参与建议");
    assert_eq!(outcome.inserted, 0);
    assert!(
        engine
            .keyword_pending_aliases()
            .await
            .expect("待确认列表应成功")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 目标处于待确认状态：不重复登记、不做反向改指（跳过）。
#[tokio::test]
async fn suggest_pending_aliases_skips_non_canonical_target() {
    let (engine, _storage, dir) = engine_with_db("keyword-suggest-non-canonical").await;
    let pool = open_pool(&dir).await;
    // 词池：工作压力（canonical）+ 职场压力（pending 指向它）
    seed_pending_aliases(&pool, &["职场压力"]).await;
    // 镜像：职场压力 3 次 + 职场倦怠 3 次（后者为未登记相似词）
    inject_mirror_tokens(&engine, &["职场压力"], 3);
    inject_mirror_tokens(&engine, &["职场倦怠"], 3);

    let outcome = engine
        .keyword_suggest_pending_aliases(None)
        .await
        .expect("建议用例应成功");
    assert_eq!(outcome.inserted, 0, "目标非规范词时不得登记");
    assert!(outcome.skipped >= 1);

    // 既有 pending 指向不变
    let pending = engine
        .keyword_pending_aliases()
        .await
        .expect("待确认列表应成功");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].alias, "职场压力");
    assert_eq!(pending[0].canonical, "工作压力");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 单次上限：相似对超过上限时只登记前 N 条，其余计入 truncated。
#[tokio::test]
async fn suggest_pending_aliases_caps_per_run() {
    let (engine, _storage, dir) = engine_with_db("keyword-suggest-cap").await;

    // 55 组三字词：组内共享全部字符（相似），组间字符唯一（编辑距离 3，不相似）
    let mut canonical_side: Vec<String> = Vec::new();
    let mut alias_side: Vec<String> = Vec::new();
    for i in 0..55u32 {
        let base = 0x4E00 + i * 3;
        let first = char::from_u32(base).expect("码点合法");
        let second = char::from_u32(base + 1).expect("码点合法");
        let third = char::from_u32(base + 2).expect("码点合法");
        canonical_side.push(format!("{first}{second}{third}"));
        alias_side.push(format!("{second}{third}{first}"));
    }
    let canonical_refs: Vec<&str> = canonical_side.iter().map(String::as_str).collect();
    let alias_refs: Vec<&str> = alias_side.iter().map(String::as_str).collect();
    inject_mirror_tokens(&engine, &canonical_refs, 5);
    inject_mirror_tokens(&engine, &alias_refs, 3);

    let outcome = engine
        .keyword_suggest_pending_aliases(None)
        .await
        .expect("建议用例应成功");
    assert_eq!(outcome.suggestions, 55, "每组产出一对相似词");
    assert_eq!(outcome.inserted, 50, "单次登记上限 50");
    assert_eq!(outcome.truncated, 5, "超出上限的 5 条本轮不写");
    assert!(outcome.inserted <= 50, "本轮写入不得超过上限");

    assert_eq!(
        engine
            .keyword_pending_aliases()
            .await
            .expect("待确认列表应成功")
            .len(),
        50,
        "落库 pending 不超过上限"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
