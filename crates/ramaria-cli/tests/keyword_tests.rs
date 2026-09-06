//! tests/keyword_tests.rs - `ramaria keyword` 命令契约测试
//!
//! 覆盖:
//! - list: 空库空列表（JSON 信封）/ seed 后可见（含状态与指向规范词）
//! - seed: 幂等（已存在不递增 use_count / 已存在 alias 保持不动）
//! - show: 存在返回详情 / 不存在报业务校验错误（exit 4 语义由 main 处理，此处断言错误类型）
//! - alias list / confirm / reject: 三态流转（pending → alias / pending → canonical）
//! - confirm 不存在 / 非 pending: 业务校验错误
//! - 进程级: 不存在 show 的 JSON 错误信封 code=4；seed+list 端到端可见
//! - 指向性: seed 词条可被词典增强分词消费（BigramWithDictionaryNormalizer 命中）
//!
//! 安全约束:
//! - keyword 命令直接访问 SQLite（repo::keyword），测试使用临时文件库（init_pool 自动迁移），
//!   不触碰 MockStorage / 真实 LLM。
//! - 不访问 OS keychain、不连网。

use ramaria_core::error::RamariaError;
use ramaria_core::keyword::KeywordToken;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use ramaria_cli::commands::keyword_cmd::{AliasAction, KeywordCmd, run};
use ramaria_storage::repo::keyword as kw_repo;

// =========================================================
// 辅助函数
// =========================================================

/// 临时库序号（并行测试线程安全：subsec_nanos 可能同秒撞车，追加原子计数保证唯一）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建唯一临时测试库目录。
fn temp_db_path(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-kw-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("kw.db")
}

/// 新建真实 SQLite 连接池（空库，自动执行 migration）。
async fn setup_pool() -> SqlitePool {
    let db = temp_db_path("func");
    ramaria_storage::database::init_pool(Some(db))
        .await
        .expect("初始化测试数据库失败")
}

/// 预置 canonical + pending 别名（职场焦虑 → 工作压力）。
///
/// 返回 pending 别名 rowid。
async fn seed_pending(pool: &SqlitePool) -> i64 {
    kw_repo::upsert_with_alias(
        pool,
        &KeywordToken::new("工作压力").unwrap(),
        0,
        "canonical",
    )
    .await
    .unwrap();
    let canonical_id =
        sqlx::query_scalar::<_, i64>("SELECT rowid FROM keyword_pool WHERE keyword = '工作压力'")
            .fetch_one(pool)
            .await
            .unwrap();
    kw_repo::upsert_with_alias(
        pool,
        &KeywordToken::new("职场焦虑").unwrap(),
        canonical_id,
        "pending",
    )
    .await
    .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT rowid FROM keyword_pool WHERE keyword = '职场焦虑'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 断言错误为业务校验失败（RamariaError::Validation，exit 4 语义由 main 映射）。
fn assert_validation(err: &anyhow::Error) {
    assert!(
        matches!(
            err.downcast_ref::<RamariaError>(),
            Some(RamariaError::Validation { .. })
        ),
        "应为业务校验错误，实际: {err:#}"
    );
}

/// 从池中读取词条文本集合（供断言 DB 落库结果）。
async fn entry_texts(pool: &SqlitePool) -> Vec<String> {
    kw_repo::list_entries(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.keyword)
        .collect()
}

// =========================================================
// list / seed / show（函数级）
// =========================================================

#[tokio::test]
async fn keyword_list_empty_json_ok() {
    let pool = setup_pool().await;
    let result = run(&pool, KeywordCmd::List, true, false).await;
    assert!(result.is_ok(), "空库 list --json 应成功输出空列表");
}

#[tokio::test]
async fn keyword_show_missing_is_validation_error() {
    let pool = setup_pool().await;
    let result = run(
        &pool,
        KeywordCmd::Show {
            keyword: "不存在的词".into(),
        },
        true,
        false,
    )
    .await;
    let err = result.expect_err("不存在词条 show 应报错");
    assert_validation(&err);
}

#[tokio::test]
async fn keyword_show_invalid_text_is_validation_error() {
    let pool = setup_pool().await;
    let result = run(
        &pool,
        KeywordCmd::Show {
            keyword: "   ".into(),
        },
        false,
        false,
    )
    .await;
    let err = result.expect_err("纯空白词条应报业务校验错误");
    assert_validation(&err);
}

#[tokio::test]
async fn keyword_seed_then_list_shows_entry() {
    let pool = setup_pool().await;
    // seed 两个规范词
    run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["工作压力".into(), "爬山".into()],
        },
        true,
        false,
    )
    .await
    .expect("seed 应成功");

    // DB 落库：两个规范词、use_count=0、canonical 形态
    let entries = kw_repo::list_entries(&pool).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries.iter().all(|e| e.use_count == 0),
        "种子词 use_count 为 0"
    );
    assert!(
        entries.iter().all(|e| e.alias_status.is_none()),
        "种子词为规范词"
    );
    assert!(entries.iter().all(|e| e.canonical_id.is_none()));

    // list --json 亦成功
    run(&pool, KeywordCmd::List, true, false)
        .await
        .expect("seed 后 list 应成功");
}

