//! crates/ramaria-service/src/browse/tests/units.rs - Ramaria 记忆单元证据导出用例
//!
//! 设计特点:
//! - 四层各一条：L1 直读区间、L2 经事件溯源、L3 经引用与证据、utt 直读块区间
//! - 每条用例断言该层独有的展开路径，不写成同构重复
//! - 覆盖软引用容错（单元不存在、区间未回填）与输入校验（层名 / id 形态）
//! - 使用真实 SQLite 临时库；消息时间戳显式递增，区间与顺序可预期
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use crate::test_support::{engine_with_db, seed_persona};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    EvidenceDirection, MemoryEvent, MemoryL1, Message, MessageRole, MessageSource,
    PersonalityTrait, TraitEvidence, TraitLayer, TraitSource, UttBlock,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

// =========================================================
// 夹具
// =========================================================

/// 造一个会话与 `count` 条消息，返回会话 id 与消息 id（按发送顺序）。
async fn seed_dialog(
    storage: &SqliteStorage,
    persona: &str,
    count: usize,
    base_ts: i64,
) -> (Uuid, Vec<Uuid>) {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let mut message = Message::new(
            session.id,
            MessageRole::User,
            format!("消息内容 {i}"),
            MessageSource::Local,
        )
        .with_persona_uid(Some(persona.to_string()));
        message.created_at = base_ts + i as i64;
        storage
            .save_message(&message)
            .await
            .expect("写入消息应成功");
        ids.push(message.id);
    }
    (session.id, ids)
}

/// 造一条带消息区间的 L1 摘要。
async fn seed_l1_range(
    storage: &SqliteStorage,
    session_id: Uuid,
    persona: &str,
    summary: &str,
    start: Uuid,
    end: Uuid,
) -> Uuid {
    let mut l1 = MemoryL1::new(session_id, summary.to_string(), None);
    l1.persona_uid = Some(persona.to_string());
    l1.start_msg_id = Some(start);
    l1.end_msg_id = Some(end);
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    l1.id
}

/// 造一条事件并挂上指定 L1 来源。
async fn seed_event_with_sources(
    storage: &SqliteStorage,
    persona: &str,
    title: &str,
    start_ms: i64,
    l1_ids: &[Uuid],
) -> i64 {
    let event = MemoryEvent::new(
        persona.to_string(),
        title.to_string(),
        format!("{title}的摘要"),
        start_ms,
        start_ms + 1_000,
    );
    let event_id = storage.save_event(&event).await.expect("写入事件应成功");
    for l1_id in l1_ids {
        storage
            .save_event_source(event_id, *l1_id, 1.0)
            .await
            .expect("写入事件溯源应成功");
    }
    event_id
}

/// 造一条性格标签（可指定直连 L1 与直连事件）。
async fn seed_trait(
    storage: &SqliteStorage,
    persona: &str,
    label: &str,
    ref_l1_id: Option<Uuid>,
    ref_event_id: Option<i64>,
) -> i64 {
    let mut row = PersonalityTrait::new(
        persona.to_string(),
        TraitLayer::Primary,
        label.to_string(),
        format!("{label}的具体含义"),
        TraitSource::Event,
        1,
    );
    row.ref_l1_id = ref_l1_id;
    row.ref_event_id = ref_event_id;
    storage.save_trait(&row).await.expect("写入性格标签应成功")
}

/// 造一条性格证据记录（`created_at` 显式给定，控制证据返回顺序）。
async fn seed_evidence(storage: &SqliteStorage, trait_id: i64, event_id: i64, created_at: i64) {
    let mut evidence = TraitEvidence::new(trait_id, event_id, EvidenceDirection::Support, 0.9);
    evidence.created_at = created_at;
    storage
        .save_evidence(&evidence)
        .await
        .expect("写入证据记录应成功");
}

// =========================================================
// 四层用例
// =========================================================

