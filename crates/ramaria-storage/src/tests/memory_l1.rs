//! crates/ramaria-storage/src/tests/memory_l1.rs - L1 会话摘要存储测试
//!
//! 设计特点:
//! - 覆盖 L1 摘要写入与读取
//! - 覆盖 mark_l1_absorbed 分批边界（100 / 101 / 200 条）与指定 ID 语义
//! - 覆盖 touch_l1 访问时间刷新的空切片与指定 ID 边界
//! - 覆盖 occurred_at（发生时间）读写往返、按发生时间排序与存量回填语义

use super::*;

#[tokio::test]
async fn memory_l1_crud() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "摘要".into(), Some("上午".into()));
    storage.save_memory_l1(&l1).await.unwrap();

    let list = storage.list_memory_l1(session.id).await.unwrap();
    assert_eq!(list.len(), 1);
}

// =========================================================
// mark_absorbed 批次边界测试
// =========================================================
// 验证 BATCH_SIZE=100 的分批逻辑在所有边界条件下正确工作。
// 由于 mark_absorbed 内部以 100 条为单位分批，需要确保:
// - 恰好 100 条 → 单批次
// - 101 条 → 两个批次（100 + 1）
// - 200 条 → 两个批次（100 + 100）

/// 创建 N 条 L1 记忆并返回它们的 ID 列表。
async fn create_n_l1(
    storage: &SqliteStorage,
    session_id: uuid::Uuid,
    persona_uid: &str,
    n: usize,
) -> Vec<uuid::Uuid> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let l1 = MemoryL1::new(
            session_id,
            format!("测试摘要 #{i}"),
            Some(format!("时段-{i}")),
        );
        // 手动设置 persona_uid（MemoryL1::new 不支持该字段）
        let mut l1_with_persona = l1;
        // 通过直接构造覆盖 persona_uid 字段
        // MemoryL1 结构体的字段为 pub，可以直接赋值
        l1_with_persona.persona_uid = Some(persona_uid.to_string());
        storage.save_memory_l1(&l1_with_persona).await.unwrap();
        ids.push(l1_with_persona.id);
    }
    ids
}

/// 辅助：创建 persona + session 用于 mark_absorbed 测试。
async fn setup_for_absorb() -> (SqliteStorage, String, uuid::Uuid) {
    let storage = setup().await;
    let persona_uid = "absorb-test".to_string();
    let p = Persona::new(
        persona_uid.clone(),
        "吸收测试".into(),
        PersonaKind::User,
        100,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let session = storage.create_session(None).await.unwrap();
    (storage, persona_uid, session.id)
}

#[tokio::test]
async fn mark_absorbed_empty_slice_is_noop() {
    // 空切片应直接返回 Ok，不产生错误
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let _l1_ids = create_n_l1(&storage, session_id, &persona_uid, 3).await;

    // 标记空切片，应成功
    storage.mark_l1_absorbed(&[]).await.unwrap();

    // 原有记录应仍未吸收
    let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
    assert_eq!(remaining.len(), 3);
}

#[tokio::test]
async fn mark_absorbed_single_item() {
    // 单条记录吸收
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 1).await;
    let target = &[l1_ids[0]];

    storage.mark_l1_absorbed(target).await.unwrap();

    let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
    assert!(
        remaining.is_empty(),
        "吸收后应无未吸收记录，实际: {remaining:?}"
    );
}

#[tokio::test]
async fn mark_absorbed_exactly_100_items() {
    // 恰好 100 条（单批次边界值，BATCH_SIZE = 100）
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 100).await;

    storage.mark_l1_absorbed(&l1_ids).await.unwrap();

    let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
    assert!(
        remaining.is_empty(),
        "100 条应全部吸收，实际剩余: {}",
        remaining.len()
    );
}

