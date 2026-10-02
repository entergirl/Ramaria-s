//! crates/ramaria-storage/src/tests/personality_trait.rs - 人格特质与证据存储测试
//!
//! 设计特点:
//! - 覆盖人格特质写入与按 persona 读取
//! - 覆盖特质置信度与状态更新
//! - 覆盖证据写入与按特质读取

use super::*;

#[tokio::test]
async fn personality_trait_crud() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let pt = PersonalityTrait::new(
        "user-0001".into(),
        TraitLayer::Base,
        "温和".into(),
        "待人温和".into(),
        TraitSource::Inferred,
        0,
    );
    let pt_id = storage.save_trait(&pt).await.unwrap();
    assert!(pt_id > 0);

    let traits = storage.list_traits_by_persona("user-0001").await.unwrap();
    assert_eq!(traits.len(), 1);
    assert_eq!(traits[0].trait_label, "温和");
    assert_eq!(traits[0].id, pt_id);

    // 更新置信度
    storage
        .update_trait_confidence(pt_id, 0.8, 5.0, 0.9)
        .await
        .unwrap();
    // 更新状态
    storage
        .update_trait_status(pt_id, TraitStatus::Deprecated)
        .await
        .unwrap();
}

#[tokio::test]
async fn trait_evidence_crud() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let pt = PersonalityTrait::new(
        "user-0001".into(),
        TraitLayer::Base,
        "温和".into(),
        "待人温和".into(),
        TraitSource::Inferred,
        0,
    );
    let pt_id = storage.save_trait(&pt).await.unwrap();
    let now = now_ms();
    let ev = MemoryEvent::new("user-0001".into(), "事件".into(), "描述".into(), now, now);
    let ev_id = storage.save_event(&ev).await.unwrap();

    let evidence = TraitEvidence::new(pt_id, ev_id, EvidenceDirection::Support, 0.8);
    let evd_id = storage.save_evidence(&evidence).await.unwrap();
    assert!(evd_id > 0);

    let list = storage.list_evidence_by_trait(pt_id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, evd_id);
    assert_eq!(list[0].trait_id, pt_id);
    assert_eq!(list[0].event_id, ev_id);
}