#[tokio::test]
async fn keyword_seed_idempotent_preserves_use_count() {
    let pool = setup_pool().await;
    // 先手工种子，再自然 upsert 两次（use_count=2）
    run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["工作压力".into()],
        },
        false,
        false,
    )
    .await
    .expect("首次 seed 成功");
    kw_repo::upsert(&pool, &KeywordToken::new("工作压力").unwrap())
        .await
        .unwrap();
    kw_repo::upsert(&pool, &KeywordToken::new("工作压力").unwrap())
        .await
        .unwrap();

    // 再次 seed：幂等，不递增 use_count
    run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["工作压力".into()],
        },
        false,
        false,
    )
    .await
    .expect("重复 seed 应成功（幂等）");
    let entries = kw_repo::list_entries(&pool).await.unwrap();
    assert_eq!(entries.len(), 1, "重复 seed 不应新增行");
    assert_eq!(entries[0].use_count, 2, "重复 seed 不得递增 use_count");
}

#[tokio::test]
async fn keyword_seed_keeps_existing_pending_untouched() {
    let pool = setup_pool().await;
    let alias_id = seed_pending(&pool).await;

    // 对 pending 别名词条执行 seed：保持状态不动、不递增 use_count
    run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["职场焦虑".into()],
        },
        false,
        false,
    )
    .await
    .expect("对已存在 pending 词条 seed 应成功（幂等提示）");

    let entries = kw_repo::list_entries(&pool).await.unwrap();
    let pending = entries
        .iter()
        .find(|e| e.keyword == "职场焦虑")
        .expect("pending 词条应保留");
    assert_eq!(
        pending.alias_status.as_deref(),
        Some("pending"),
        "状态不得被改写"
    );
    assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));
    assert_eq!(pending.use_count, 1, "seed 不得递增 pending 词条 use_count");
    let _ = alias_id;
}

#[tokio::test]
async fn keyword_seed_invalid_any_rejected_without_partial_write() {
    let pool = setup_pool().await;
    // 任一无效输入整体拒绝（不部分写入）
    let result = run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["合法词".into(), "   ".into()],
        },
        false,
        false,
    )
    .await;
    let err = result.expect_err("含无效关键词应整体拒绝");
    assert_validation(&err);
    assert!(
        entry_texts(&pool).await.is_empty(),
        "部分写入不应发生（整体校验失败）"
    );
}

// =========================================================
// alias list / confirm / reject（函数级）
// =========================================================

