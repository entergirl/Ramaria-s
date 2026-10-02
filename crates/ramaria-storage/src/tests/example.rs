//! crates/ramaria-storage/src/tests/example.rs - persona 示例池存储测试
//!
//! 设计特点:
//! - 覆盖示例保存与全量 / 已选读取
//! - 覆盖按伙伴 - 回复对查重
//! - 覆盖字段往返（session_id / tags / context / length）
//! - 覆盖跨 persona 隔离

use super::*;

// =========================================================
// examples repo（v1.4）
// =========================================================

async fn setup_example_persona(storage: &SqliteStorage) -> String {
    let persona = ramaria_core::types::Persona::new(
        "char-0001".to_string(),
        "测试角色".to_string(),
        ramaria_core::types::PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage.create_persona(&persona).await.unwrap();
    "char-0001".to_string()
}

fn example(uid: &str, partner: &str, reply: &str) -> ramaria_core::types::PersonaExample {
    let mut e = ramaria_core::types::PersonaExample::new(
        uid.to_string(),
        partner.to_string(),
        reply.to_string(),
    );
    e.tags = Some("测试,话题".to_string());
    e.context = Some("前文".to_string());
    e
}

#[tokio::test]
async fn examples_repo_save_and_list_all() {
    let storage = setup().await;
    let uid = setup_example_persona(&storage).await;

    storage
        .save_example(&example(&uid, "问题甲", "回复内容甲"))
        .await
        .unwrap();
    storage
        .save_example(&example(&uid, "问题乙", "回复内容乙"))
        .await
        .unwrap();

    let all = storage.list_all_examples(&uid).await.unwrap();
    assert_eq!(all.len(), 2, "候选池应包含全部示例");
    assert!(all.iter().all(|e| e.persona_uid == uid));

    // 新入库示例为候选（selected=false）→ list_selected 兼容路径为空
    let selected = storage.list_selected_examples(&uid).await.unwrap();
    assert!(selected.is_empty(), "候选池示例不进入静态 selected 路径");

    // 跨 persona 隔离
    let other = storage.list_all_examples("char-9999").await.unwrap();
    assert!(other.is_empty());
}

#[tokio::test]
async fn examples_repo_find_by_pair() {
    let storage = setup().await;
    let uid = setup_example_persona(&storage).await;
    storage
        .save_example(&example(&uid, "问题甲", "回复内容甲"))
        .await
        .unwrap();

    let hit = storage
        .find_example_by_pair(&uid, "问题甲", "回复内容甲")
        .await
        .unwrap();
    assert!(hit.is_some(), "相同回复对应查重命中");
    assert_eq!(hit.unwrap().id, 1);

    // 内容不同 / 归属不同 → 未命中
    assert!(
        storage
            .find_example_by_pair(&uid, "问题甲", "回复内容乙")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .find_example_by_pair("char-9999", "问题甲", "回复内容甲")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn examples_repo_list_selected_respects_flag() {
    let storage = setup().await;
    let uid = setup_example_persona(&storage).await;

    let mut sel = example(&uid, "问题甲", "回复内容甲");
    sel.selected = true;
    storage.save_example(&sel).await.unwrap();
    storage
        .save_example(&example(&uid, "问题乙", "回复内容乙"))
        .await
        .unwrap();

    let selected = storage.list_selected_examples(&uid).await.unwrap();
    assert_eq!(selected.len(), 1, "仅 selected=1 的示例被静态路径返回");
    assert_eq!(selected[0].partner, "问题甲");

    let all = storage.list_all_examples(&uid).await.unwrap();
    assert_eq!(all.len(), 2, "候选池路径返回全部");
}

#[tokio::test]
async fn examples_repo_save_roundtrip_fields() {
    let storage = setup().await;
    let uid = setup_example_persona(&storage).await;
    let session = storage.create_session(Some(&uid)).await.unwrap();

    let mut e = example(&uid, "问题甲", "回复内容甲");
    e.session_id = Some(session.id);
    storage.save_example(&e).await.unwrap();

    let all = storage.list_all_examples(&uid).await.unwrap();
    assert_eq!(all.len(), 1);
    let got = &all[0];
    assert_eq!(got.session_id, e.session_id);
    assert_eq!(got.tags.as_deref(), Some("测试,话题"));
    assert_eq!(got.context.as_deref(), Some("前文"));
    assert_eq!(got.length, 5, "length = reply 字符数");
    assert!(!got.selected);
}