#[tokio::test]
async fn mark_absorbed_101_items_crosses_batch_boundary() {
    // 101 条，跨越批次边界（100 + 1），验证事务中多批次原子性
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 101).await;

    storage.mark_l1_absorbed(&l1_ids).await.unwrap();

    let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
    assert!(
        remaining.is_empty(),
        "101 条跨批次应全部吸收，实际剩余: {}",
        remaining.len()
    );
}

#[tokio::test]
async fn mark_absorbed_200_items_two_full_batches() {
    // 200 条，恰好两个完整批次（100 + 100）
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 200).await;

    storage.mark_l1_absorbed(&l1_ids).await.unwrap();

    let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
    assert!(
        remaining.is_empty(),
        "200 条应全部吸收，实际剩余: {}",
        remaining.len()
    );
}

#[tokio::test]
async fn mark_absorbed_only_absorbs_specified_ids() {
    // 仅指定 ID 被吸收，未指定的不受影响
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 5).await;

    // 只吸收前 3 条
    storage.mark_l1_absorbed(&l1_ids[..3]).await.unwrap();

    let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
    assert_eq!(remaining.len(), 2, "应剩余 2 条未吸收");
}

// =========================================================
// touch_l1 访问时间刷新测试（v1.7 touch 接线，决策 D-V17-006）
// =========================================================

#[tokio::test]
async fn touch_l1_updates_last_accessed_at() {
    // 检索命中后 touch_l1 应刷新 last_accessed_at（激活 recent_boost_*）
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 2).await;

    let before = storage.get_memory_l1(l1_ids[0]).await.unwrap().unwrap();
    assert!(before.last_accessed_at.is_none(), "初始应无访问时间");

    let now = now_ms();
    storage.touch_l1(&l1_ids, now).await.unwrap();

    for id in &l1_ids {
        let l1 = storage.get_memory_l1(*id).await.unwrap().unwrap();
        assert_eq!(
            l1.last_accessed_at,
            Some(now),
            "touch 后 last_accessed_at 应刷新为 now"
        );
    }
}

#[tokio::test]
async fn touch_l1_empty_slice_is_noop() {
    // 空列表应直接成功（不产生错误、不影响既有记录）
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 3).await;

    storage.touch_l1(&[], now_ms()).await.unwrap();

    for id in &l1_ids {
        let l1 = storage.get_memory_l1(*id).await.unwrap().unwrap();
        assert!(l1.last_accessed_at.is_none(), "空 touch 不应改动访问时间");
    }
}

#[tokio::test]
async fn touch_l1_only_updates_specified_ids() {
    // 仅指定 ID 被刷新，未指定的保持原值
    let (storage, persona_uid, session_id) = setup_for_absorb().await;
    let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 3).await;

    let now = now_ms();
    storage.touch_l1(&l1_ids[..2], now).await.unwrap();

    let touched = storage.get_memory_l1(l1_ids[0]).await.unwrap().unwrap();
    assert_eq!(touched.last_accessed_at, Some(now), "前 2 条应被刷新");
    let untouched = storage.get_memory_l1(l1_ids[2]).await.unwrap().unwrap();
    assert!(untouched.last_accessed_at.is_none(), "未指定 ID 不应被刷新");
}

// =========================================================
// occurred_at（发生时间）读写与排序
// =========================================================

/// occurred_at 往返：Some 值 save → get 保留；None 往返为 None（读取回退 created_at）。
#[tokio::test]
async fn occurred_at_roundtrip() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();

    // Some 值往返
    let mut l1 = MemoryL1::new(session.id, "摘要".into(), None);
    l1.occurred_at = Some(1_700_000_000_000);
    storage.save_memory_l1(&l1).await.unwrap();
    let got = storage.get_memory_l1(l1.id).await.unwrap().unwrap();
    assert_eq!(
        got.occurred_at,
        Some(1_700_000_000_000),
        "occurred_at 应往返保留"
    );
    assert_eq!(got.occurred_at_or_created(), 1_700_000_000_000);

    // None 往返（存量未回填）
    let l1b = MemoryL1::new(session.id, "摘要2".into(), None);
    storage.save_memory_l1(&l1b).await.unwrap();
    let got_b = storage.get_memory_l1(l1b.id).await.unwrap().unwrap();
    assert_eq!(got_b.occurred_at, None, "None 应往返为 None");
    assert_eq!(
        got_b.occurred_at_or_created(),
        got_b.created_at,
        "读取回退 created_at"
    );
}