#[tokio::test]
async fn keyword_alias_confirm_flow() {
    let pool = setup_pool().await;
    seed_pending(&pool).await;

    // alias list 可见待确认冲突
    run(&pool, KeywordCmd::Alias(AliasAction::List), true, false)
        .await
        .expect("alias list --json 应成功");

    // confirm（--yes 自动确认）
    run(
        &pool,
        KeywordCmd::Alias(AliasAction::Confirm {
            alias: "职场焦虑".into(),
        }),
        true,
        true,
    )
    .await
    .expect("confirm --yes 应成功");

    // pending 清空，词条转为 alias（canonical_id 指向工作压力）
    assert!(
        kw_repo::list_pending_aliases(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    let entries = kw_repo::list_entries(&pool).await.unwrap();
    let alias = entries
        .iter()
        .find(|e| e.keyword == "职场焦虑")
        .expect("词条应保留");
    assert_eq!(alias.alias_status.as_deref(), Some("alias"));
    assert_eq!(alias.canonical_keyword.as_deref(), Some("工作压力"));
}

#[tokio::test]
async fn keyword_alias_reject_flow() {
    let pool = setup_pool().await;
    seed_pending(&pool).await;

    run(
        &pool,
        KeywordCmd::Alias(AliasAction::Reject {
            alias: "职场焦虑".into(),
        }),
        true,
        true,
    )
    .await
    .expect("reject --yes 应成功");

    // pending 清空，词条晋升为独立规范词（canonical 指向解除）
    assert!(
        kw_repo::list_pending_aliases(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    let entries = kw_repo::list_entries(&pool).await.unwrap();
    let promoted = entries
        .iter()
        .find(|e| e.keyword == "职场焦虑")
        .expect("词条应保留");
    assert_eq!(promoted.alias_status, None, "reject 后应清除 alias_status");
    assert_eq!(promoted.canonical_id, None, "reject 后应解除规范词指向");
    assert_eq!(promoted.canonical_keyword, None);
    assert_eq!(promoted.use_count, 1, "reject 保留原 use_count");
}

#[tokio::test]
async fn keyword_alias_confirm_nonexistent_is_validation_error() {
    let pool = setup_pool().await;
    let result = run(
        &pool,
        KeywordCmd::Alias(AliasAction::Confirm {
            alias: "不存在的词".into(),
        }),
        false,
        true,
    )
    .await;
    let err = result.expect_err("不存在词条 confirm 应报错");
    assert_validation(&err);
}

#[tokio::test]
async fn keyword_alias_confirm_non_pending_is_validation_error() {
    let pool = setup_pool().await;
    // 仅注入规范词（canonical，非 pending）
    run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["工作压力".into()],
        },
        false,
        false,
    )
    .await
    .expect("seed 应成功");

    let result = run(
        &pool,
        KeywordCmd::Alias(AliasAction::Confirm {
            alias: "工作压力".into(),
        }),
        false,
        true,
    )
    .await;
    let err = result.expect_err("对非 pending 词条 confirm 应报错");
    assert_validation(&err);
}

#[tokio::test]
async fn keyword_alias_reject_nonexistent_is_validation_error() {
    let pool = setup_pool().await;
    let result = run(
        &pool,
        KeywordCmd::Alias(AliasAction::Reject {
            alias: "不存在的词".into(),
        }),
        false,
        true,
    )
    .await;
    let err = result.expect_err("不存在词条 reject 应报错");
    assert_validation(&err);
}

// =========================================================
// 指向性：seed 词条可被词典增强分词消费
// =========================================================

#[tokio::test]
async fn keyword_seed_feeds_dictionary_segmentation() {
    let pool = setup_pool().await;
    // seed "工作压力" → 成为 keyword_pool 规范词
    run(
        &pool,
        KeywordCmd::Seed {
            keywords: vec!["工作压力".into()],
        },
        false,
        false,
    )
    .await
    .expect("seed 应成功");

    // 装载路径（与 BM25 词典一致）：keyword_pool 规范词 → BigramWithDictionaryNormalizer
    let canonicals = kw_repo::list_canonicals(&pool).await.unwrap();
    let dict: Vec<String> = canonicals.iter().map(|r| r.keyword.clone()).collect();
    assert!(dict.contains(&"工作压力".to_string()));

    use ramaria_memory::keyword::KeywordNormalizer;
    let normalizer =
        ramaria_memory::keyword::BigramWithDictionaryNormalizer::from_dictionary(&dict);
    let tokens = normalizer.normalize("最近工作压力很大");
    let texts: Vec<&str> = tokens.iter().map(|t| t.as_str()).collect();
    assert!(
        texts.contains(&"工作压力"),
        "词典增强分词应命中 seed 的规范词，实际: {texts:?}"
    );
    assert!(
        !texts.contains(&"作压"),
        "词典命中后不应产生跨词噪声 bigram"
    );
}

// =========================================================
// 进程级 CLI 契约（真实二进制 + 临时 DB）
// =========================================================

static DB_SEQ: AtomicU32 = AtomicU32::new(0);

/// 以真实二进制运行 CLI（同一批次内共享 DB，支持 seed → list 两段式验证）。
fn run_cli_shared(args: &[&str], db: &std::path::Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(db)
        .output()
        .expect("运行 ramaria 二进制失败")
}

/// 创建唯一的共享 DB 路径（跨多次进程调用保留）。
fn shared_db(tag: &str) -> std::path::PathBuf {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let db_dir = std::env::temp_dir().join(format!(
        "ramaria_cli_keyword_{}_{}_{}",
        std::process::id(),
        seq,
        tag
    ));
    std::fs::create_dir_all(&db_dir).unwrap();
    db_dir.join("kw.db")
}

/// seed + list 端到端：真实二进制写库后 list --json 可见。
#[test]
fn keyword_process_seed_then_list_end_to_end() {
    let db = shared_db("e2e");
    let seed = run_cli_shared(&["keyword", "seed", "工作压力", "--yes"], &db);
    assert_eq!(seed.status.code(), Some(0), "seed 应成功退出: {:?}", seed);
    let stderr = String::from_utf8_lossy(&seed.stderr);
    assert!(
        stderr.contains("已注入"),
        "seed 提示应输出到 stderr: {stderr}"
    );

    let list = run_cli_shared(&["keyword", "list", "--json"], &db);
    assert_eq!(list.status.code(), Some(0), "list --json 应成功退出");
    let stdout = String::from_utf8_lossy(&list.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON 信封: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["total"], 1);
    assert_eq!(parsed["data"]["keywords"][0]["keyword"], "工作压力");
    assert_eq!(parsed["data"]["keywords"][0]["status"], "canonical");
    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}

/// show 不存在的词条：--json 错误信封 + exit 4（业务校验失败）。
#[test]
fn keyword_process_show_missing_validation_exit4() {
    let db = shared_db("show");
    let out = run_cli_shared(&["keyword", "show", "不存在的词", "--json"], &db);
    assert_eq!(out.status.code(), Some(4), "不存在词条 show 应 exit 4");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], 4);
    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}

