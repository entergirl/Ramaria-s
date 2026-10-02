//! crates/ramaria-storage/src/tests/memory_l1.rs - L1 会话摘要存储测试
//!
//! 设计特点:
//! - 覆盖 L1 摘要写入与读取
//! - 覆盖 mark_l1_absorbed 分批边界（100 / 101 / 200 条）与指定 ID 语义
//! - 覆盖 touch_l1 访问时间刷新的空切片与指定 ID 边界

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