/// 排序对抗：A（发生早 / 入库晚）、B（发生晚 / 入库早）、C（未回填 → 回退入库时间）。
///
/// - `list_recent_l1_by_persona`（DESC）→ [B, C, A]
/// - `list_memory_l1`（会话内 ASC）→ [A, C, B]
/// - `list_unabsorbed_l1`（ASC）→ [A, C, B]
#[tokio::test]
async fn occurred_at_ordering_overrides_created_at() {
    let storage = setup().await;
    let p = Persona::new(
        "order-uid".into(),
        "排序".into(),
        PersonaKind::User,
        2,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let session = storage.create_session(None).await.unwrap();

    // A：发生早（1000）、入库晚（9000）
    let mut a = MemoryL1::new(session.id, "A".into(), None);
    a.persona_uid = Some("order-uid".into());
    a.occurred_at = Some(1000);
    a.created_at = 9000;
    storage.save_memory_l1(&a).await.unwrap();

    // B：发生晚（8000）、入库早（2000）
    let mut b = MemoryL1::new(session.id, "B".into(), None);
    b.persona_uid = Some("order-uid".into());
    b.occurred_at = Some(8000);
    b.created_at = 2000;
    storage.save_memory_l1(&b).await.unwrap();

    // C：未回填（NULL）→ 回退入库时间（5000）
    let mut c = MemoryL1::new(session.id, "C".into(), None);
    c.persona_uid = Some("order-uid".into());
    c.created_at = 5000;
    storage.save_memory_l1(&c).await.unwrap();

    let recent = storage
        .list_recent_l1_by_persona("order-uid", 10)
        .await
        .unwrap();
    let ids: Vec<&str> = recent.iter().map(|l| l.summary.as_str()).collect();
    assert_eq!(
        ids,
        vec!["B", "C", "A"],
        "最近查询按发生时间 DESC（C 回退）"
    );

    let by_session = storage.list_memory_l1(session.id).await.unwrap();
    let ids: Vec<&str> = by_session.iter().map(|l| l.summary.as_str()).collect();
    assert_eq!(ids, vec!["A", "C", "B"], "会话内按发生时间 ASC（C 回退）");

    let unabsorbed = storage.list_unabsorbed_l1("order-uid").await.unwrap();
    let ids: Vec<&str> = unabsorbed.iter().map(|l| l.summary.as_str()).collect();
    assert_eq!(ids, vec!["A", "C", "B"], "未吸收按发生时间 ASC（C 回退）");
}

// =========================================================
// 存量回填语义
// =========================================================

/// 与 20261012_v2.8_time_semantics.sql 回填语句一致的 L1 回填 SQL。
const BACKFILL_L1_SQL: &str = "UPDATE memory_l1 SET occurred_at = (
    SELECT MIN(m.created_at) FROM messages m WHERE m.session_id = memory_l1.session_id
) WHERE occurred_at IS NULL AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = memory_l1.session_id)";

/// 与 20261012_v2.8_time_semantics.sql 回填语句一致的 L2 回填 SQL。
const BACKFILL_EVENTS_SQL: &str = "UPDATE memory_events SET
  start = COALESCE((SELECT MIN(l.occurred_at) FROM event_sources s JOIN memory_l1 l ON l.id = s.l1_id WHERE s.event_id = memory_events.id), start),
  \"end\" = COALESCE((SELECT MAX(l.occurred_at) FROM event_sources s JOIN memory_l1 l ON l.id = s.l1_id WHERE s.event_id = memory_events.id), \"end\")
WHERE EXISTS (SELECT 1 FROM event_sources s WHERE s.event_id = memory_events.id)";