/// L1 直读自身区间：同一会话存在另一条不重叠的摘要时不纳入，未回填区间贡献为空。
#[tokio::test]
async fn l1_expands_only_its_own_range() {
    let (engine, storage, dir) = engine_with_db("browse-units-l1").await;
    seed_persona(&storage, "char-0001").await;
    let (session_id, msg_ids) = seed_dialog(&storage, "char-0001", 4, 1_000).await;

    let target = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "目标摘要",
        msg_ids[1],
        msg_ids[2],
    )
    .await;
    seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "相邻摘要",
        msg_ids[3],
        msg_ids[3],
    )
    .await;

    let got = engine
        .memory_unit_sources("l1", &target.to_string())
        .await
        .expect("L1 证据导出应成功");
    assert_eq!(
        got,
        vec![msg_ids[1].to_string(), msg_ids[2].to_string()],
        "只覆盖本单元区间，同一会话的其他摘要不纳入"
    );

    // 未回填区间的存量摘要：贡献为空而非报错
    let mut legacy = MemoryL1::new(session_id, "存量摘要".to_string(), None);
    legacy.persona_uid = Some("char-0001".to_string());
    storage
        .save_memory_l1(&legacy)
        .await
        .expect("写入 L1 应成功");
    let empty = engine
        .memory_unit_sources("l1", &legacy.id.to_string())
        .await
        .expect("未回填区间应返回空集合");
    assert!(empty.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// L2 经事件溯源展开：两条来源 L1 的区间取并集，未挂该事件的 L1 不纳入。
#[tokio::test]
async fn l2_expands_event_sources_union() {
    let (engine, storage, dir) = engine_with_db("browse-units-l2").await;
    seed_persona(&storage, "char-0001").await;
    let (session_id, msg_ids) = seed_dialog(&storage, "char-0001", 4, 1_000).await;

    let head = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "前半段",
        msg_ids[0],
        msg_ids[2],
    )
    .await;
    let tail = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "后半段",
        msg_ids[1],
        msg_ids[2],
    )
    .await;
    let unrelated = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "无关摘要",
        msg_ids[3],
        msg_ids[3],
    )
    .await;
    let event_id =
        seed_event_with_sources(&storage, "char-0001", "事件", 1_000, &[head, tail]).await;

    let got = engine
        .memory_unit_sources("l2", &event_id.to_string())
        .await
        .expect("L2 证据导出应成功");

    // 来源之间无固定顺序（event_sources 未定序）：按消息 id 比较集合
    let mut actual = got.clone();
    actual.sort();
    let mut expected: Vec<String> = msg_ids[..3].iter().map(Uuid::to_string).collect();
    expected.sort();
    assert_eq!(actual, expected, "两条来源区间取并集，重叠消息只出现一次");

    // 区间内保持消息时间序：重叠段的首条始终早于末条
    let first_pos = got
        .iter()
        .position(|id| *id == msg_ids[1].to_string())
        .expect("重叠段首条应出现");
    let last_pos = got
        .iter()
        .position(|id| *id == msg_ids[2].to_string())
        .expect("重叠段末条应出现");
    assert!(first_pos < last_pos, "同一区间内按消息时间序追加");

    // 只有经 event_sources 才能到达：未挂载该事件的 L1 所覆盖消息不纳入
    assert!(
        !got.contains(&msg_ids[3].to_string()),
        "未挂载的摘要（{unrelated}）不参与该事件的证据集合"
    );

    // 事件不存在：空集合，不报错
    let missing = engine
        .memory_unit_sources("l2", "999999")
        .await
        .expect("不存在的事件应返回空集合");
    assert!(missing.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// L3 三路径合并：直连 L1、直连事件与证据事件各贡献一段，同一事件只展开一次。
#[tokio::test]
async fn l3_merges_refs_and_evidence_without_duplicates() {
    let (engine, storage, dir) = engine_with_db("browse-units-l3").await;
    seed_persona(&storage, "char-0001").await;
    let (session_id, msg_ids) = seed_dialog(&storage, "char-0001", 6, 1_000).await;

    let direct = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "直连摘要",
        msg_ids[0],
        msg_ids[1],
    )
    .await;
    let ref_event_l1 = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "引用事件摘要",
        msg_ids[2],
        msg_ids[3],
    )
    .await;
    let evidence_l1 = seed_l1_range(
        &storage,
        session_id,
        "char-0001",
        "证据事件摘要",
        msg_ids[4],
        msg_ids[5],
    )
    .await;

    let ref_event =
        seed_event_with_sources(&storage, "char-0001", "引用事件", 1_000, &[ref_event_l1]).await;
    let evidence_event =
        seed_event_with_sources(&storage, "char-0001", "证据事件", 2_000, &[evidence_l1]).await;

    let trait_id = seed_trait(&storage, "char-0001", "温和", Some(direct), Some(ref_event)).await;
    // 证据先返回较晚写入的一条，命中事件与直连事件重叠
    seed_evidence(&storage, trait_id, evidence_event, 1_000).await;
    seed_evidence(&storage, trait_id, ref_event, 2_000).await;

    let got = engine
        .memory_unit_sources("l3", &trait_id.to_string())
        .await
        .expect("L3 证据导出应成功");
    assert_eq!(
        got,
        msg_ids.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        "直连 L1 + 引用事件 + 证据事件三条路径合并，顺序为直连在前、证据在后"
    );
    assert_eq!(
        got.len(),
        6,
        "重叠事件只展开一次：三路径齐全时为六条而非八条"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// utt 直读块区间：块覆盖中间三条消息，块外消息不纳入。
#[tokio::test]
async fn utt_expands_block_range() {
    let (engine, storage, dir) = engine_with_db("browse-units-utt").await;
    seed_persona(&storage, "char-0001").await;
    let (session_id, msg_ids) = seed_dialog(&storage, "char-0001", 5, 1_000).await;

    let block = UttBlock::new(
        "char-0001".to_string(),
        session_id,
        msg_ids[1],
        msg_ids[3],
        "块内原文".to_string(),
        3,
        3_000,
    );
    let block_id = storage
        .insert_utt_block(&block)
        .await
        .expect("插入 utt 块应成功");

    let got = engine
        .memory_unit_sources("utt", &block_id.to_string())
        .await
        .expect("utt 证据导出应成功");
    assert_eq!(
        got,
        vec![
            msg_ids[1].to_string(),
            msg_ids[2].to_string(),
            msg_ids[3].to_string()
        ],
        "按块自身首末消息定位闭区间，块外消息不纳入"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 输入校验与空态：未知层 / id 形态不符返回校验错误，单元不存在返回空集合。
#[tokio::test]
async fn invalid_input_is_validation_error_and_missing_unit_is_empty() {
    let (engine, storage, dir) = engine_with_db("browse-units-input").await;
    seed_persona(&storage, "char-0001").await;

    for (layer, id) in [
        ("l0", "1"),
        ("l1", "not-a-uuid"),
        ("l2", "abc"),
        ("l3", "0"),
        ("utt", "-3"),
    ] {
        let err = engine
            .memory_unit_sources(layer, id)
            .await
            .expect_err("非法层名或 id 应返回校验错误");
        assert_eq!(
            err.category(),
            "validation",
            "层 {layer} / id {id} 应报校验错误"
        );
    }

    // 单元不存在：按层解析成功但查无记录 → 空集合
    let missing_l1 = engine
        .memory_unit_sources("l1", &Uuid::new_v4().to_string())
        .await
        .expect("不存在的 L1 应返回空集合");
    assert!(missing_l1.is_empty());
    let missing_trait = engine
        .memory_unit_sources("l3", "424242")
        .await
        .expect("不存在的性格标签应返回空集合");
    assert!(missing_trait.is_empty());
    let missing_utt = engine
        .memory_unit_sources("utt", "424242")
        .await
        .expect("不存在的 utt 块应返回空集合");
    assert!(missing_utt.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
