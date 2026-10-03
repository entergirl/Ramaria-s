//! crates/ramaria-storage/src/tests/event.rs - L2 离散事件存储测试
//!
//! 设计特点:
//! - 覆盖事件、关系、来源 CRUD 与事件批次单事务写入（含失败回滚）
//! - 覆盖未吸收事件查询、按 persona 计数与跨用户经验分布聚合
//! - 覆盖 mark_events_absorbed 事务化分批与指定 ID 语义
//! - 共享夹具覆盖 persona + session + L1 上下文与带推断信号的事件造数

use super::*;

#[tokio::test]
async fn memory_event_crud() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let now = now_ms();
    let ev = MemoryEvent::new(
        "user-0001".into(),
        "事件".into(),
        "描述".into(),
        now - 1000,
        now,
    );
    let ev_id = storage.save_event(&ev).await.unwrap();
    assert!(ev_id > 0);

    let events = storage
        .list_events_by_persona("user-0001", 0, 10)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title, "事件");
    assert_eq!(events[0].id, ev_id);
}

#[tokio::test]
async fn event_relation_crud() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let now = now_ms();
    let e1 = MemoryEvent::new("user-0001".into(), "A".into(), "desc".into(), now, now);
    let e2 = MemoryEvent::new("user-0001".into(), "B".into(), "desc".into(), now, now);
    let id1 = storage.save_event(&e1).await.unwrap();
    let id2 = storage.save_event(&e2).await.unwrap();

    let rel = EventRelation::new(id1, id2, EventRelationKind::CausedBy);
    let rel_id = storage.save_event_relation(&rel).await.unwrap();
    assert!(rel_id > 0);
}

#[tokio::test]
async fn event_source_crud() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "摘要".into(), None);
    storage.save_memory_l1(&l1).await.unwrap();
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let now = now_ms();
    let ev = MemoryEvent::new("user-0001".into(), "E".into(), "desc".into(), now, now);
    let ev_id = storage.save_event(&ev).await.unwrap();

    storage.save_event_source(ev_id, l1.id, 1.0).await.unwrap();
}

// =========================================================
// 事件批次单事务写入（save_event_batch）
// =========================================================

/// 正常路径：2 事件 + 来源 + 关系 + absorbed 全量落库；
/// 返回 id 与 events 顺序一一对应。
#[tokio::test]
async fn save_event_batch_all_parts_persisted() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "摘要".into(), None);
    storage.save_memory_l1(&l1).await.unwrap();
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let now = now_ms();
    let e1 = MemoryEvent::new("user-0001".into(), "A".into(), "desc-a".into(), now, now);
    let e2 = MemoryEvent::new("user-0001".into(), "B".into(), "desc-b".into(), now, now);
    let batch = EventBatchWrite {
        events: vec![e1, e2],
        sources: vec![(0, l1.id, 1.0), (1, l1.id, 0.5)],
        relations: vec![(0, 1, EventRelationKind::CausedBy, 0.8)],
        absorbed_l1_ids: vec![l1.id],
    };

    let ids = storage.save_event_batch(&batch).await.unwrap();
    assert_eq!(ids.len(), 2, "返回 id 数应与事件数一致");
    assert!(ids[0] > 0 && ids[1] > 0 && ids[0] != ids[1]);

    // 事件落库且 id 与输入顺序对应
    let events = storage
        .list_events_by_persona("user-0001", 0, 10)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    let a = events
        .iter()
        .find(|e| e.title == "A")
        .expect("事件 A 应落库");
    let b = events
        .iter()
        .find(|e| e.title == "B")
        .expect("事件 B 应落库");
    assert_eq!(a.id, ids[0], "返回 id 应与 events 顺序对应");
    assert_eq!(b.id, ids[1], "返回 id 应与 events 顺序对应");

    // 来源落库
    let sources_a = storage.list_event_sources_by_event(a.id).await.unwrap();
    assert_eq!(sources_a.len(), 1);
    assert_eq!(sources_a[0].l1_id, l1.id);
    assert!((sources_a[0].weight - 1.0).abs() < 1e-9);
    let sources_b = storage.list_event_sources_by_event(b.id).await.unwrap();
    assert_eq!(sources_b.len(), 1);
    assert!((sources_b[0].weight - 0.5).abs() < 1e-9);

    // 关系落库
    let relations = storage
        .list_event_relations_by_persona("user-0001")
        .await
        .unwrap();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].from_id, a.id);
    assert_eq!(relations[0].to_id, b.id);
    assert_eq!(relations[0].kind, EventRelationKind::CausedBy);

    // L1 已吸收
    let l1_loaded = storage.get_memory_l1(l1.id).await.unwrap().unwrap();
    assert!(l1_loaded.absorbed, "L1 应被批次事务标记为已吸收");
}

