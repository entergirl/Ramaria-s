//! crates/ramaria-eval/src/run/context/tests.rs - Ramaria 全量上下文单元测试
//!
//! 设计特点:
//! - 真实迁移库：按 schema 插入人格 / 会话 / L1，不写任何 DDL
//! - 时间升序与预算裁剪各一条，裁剪锁定"从最早丢弃、至少留一条"
//! - 空库返回固定占位行，调用方无需区分空集合
//! - 分页用例覆盖单页上限之外的多页拼接
//! - Windows 上先关连接池再删目录，避免句柄占用

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_service::Engine;
use uuid::Uuid;

use super::*;

const PERSONA: &str = "lifebench-user-1";

// =========================================================
// 夹具
// =========================================================

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-context-{tag}-{nanos}"))
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

async fn close(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
}

/// 插入人格、会话与若干 L1（summary, occurred_at, created_at）。
async fn seed(engine: &Engine, rows: &[(String, i64, i64)]) {
    let pool = engine.sqlite_pool().expect("评测引擎应带连接池");
    sqlx::query(
        "INSERT OR IGNORE INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES (?1, ?2, 'char', 1, 'local', 0, 0)",
    )
    .bind(PERSONA)
    .bind(PERSONA)
    .execute(&pool)
    .await
    .expect("人格夹具应可写");

    let session_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO sessions (id, started_at, channel) VALUES (?1, 0, 'local')")
        .bind(&session_id)
        .execute(&pool)
        .await
        .expect("会话夹具应可写");

    for (summary, occurred_at, created_at) in rows {
        sqlx::query(
            "INSERT INTO memory_l1 (id, session_id, summary, created_at, occurred_at, persona_uid) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&session_id)
        .bind(summary)
        .bind(created_at)
        .bind(occurred_at)
        .bind(PERSONA)
        .execute(&pool)
        .await
        .expect("L1 夹具应可写");
    }
}

// =========================================================
// 渲染
// =========================================================

/// 条目按发生时间升序渲染并重新编号。
#[tokio::test]
async fn context_orders_by_occurred_at() {
    let root = temp_root("order");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");
    let rows = vec![
        ("第一".to_string(), 100, 100),
        ("第三".to_string(), 300, 300),
        ("第二".to_string(), 200, 200),
    ];
    seed(&engine, &rows).await;

    let context = build_full_context(&engine, PERSONA, usize::MAX)
        .await
        .expect("装配上下文应成功");
    assert_eq!(
        context,
        "[相关记忆]\n1. (L1) 第一\n2. (L1) 第二\n3. (L1) 第三"
    );

    close(engine).await;
    cleanup(&root);
}

/// 同发生时间按创建时间升序，保证渲染可复现。
#[tokio::test]
async fn context_breaks_ties_by_created_at() {
    let root = temp_root("tie");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");
    let rows = vec![
        ("晚创建".to_string(), 100, 200),
        ("早创建".to_string(), 100, 100),
    ];
    seed(&engine, &rows).await;

    let context = build_full_context(&engine, PERSONA, usize::MAX)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context, "[相关记忆]\n1. (L1) 早创建\n2. (L1) 晚创建");

    close(engine).await;
    cleanup(&root);
}

/// 超预算从最早条目开始丢弃，保留最近条目。
#[tokio::test]
async fn context_drops_earliest_when_over_budget() {
    let root = temp_root("budget");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");
    let rows = vec![
        ("甲".to_string(), 100, 100),
        ("乙".to_string(), 200, 200),
        ("丙".to_string(), 300, 300),
    ];
    seed(&engine, &rows).await;

    let two_lines = "[相关记忆]\n1. (L1) 乙\n2. (L1) 丙".to_string();
    let budget = two_lines.chars().count() + 1;
    let context = build_full_context(&engine, PERSONA, budget)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context, two_lines, "应只保留最近两条并重新编号");
    assert!(!context.contains("甲"), "最早条目应先被丢弃");

    // 预算为零时仍保留最近一条（单条超预算也保留）。
    let context = build_full_context(&engine, PERSONA, 0)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context, "[相关记忆]\n1. (L1) 丙");

    close(engine).await;
    cleanup(&root);
}

/// 空人格返回固定占位行。
#[tokio::test]
async fn context_reports_empty_memory() {
    let root = temp_root("empty");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");

    let context = build_full_context(&engine, PERSONA, 800)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context, "[相关记忆]\n（无）");

    close(engine).await;
    cleanup(&root);
}

/// 超过单页上限的 L1 需跨页取全。
#[tokio::test]
async fn context_pages_through_all_rows() {
    let root = temp_root("pages");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");
    let total = PAGE_SIZE as usize + 5;
    let rows: Vec<(String, i64, i64)> = (0..total)
        .map(|index| (format!("摘要{index}"), index as i64, index as i64))
        .collect();
    seed(&engine, &rows).await;

    let context = build_full_context(&engine, PERSONA, usize::MAX)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context.lines().count(), total + 1, "应拼接所有分页");
    assert!(context.contains(&format!("{total}. (L1) 摘要{}", total - 1)));

    close(engine).await;
    cleanup(&root);
}
