//! crates/ramaria-storage/src/repo/events/tests.rs - L2 事件选题查询单元测试
//!
//! 设计特点:
//! - 覆盖显著性门槛查询的阈值 / 时间窗闭区间边界、排序与截断、persona 隔离
//! - 覆盖时间窗查询的闭区间边界、排序与截断、persona 隔离
//! - 直接以 SQL 插入 persona fixture，事件经 repo 保存接口落库
//! - 排序口径按 salience / end / id 的多级降序逐项断言

use super::*;
use crate::database::init_test_pool;

/// 插入 persona fixture（memory_events.persona_uid 外键要求）。
async fn setup_fixture(pool: &SqlitePool, persona_uid: &str) {
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES (?, '测试', 'char', 1, 'local', 0, 0)",
    )
    .bind(persona_uid)
    .execute(pool)
    .await
    .expect("插入 persona fixture 应成功");
}

/// 写入一条事件（指定 salience 与 end），返回自增 id。
async fn insert_event(
    pool: &SqlitePool,
    persona_uid: &str,
    title: &str,
    salience: f64,
    end: i64,
) -> i64 {
    let mut ev = MemoryEvent::new(
        persona_uid.to_string(),
        title.to_string(),
        "摘要".to_string(),
        end - 1_000,
        end,
    );
    ev.salience = salience;
    save_event(pool, &ev).await.expect("写入事件应成功")
}

/// list_events_by_salience：阈值闭区间、时间窗闭区间、persona 隔离、空数据。
#[tokio::test]
async fn list_events_by_salience_filters_threshold_window_and_persona() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    setup_fixture(&pool, "char-0001").await;
    setup_fixture(&pool, "char-0002").await;

    // 空数据：无事件 → 空列表
    assert!(
        list_events_by_salience(&pool, "char-0001", 0, 0.6, 10)
            .await
            .expect("查询应成功")
            .is_empty(),
        "无事件 persona 应返回空列表"
    );

    // salience 恰等于门槛、end 恰等于窗口下界 → 双边界均入选（闭区间）
    let at_boundaries = insert_event(&pool, "char-0001", "恰等门槛与窗口", 0.6, 5_000).await;
    let below_threshold = insert_event(&pool, "char-0001", "低于门槛", 0.59, 6_000).await;
    let before_window = insert_event(&pool, "char-0001", "窗口外", 0.9, 4_999).await;
    let high = insert_event(&pool, "char-0001", "高显著", 0.8, 7_000).await;
    let other_persona = insert_event(&pool, "char-0002", "他人事件", 1.0, 8_000).await;

    let events = list_events_by_salience(&pool, "char-0001", 5_000, 0.6, 10)
        .await
        .expect("查询应成功");
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![high, at_boundaries],
        "应仅含窗内且达门槛的事件（salience 降序）"
    );
    assert!(!ids.contains(&below_threshold), "低于门槛的事件不应入选");
    assert!(!ids.contains(&before_window), "窗口外的事件不应入选");
    assert!(!ids.contains(&other_persona), "其他 persona 的事件不应混入");
}

/// list_events_by_salience：salience 降序、同分按 end 降序、再按 id 降序；limit 截断。
#[tokio::test]
async fn list_events_by_salience_orders_and_limits() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    setup_fixture(&pool, "char-0001").await;

    // salience 同为 0.7 的三条（其中两条 end 相同）+ 一条 0.9
    let same_lower_end = insert_event(&pool, "char-0001", "同分早", 0.7, 1_000).await;
    let same_higher_end_first = insert_event(&pool, "char-0001", "同分晚先写", 0.7, 3_000).await;
    let same_higher_end_second = insert_event(&pool, "char-0001", "同分晚后写", 0.7, 3_000).await;
    let highest = insert_event(&pool, "char-0001", "最高", 0.9, 1_000).await;

    let events = list_events_by_salience(&pool, "char-0001", 0, 0.6, 10)
        .await
        .expect("查询应成功");
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![
            highest,
            same_higher_end_second,
            same_higher_end_first,
            same_lower_end
        ],
        "应为 salience 降序 → end 降序 → id 降序"
    );

    // limit 截断：仅取前 2 条
    let limited = list_events_by_salience(&pool, "char-0001", 0, 0.6, 2)
        .await
        .expect("查询应成功");
    assert_eq!(limited.len(), 2, "limit 应截断返回条数");
    assert_eq!(limited[0].id, highest);
    assert_eq!(limited[1].id, same_higher_end_second);

    // limit = 0 → 空
    assert!(
        list_events_by_salience(&pool, "char-0001", 0, 0.6, 0)
            .await
            .expect("查询应成功")
            .is_empty(),
        "limit 0 应返回空列表"
    );
}

/// list_events_since：时间窗闭区间、end 降序（同分 id 降序）、persona 隔离、limit 截断。
#[tokio::test]
async fn list_events_since_filters_window_orders_and_limits() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    setup_fixture(&pool, "char-0001").await;
    setup_fixture(&pool, "char-0002").await;

    // 空数据：无事件 → 空列表
    assert!(
        list_events_since(&pool, "char-0001", 0, 10)
            .await
            .expect("查询应成功")
            .is_empty(),
        "无事件 persona 应返回空列表"
    );

    // end 恰等于窗口下界 → 入选（闭区间）
    let at_boundary = insert_event(&pool, "char-0001", "恰等窗口", 0.1, 2_000).await;
    let before_boundary = insert_event(&pool, "char-0001", "窗口外", 0.9, 1_999).await;
    let same_end_first = insert_event(&pool, "char-0001", "同刻先写", 0.5, 4_000).await;
    let same_end_second = insert_event(&pool, "char-0001", "同刻后写", 0.5, 4_000).await;
    let other_persona = insert_event(&pool, "char-0002", "他人事件", 0.9, 5_000).await;

    let events = list_events_since(&pool, "char-0001", 2_000, 10)
        .await
        .expect("查询应成功");
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![same_end_second, same_end_first, at_boundary],
        "应仅含窗内事件（end 降序、同分 id 降序）"
    );
    assert!(!ids.contains(&before_boundary), "窗口外的事件不应混入");
    assert!(!ids.contains(&other_persona), "其他 persona 的事件不应混入");

    // limit 截断：仅取最新 1 条
    let limited = list_events_since(&pool, "char-0001", 2_000, 1)
        .await
        .expect("查询应成功");
    assert_eq!(limited.len(), 1, "limit 应截断返回条数");
    assert_eq!(limited[0].id, same_end_second);
}