/// 失败回滚：sources 下标越界 → 整体回滚（事件不落库、L1 不吸收）。
#[tokio::test]
async fn save_event_batch_rolls_back_on_out_of_range_source() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "摘要".into(), None);
    storage.save_memory_l1(&l1).await.unwrap();
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let now = now_ms();
    let e1 = MemoryEvent::new("user-0001".into(), "A".into(), "desc".into(), now, now);
    let batch = EventBatchWrite {
        events: vec![e1],
        // 越界：批次仅 1 条事件（下标 0），下标 9 非法
        sources: vec![(9, l1.id, 1.0)],
        relations: vec![],
        absorbed_l1_ids: vec![l1.id],
    };

    let err = storage
        .save_event_batch(&batch)
        .await
        .expect_err("越界下标应报错");
    assert_eq!(err.category(), "validation", "越界应返回 Validation: {err}");

    // 整体回滚：无新事件、L1 未吸收
    let events = storage
        .list_events_by_persona("user-0001", 0, 10)
        .await
        .unwrap();
    assert!(events.is_empty(), "失败批次不应残留事件行");
    let l1_loaded = storage.get_memory_l1(l1.id).await.unwrap().unwrap();
    assert!(!l1_loaded.absorbed, "失败批次不应标记 L1 吸收");
}

/// 空事件 + absorbed 非空 → 只标记吸收、返回空 ids。
#[tokio::test]
async fn save_event_batch_empty_events_marks_l1_only() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "摘要".into(), None);
    storage.save_memory_l1(&l1).await.unwrap();

    let batch = EventBatchWrite {
        events: vec![],
        sources: vec![],
        relations: vec![],
        absorbed_l1_ids: vec![l1.id],
    };
    let ids = storage.save_event_batch(&batch).await.unwrap();
    assert!(ids.is_empty(), "空事件批次应返回空 id 列表");
    let l1_loaded = storage.get_memory_l1(l1.id).await.unwrap().unwrap();
    assert!(l1_loaded.absorbed, "空事件批次仍应标记吸收");
}