/// 执行与 migration 一致的两条回填语句（L1 → 事件）。
async fn run_backfill(pool: &sqlx::SqlitePool) {
    sqlx::query(BACKFILL_L1_SQL).execute(pool).await.unwrap();
    sqlx::query(BACKFILL_EVENTS_SQL)
        .execute(pool)
        .await
        .unwrap();
}

/// 回填：L1 按会话最早消息推导；事件 start/end 回归源 L1 发生时间 min/max；
/// 幂等可重跑；无来源事件保持原值。
#[tokio::test]
async fn occurred_at_backfill_semantics() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();

    // 双消息（最早 1000）+ L1（occurred 未回填）
    for ts in [3000_i64, 1000] {
        let mut msg = Message::new(
            session.id,
            MessageRole::User,
            format!("消息-{ts}"),
            MessageSource::Local,
        );
        msg.created_at = ts;
        storage.save_message(&msg).await.unwrap();
    }
    let mut l1 = MemoryL1::new(session.id, "旧摘要".into(), None);
    l1.created_at = 9999;
    storage.save_memory_l1(&l1).await.unwrap();

    // 另一条已带发生时间的 L1（模拟已回填/新写入），用于验证事件 end 取 max
    let mut l1_late = MemoryL1::new(session.id, "已回填摘要".into(), None);
    l1_late.occurred_at = Some(7000);
    storage.save_memory_l1(&l1_late).await.unwrap();

    // 事件一：双来源；事件二：无来源（降级路径残留）
    let p = Persona::new(
        "bf-uid".into(),
        "回填".into(),
        PersonaKind::User,
        3,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let ev = MemoryEvent::new("bf-uid".into(), "来源事件".into(), "desc".into(), 0, 0);
    let ev_id = storage.save_event(&ev).await.unwrap();
    storage.save_event_source(ev_id, l1.id, 1.0).await.unwrap();
    storage
        .save_event_source(ev_id, l1_late.id, 1.0)
        .await
        .unwrap();
    let orphan = MemoryEvent::new("bf-uid".into(), "无来源事件".into(), "desc".into(), 42, 43);
    let orphan_id = storage.save_event(&orphan).await.unwrap();

    // 执行与 migration 一致的回填
    run_backfill(&storage.pool).await;

    // L1 回填 = 会话最早消息时间
    let l1_back = storage.get_memory_l1(l1.id).await.unwrap().unwrap();
    assert_eq!(l1_back.occurred_at, Some(1000), "L1 回填为最早消息时间");

    // 事件 start/end = 源 L1 发生时间 min/max
    let events = storage
        .list_events_by_persona("bf-uid", 0, 10)
        .await
        .unwrap();
    let by_id: std::collections::HashMap<i64, &MemoryEvent> =
        events.iter().map(|e| (e.id, e)).collect();
    let ev_back = by_id.get(&ev_id).expect("来源事件应存在");
    assert_eq!(ev_back.start, 1000, "事件 start 取源 L1 发生时间 min");
    assert_eq!(ev_back.end, 7000, "事件 end 取源 L1 发生时间 max");

    // 无来源事件不动
    let orphan_back = by_id.get(&orphan_id).expect("无来源事件应存在");
    assert_eq!(orphan_back.start, 42, "无来源事件 start 保持原值");
    assert_eq!(orphan_back.end, 43, "无来源事件 end 保持原值");

    // 幂等可重跑：值不变
    run_backfill(&storage.pool).await;
    let l1_again = storage.get_memory_l1(l1.id).await.unwrap().unwrap();
    assert_eq!(l1_again.occurred_at, Some(1000), "重跑 L1 回填幂等");
    let ev_again = storage
        .list_events_by_persona("bf-uid", 0, 10)
        .await
        .unwrap();
    let ev_again = ev_again.iter().find(|e| e.id == ev_id).unwrap();
    assert_eq!(
        (ev_again.start, ev_again.end),
        (1000, 7000),
        "重跑事件回填幂等"
    );
}
