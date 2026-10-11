//! crates/ramaria-service/src/browse/tests/l1.rs - Ramaria L1 记忆浏览用例
//!
//! 设计特点:
//! - 直查口径（persona 过滤 / 无 persona 合并 / 分页与取回收敛）与未吸收口径分列
//! - 会话窗口回归：会话数超过旧扫描上限时，最旧会话的摘要仍可见
//! - 使用真实 SQLite 临时库；断言以浏览视图与落库状态为准

use crate::engine::Engine;
use crate::test_support::{engine_with_db, seed_l1, seed_persona};
use crate::types::L1BrowseRequest;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::MemoryL1;
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

/// L1 直查口径：persona 过滤 + 创建时间倒序 + limit 截断；无 persona 时合并全人格。
#[tokio::test]
async fn l1_desktop_scope_filters_orders_and_truncates() {
    let (engine, storage, dir) = engine_with_db("browse-l1-desktop").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    seed_l1(&storage, "char-0001", "摘要 A", Some("考试"), 1_000).await;
    seed_l1(&storage, "char-0002", "摘要 B", None, 2_000).await;
    seed_l1(&storage, "char-0001", "摘要 C", None, 3_000).await;

    // persona 过滤：只含 char-0001 的两条，创建时间倒序
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: false,
            limit: None,
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(page.total, 2);
    let summaries: Vec<&str> = page.items.iter().map(|m| m.summary.as_str()).collect();
    assert_eq!(summaries, vec!["摘要 C", "摘要 A"], "应按创建时间倒序");
    assert!(
        page.items
            .iter()
            .all(|m| m.persona_uid.as_deref() == Some("char-0001"))
    );
    let a_item = page
        .items
        .iter()
        .find(|m| m.summary == "摘要 A")
        .expect("摘要 A 应出现");
    assert_eq!(a_item.keywords.as_deref(), Some("考试"), "伴随字段应透传");

    // 无 persona：三条全含
    let all = engine
        .memory_l1(L1BrowseRequest {
            persona: None,
            unabsorbed_only: false,
            limit: None,
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(all.total, 3);
    let summaries: Vec<&str> = all.items.iter().map(|m| m.summary.as_str()).collect();
    assert_eq!(summaries, vec!["摘要 C", "摘要 B", "摘要 A"]);

    // limit 截断：取最新一条
    let limited = engine
        .memory_l1(L1BrowseRequest {
            persona: None,
            unabsorbed_only: false,
            limit: Some(1),
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(limited.items.len(), 1);
    assert_eq!(limited.items[0].summary, "摘要 C");

    let _ = std::fs::remove_dir_all(&dir);
}

/// L1 未吸收口径：分页 + total 为分页前条数；persona 缺省报业务校验错误。
#[tokio::test]
async fn l1_unabsorbed_scope_paginates() {
    let (engine, storage, dir) = engine_with_db("browse-l1-unabsorbed").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "摘要 1", None, 1_000).await;
    let absorbed = seed_l1(&storage, "char-0001", "摘要 2", None, 2_000).await;
    seed_l1(&storage, "char-0001", "摘要 3", None, 3_000).await;
    storage
        .mark_l1_absorbed(&[absorbed])
        .await
        .expect("标记吸收应成功");

    // 全量：未吸收 2 条（按创建时间升序取回）
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: true,
            limit: None,
            offset: None,
        })
        .await
        .expect("未吸收浏览应成功");
    assert_eq!(page.total, 2, "total 为分页前条数");
    let summaries: Vec<&str> = page.items.iter().map(|m| m.summary.as_str()).collect();
    assert_eq!(summaries, vec!["摘要 1", "摘要 3"]);

    // 分页：offset 1 + limit 1 → 第二条
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: true,
            limit: Some(1),
            offset: Some(1),
        })
        .await
        .expect("未吸收浏览应成功");
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].summary, "摘要 3");

    // persona 缺省：业务校验错误
    let err = engine
        .memory_l1(L1BrowseRequest {
            persona: None,
            unabsorbed_only: true,
            limit: None,
            offset: None,
        })
        .await
        .expect_err("未吸收口径需 persona");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 新建会话并把开始时间改为指定值（会话新旧顺序与扫描窗口回归用）。
async fn seed_session_at(
    engine: &Engine,
    storage: &SqliteStorage,
    persona: &str,
    started_at: i64,
) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    let pool = engine.sqlite_pool().expect("测试引擎应附着连接池");
    sqlx::query("UPDATE sessions SET started_at = ? WHERE id = ?")
        .bind(started_at)
        .bind(session.id.to_string())
        .execute(&pool)
        .await
        .expect("更新会话开始时间应成功");
    session.id
}

/// 回归（浏览口径缺陷）：会话数超过修复前的会话扫描上限时，最旧会话的 L1 仍可见。
///
/// 背景：早期口径按"最近 N 个会话"逐个读取（N = 500），导入长会话文件时
/// L1 按时间从旧到新生成，最先产出的摘要恰好落在扫描窗口之外而不可见。
#[tokio::test]
async fn l1_persona_scope_reads_old_session_beyond_former_scan_window() {
    let (engine, storage, dir) = engine_with_db("browse-l1-window").await;
    seed_persona(&storage, "char-0001").await;

    // 目标 L1 挂在最旧会话上
    let oldest = seed_session_at(&engine, &storage, "char-0001", 1_000).await;
    let mut l1 = MemoryL1::new(oldest, "早期摘要".to_string(), None);
    l1.persona_uid = Some("char-0001".to_string());
    l1.created_at = 1_000;
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    // 500 个更新会话（修复前扫描窗口大小）
    for index in 0..500_i64 {
        seed_session_at(&engine, &storage, "char-0001", 2_000 + index).await;
    }

    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: false,
            limit: None,
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(page.items.len(), 1, "最旧会话的 L1 应可见");
    assert_eq!(page.items[0].summary, "早期摘要");
    assert_eq!(page.items[0].session_id, oldest);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 直查口径：分页与取回收敛（offset 跳过最新、limit 截断、total 按取回条数）。
#[tokio::test]
async fn l1_persona_scope_paginates_by_created_at() {
    let (engine, storage, dir) = engine_with_db("browse-l1-paginate").await;
    seed_persona(&storage, "char-0001").await;
    for i in 0..3_i64 {
        seed_l1(
            &storage,
            "char-0001",
            &format!("摘要{i}"),
            None,
            1_000 + i * 10,
        )
        .await;
    }

    // limit=1 + offset=1 → 跳过最新一条
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: false,
            limit: Some(1),
            offset: Some(1),
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].summary, "摘要1");
    assert_eq!(page.total, 3, "取回条数（offset + limit + 1 内）应覆盖全量");

    let _ = std::fs::remove_dir_all(&dir);
}