/// 事件计数：按 persona 隔离统计，无事件 persona 返回 0。
#[tokio::test]
async fn count_events_by_persona_counts_scoped() {
    let storage = setup().await;
    let p = Persona::new(
        "char-count".into(),
        "计数角色".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    for i in 0_i64..3 {
        let ev = MemoryEvent::new(
            "char-count".to_string(),
            format!("事件{i}"),
            "摘要".to_string(),
            1_000 + i,
            2_000 + i,
        );
        storage.save_event(&ev).await.unwrap();
    }

    assert_eq!(
        storage.count_events_by_persona("char-count").await.unwrap(),
        3
    );
    assert_eq!(
        storage.count_events_by_persona("char-other").await.unwrap(),
        0,
        "无事件 persona 计数应为 0"
    );
}

// =========================================================
// list_unabsorbed_events & update_persona 补充测试
// =========================================================

/// 辅助：创建 MemoryEvent 并关联到 persona。
async fn create_test_event(storage: &SqliteStorage, persona_uid: &str, title: &str) -> i64 {
    let now = now_ms();
    let ev = MemoryEvent::new(
        persona_uid.into(),
        title.into(),
        "测试描述".into(),
        now - 1000,
        now,
    );
    storage.save_event(&ev).await.unwrap()
}

/// 辅助：创建带推断信号属性的事件（valence/share/presentation 可设）。
async fn create_event_with_signals(
    storage: &SqliteStorage,
    persona_uid: &str,
    title: &str,
    valence: f64,
    share: f64,
    presentation: &str,
) -> i64 {
    use ramaria_core::types::Presentation;
    let now = now_ms();
    let mut ev = MemoryEvent::new(
        persona_uid.into(),
        title.into(),
        "测试描述".into(),
        now - 1000,
        now,
    );
    ev.valence = valence;
    ev.share = share;
    ev.presentation = match presentation {
        "objective" => Presentation::Objective,
        "subjective" => Presentation::Subjective,
        _ => Presentation::Mixed,
    };
    storage.save_event(&ev).await.unwrap()
}

#[tokio::test]
async fn list_unabsorbed_events_empty() {
    // 新建 persona 尚未有任何事件
    let (storage, persona_uid, _, _) = setup_with_persona().await;

    let events = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
    assert!(events.is_empty(), "新 persona 应该没有未吸收事件");
}

// =========================================================
// aggregate_persona_event_priors（跨用户事件经验分布聚合）
// =========================================================

/// 辅助：创建其他 persona（返回其 uid）。
async fn create_extra_persona(storage: &SqliteStorage, uid: &str, name: &str) {
    let p = Persona::new(
        uid.into(),
        name.into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
}

/// 多 persona 场景：聚合返回除目标 persona 外各 persona 的 n/均值/占比。
#[tokio::test]
async fn aggregate_persona_event_priors_multiple_personas() {
    let (storage, target_uid, _, _) = setup_with_persona().await;
    create_extra_persona(&storage, "char-a", "角色A").await;
    create_extra_persona(&storage, "char-b", "角色B").await;

    // 目标 persona 自身事件（应被 exclude，不污染跨用户先验）
    create_event_with_signals(&storage, &target_uid, "目标自身事件", 0.8, 0.9, "objective").await;

    // char-a: 2 条（valence 0.2 / -0.4，share 0.6），presentation objective + subjective
    create_event_with_signals(&storage, "char-a", "A1", 0.2, 0.6, "objective").await;
    create_event_with_signals(&storage, "char-a", "A2", -0.4, 0.6, "subjective").await;

    // char-b: 3 条（valence 全部 0.5，share 0.3），presentation 全部 mixed
    for i in 0..3 {
        create_event_with_signals(&storage, "char-b", &format!("B{i}"), 0.5, 0.3, "mixed").await;
    }

    let rows = storage
        .aggregate_persona_event_priors(&target_uid)
        .await
        .unwrap();

    // 排除目标 persona：仅返回 char-a / char-b
    assert_eq!(rows.len(), 2, "应只聚合其他 persona：{rows:?}");
    assert!(
        rows.iter().all(|r| r.persona_uid != target_uid),
        "目标 persona 自身事件不得进入聚合结果"
    );

    let char_a = rows
        .iter()
        .find(|r| r.persona_uid == "char-a")
        .expect("应包含 char-a");
    assert_eq!(char_a.n_events, 2);
    assert!(
        (char_a.valence_mean - (0.2 + -0.4) / 2.0).abs() < 1e-9,
        "char-a valence 应为事件级均值，实际={}",
        char_a.valence_mean
    );
    assert!((char_a.share_mean - 0.6).abs() < 1e-9);
    assert!((char_a.obj_ratio - 0.5).abs() < 1e-9);
    assert!((char_a.sub_ratio - 0.5).abs() < 1e-9);
    assert!((char_a.mix_ratio - 0.0).abs() < 1e-9);

    let char_b = rows
        .iter()
        .find(|r| r.persona_uid == "char-b")
        .expect("应包含 char-b");
    assert_eq!(char_b.n_events, 3);
    assert!((char_b.valence_mean - 0.5).abs() < 1e-9);
    assert!((char_b.share_mean - 0.3).abs() < 1e-9);
    assert!((char_b.obj_ratio - 0.0).abs() < 1e-9);
    assert!((char_b.sub_ratio - 0.0).abs() < 1e-9);
    assert!((char_b.mix_ratio - 1.0).abs() < 1e-9);

    // 三态占比和恒为 1
    for row in &rows {
        let sum = row.obj_ratio + row.sub_ratio + row.mix_ratio;
        assert!(
            (sum - 1.0).abs() < 1e-9,
            "presentation 占比和应为1: {row:?}"
        );
    }
}

/// 空库（无任何事件）→ 返回空列表。
#[tokio::test]
async fn aggregate_persona_event_priors_empty_db_returns_empty() {
    let storage = setup().await;
    let rows = storage
        .aggregate_persona_event_priors("user-none")
        .await
        .unwrap();
    assert!(rows.is_empty(), "空库应返回空聚合结果");
}

/// 目标 persona 是系统内唯一有事件者 → 无其他 persona 来源，返回空。
#[tokio::test]
async fn aggregate_persona_event_priors_only_target_returns_empty() {
    let (storage, target_uid, _, _) = setup_with_persona().await;
    create_event_with_signals(&storage, &target_uid, "唯一事件", 0.1, 0.5, "mixed").await;

    let rows = storage
        .aggregate_persona_event_priors(&target_uid)
        .await
        .unwrap();
    assert!(rows.is_empty(), "仅目标 persona 有事件时不应产生跨用户来源");
}

#[tokio::test]
async fn list_unabsorbed_events_some() {
    let (storage, persona_uid, _, _) = setup_with_persona().await;

    // 创建 3 个事件
    let id1 = create_test_event(&storage, &persona_uid, "事件A").await;
    let id2 = create_test_event(&storage, &persona_uid, "事件B").await;
    let id3 = create_test_event(&storage, &persona_uid, "事件C").await;

    let events = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
    assert_eq!(events.len(), 3, "应返回全部 3 个未吸收事件");
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    assert!(ids.contains(&id1));
    assert!(ids.contains(&id2));
    assert!(ids.contains(&id3));
}

#[tokio::test]
async fn list_unabsorbed_events_only_matching_persona() {
    let (storage, persona_uid, _, _) = setup_with_persona().await;

    // 创建第二个 persona
    let p2 = Persona::new(
        "char-test".into(),
        "角色".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p2).await.unwrap();

    create_test_event(&storage, &persona_uid, "用户事件").await;
    create_test_event(&storage, "char-test", "角色事件").await;

    let events = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
    assert_eq!(events.len(), 1, "只应返回 user-test 的事件");
    assert_eq!(events[0].title, "用户事件");
}

// =========================================================
// mark_events_absorbed 事务化测试（v1.7 决策 D-V17-014-23）
// =========================================================
// 与 L1 版 mark_absorbed 对齐为事务化执行（杜绝事件半吸收），
// 批次边界与指定 ID 语义测试覆盖行为一致性。

#[tokio::test]
async fn mark_events_absorbed_empty_slice_is_noop() {
    let (storage, persona_uid, _, _) = setup_with_persona().await;
    let _id = create_test_event(&storage, &persona_uid, "事件A").await;

    storage.mark_events_absorbed(&[]).await.unwrap();

    let remaining = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
    assert_eq!(remaining.len(), 1, "空切片不应吸收任何事件");
}

#[tokio::test]
async fn mark_events_absorbed_batch_boundary_transactional() {
    // 101 条事件跨批次（100 + 1）在单事务中全部吸收（无半吸收）
    let (storage, persona_uid, _, _) = setup_with_persona().await;
    let mut ids = Vec::new();
    for i in 0..101 {
        ids.push(create_test_event(&storage, &persona_uid, &format!("事件{i}")).await);
    }

    storage.mark_events_absorbed(&ids).await.unwrap();

    let remaining = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
    assert!(
        remaining.is_empty(),
        "101 条跨批次应全部吸收（事务保证无半吸收），实际剩余: {}",
        remaining.len()
    );
}

#[tokio::test]
async fn mark_events_absorbed_only_absorbs_specified_ids() {
    // 仅指定事件被吸收，未指定的不受影响
    let (storage, persona_uid, _, _) = setup_with_persona().await;
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(create_test_event(&storage, &persona_uid, &format!("事件{i}")).await);
    }

    // 只吸收前 3 条
    storage.mark_events_absorbed(&ids[..3]).await.unwrap();

    let remaining = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
    assert_eq!(remaining.len(), 2, "应剩余 2 条未吸收");
}

/// 事件→所属会话映射：多来源取权重最高者；无来源 / 脏数据缺失；空输入空映射。
#[tokio::test]
async fn event_session_map_selects_highest_weight_source() {
    let storage = setup().await;
    // 事件 persona 外键要求存在对应人格
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let session_high = storage.create_session(None).await.unwrap();
    let session_low = storage.create_session(None).await.unwrap();
    let l1_high = MemoryL1::new(session_high.id, "高权来源".into(), None);
    let l1_low = MemoryL1::new(session_low.id, "低权来源".into(), None);
    storage.save_memory_l1(&l1_high).await.unwrap();
    storage.save_memory_l1(&l1_low).await.unwrap();

    let now = now_ms();
    let ev = MemoryEvent::new("user-0001".into(), "跟进".into(), "desc".into(), now, now);
    let ev_id = storage.save_event(&ev).await.unwrap();
    let orphan = MemoryEvent::new("user-0001".into(), "无来源".into(), "desc".into(), now, now);
    let orphan_id = storage.save_event(&orphan).await.unwrap();

    // 同一事件两条来源：低权先写、高权后写（验证选择不受写入顺序影响）
    storage
        .save_event_source(ev_id, l1_low.id, 0.2)
        .await
        .unwrap();
    storage
        .save_event_source(ev_id, l1_high.id, 0.9)
        .await
        .unwrap();

    // 脏数据：所属会话主键非 UUID（解析失败应跳过，不阻塞整批映射）
    let dirty = MemoryEvent::new("user-0001".into(), "脏".into(), "desc".into(), now, now);
    let dirty_id = storage.save_event(&dirty).await.unwrap();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES ('not-a-uuid-session', 0)")
        .execute(&storage.pool)
        .await
        .unwrap();
    let dirty_l1_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO memory_l1 (id, session_id, summary, created_at) \
         VALUES (?, 'not-a-uuid-session', '脏数据', 0)",
    )
    .bind(&dirty_l1_id)
    .execute(&storage.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO event_sources (event_id, l1_id, weight) VALUES (?, ?, 1.0)")
        .bind(dirty_id)
        .bind(&dirty_l1_id)
        .execute(&storage.pool)
        .await
        .unwrap();

    let map = storage
        .list_event_session_map(&[ev_id, orphan_id, dirty_id])
        .await
        .unwrap();
    assert_eq!(
        map.get(&ev_id),
        Some(&session_high.id),
        "同一事件应取权重最高来源所属会话"
    );
    assert!(!map.contains_key(&orphan_id), "无来源事件不应出现在映射中");
    assert!(
        !map.contains_key(&dirty_id),
        "会话 UUID 解析失败的事件不应出现在映射中"
    );
    assert!(
        storage
            .list_event_session_map(&[])
            .await
            .unwrap()
            .is_empty(),
        "空输入应返回空映射"
    );
}
