//! crates/ramaria-eval/src/run/oracle_context/tests.rs - Ramaria 压缩层 oracle 上下文单元测试
//!
//! 设计特点:
//! - 真实迁移库：按 schema 插入人格 / 会话 / 消息 / L1 区间，不写任何 DDL
//! - L1 覆盖范围由 start_msg_id / end_msg_id 决定，与线上证据展开口径一致
//! - 排序、去重、预算裁剪与空 gold 各一条，全部落在渲染结果上
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
    std::env::temp_dir().join(format!("ramaria-eval-oracle-{tag}-{nanos}"))
}

async fn close(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

async fn with_engine(tag: &str) -> (PathBuf, Engine) {
    let root = temp_root(tag);
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");
    (root, engine)
}

/// 一天的摘要与它覆盖的消息下标区间（闭区间）。
struct Seed<'a> {
    summary: &'a str,
    occurred_at: i64,
    span: (usize, usize),
}

/// 插入人格、一个会话、四条消息与若干 L1 摘要；返回消息 id 列表。
async fn seed(engine: &Engine, rows: &[Seed<'_>]) -> Vec<String> {
    let pool = engine.sqlite_pool().expect("评测引擎应带连接池");
    sqlx::query(
        "INSERT OR IGNORE INTO personas (uid, name, kind, seq, source, created_at, updated_at) VALUES (?1, ?2, 'char', 1, 'local', 0, 0)",
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

    let mut messages = Vec::new();
    for index in 0..4i64 {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, created_at, source) VALUES (?1, ?2, 'user', ?3, ?4, 'local')",
        )
        .bind(&id)
        .bind(&session_id)
        .bind(format!("消息{index}"))
        .bind(index)
        .execute(&pool)
        .await
        .expect("消息夹具应可写");
        messages.push(id);
    }

    for row in rows {
        sqlx::query(
            "INSERT INTO memory_l1 (id, session_id, summary, created_at, occurred_at, persona_uid, start_msg_id, end_msg_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&session_id)
        .bind(row.summary)
        .bind(row.occurred_at)
        .bind(row.occurred_at)
        .bind(PERSONA)
        .bind(&messages[row.span.0])
        .bind(&messages[row.span.1])
        .execute(&pool)
        .await
        .expect("L1 夹具应可写");
    }
    messages
}

// =========================================================
// 渲染
// =========================================================

/// 命中的 L1 按发生时间升序渲染，同一 L1 被多条 gold 消息命中只出现一次。
#[tokio::test]
async fn render_orders_by_occurred_at_and_dedupes() {
    let (root, engine) = with_engine("order").await;
    let messages = seed(
        &engine,
        &[
            Seed {
                summary: "甲",
                occurred_at: 300,
                span: (0, 1),
            },
            Seed {
                summary: "乙",
                occurred_at: 200,
                span: (2, 3),
            },
        ],
    )
    .await;

    let mut cache = OracleCache::default();
    let gold = vec![
        messages[0].clone(),
        messages[2].clone(),
        messages[3].clone(),
    ];
    let context = cache
        .context(&engine, &root, PERSONA, &gold, usize::MAX)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context, "[相关记忆]\n1. (L1) 乙\n2. (L1) 甲");

    close(engine).await;
    cleanup(&root);
}

/// 超预算从最早的命中条目开始丢弃，保留最近。
#[tokio::test]
async fn render_drops_earliest_over_budget() {
    let (root, engine) = with_engine("budget").await;
    let messages = seed(
        &engine,
        &[
            Seed {
                summary: "甲",
                occurred_at: 100,
                span: (0, 0),
            },
            Seed {
                summary: "乙",
                occurred_at: 200,
                span: (1, 1),
            },
        ],
    )
    .await;

    let mut cache = OracleCache::default();
    let gold = vec![messages[0].clone(), messages[1].clone()];
    let one_line = "[相关记忆]\n1. (L1) 乙".to_string();
    let budget = one_line.chars().count() + 1;
    let context = cache
        .context(&engine, &root, PERSONA, &gold, budget)
        .await
        .expect("装配上下文应成功");
    assert_eq!(context, one_line, "应只保留最近的命中条目并重新编号");

    close(engine).await;
    cleanup(&root);
}

/// 空 gold 返回空串（不建索引、不注入）。
#[tokio::test]
async fn context_returns_empty_without_gold() {
    let (root, engine) = with_engine("empty").await;
    seed(
        &engine,
        &[Seed {
            summary: "甲",
            occurred_at: 100,
            span: (0, 0),
        }],
    )
    .await;

    let mut cache = OracleCache::default();
    let context = cache
        .context(&engine, &root, PERSONA, &[], 800)
        .await
        .expect("空 gold 不应失败");
    assert_eq!(context, "");

    close(engine).await;
    cleanup(&root);
}