/// 非 TTY 且无 --yes：alias confirm 不挂起、直接失败并提示 --yes。
#[test]
fn keyword_process_confirm_non_tty_without_yes_fails() {
    let db = shared_db("confirm");
    // 预置 canonical + pending 别名（复用 repo 需异步——用独立 tokio runtime）
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = ramaria_storage::database::init_pool(Some(db.clone()))
            .await
            .expect("初始化 DB 失败");
        seed_pending(&pool).await;
        pool.close().await;
    });

    let out = run_cli_shared(&["keyword", "alias", "confirm", "职场焦虑"], &db);
    // 非 TTY 无 --yes → 业务校验失败 exit 4（与既有 session delete 契约一致）
    assert_eq!(
        out.status.code(),
        Some(4),
        "非 TTY 无 --yes 应失败（不挂起）"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "提示应包含 --yes: {stderr}");

    // 确认失败不写库：词条仍为 pending
    rt.block_on(async {
        let pool = ramaria_storage::database::init_pool(Some(db.clone()))
            .await
            .expect("初始化 DB 失败");
        let pending = kw_repo::list_pending_aliases(&pool).await.unwrap();
        assert_eq!(pending.len(), 1, "确认失败不得改库");
        pool.close().await;
    });
    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}

/// 非 TTY + --yes：alias confirm 自动通过并落库。
#[test]
fn keyword_process_confirm_with_yes_succeeds() {
    let db = shared_db("confirm-yes");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = ramaria_storage::database::init_pool(Some(db.clone()))
            .await
            .expect("初始化 DB 失败");
        seed_pending(&pool).await;
        pool.close().await;
    });

    let out = run_cli_shared(
        &["keyword", "alias", "confirm", "职场焦虑", "--yes", "--json"],
        &db,
    );
    assert_eq!(out.status.code(), Some(0), "--yes 应通过确认并成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON 信封: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["status"], "alias");

    rt.block_on(async {
        let pool = ramaria_storage::database::init_pool(Some(db.clone()))
            .await
            .expect("初始化 DB 失败");
        assert!(
            kw_repo::list_pending_aliases(&pool)
                .await
                .unwrap()
                .is_empty(),
            "confirm 后 pending 应清空"
        );
        pool.close().await;
    });
    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}
