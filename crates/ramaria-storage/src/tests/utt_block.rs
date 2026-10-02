//! crates/ramaria-storage/src/tests/utt_block.rs - 原文话语块存储测试
//!
//! 设计特点:
//! - 覆盖话语块插入与按会话读取最新块
//! - 覆盖按 persona 读取的严格隔离
//! - 覆盖按会话删除的幂等与空会话返回 None
//! - 覆盖嵌入向量的 BLOB 小端往返

use super::*;

// =========================================================
// Utt Blocks（原文话语块）
// =========================================================

/// 辅助：创建 persona + session + 若干消息，返回 (storage, persona_uid, session_id)。
async fn setup_utt_context() -> (SqliteStorage, String, Uuid) {
    let storage = setup().await;
    let p = Persona::new(
        "char-0001".into(),
        "测试角色".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let session = storage.create_session(Some("char-0001")).await.unwrap();

    // 插入 3 条消息作为块内原文（utt_blocks FK→messages）
    for (i, text) in ["你好呀", "最近怎么样", "挺好的"].iter().enumerate() {
        let msg = Message::new(
            session.id,
            MessageRole::User,
            text.to_string(),
            MessageSource::Local,
        )
        .with_persona_uid(Some("char-0001".to_string()));
        // 时间递增，保证 created_at 有序
        let mut m = msg;
        m.created_at = 1_700_000_000_000 + i as i64 * 60_000;
        storage.save_message(&m).await.unwrap();
    }
    (storage, "char-0001".to_string(), session.id)
}

#[tokio::test]
async fn utt_block_insert_and_get_latest() {
    let (storage, persona_uid, session_id) = setup_utt_context().await;
    let messages = storage.list_messages(session_id).await.unwrap();

    let block = UttBlock::new(
        persona_uid.clone(),
        session_id,
        messages[0].id,
        messages[2].id,
        "你好呀\n最近怎么样\n挺好的".to_string(),
        3,
        120_000,
    );
    let id = storage.insert_utt_block(&block).await.unwrap();
    assert!(id > 0, "插入应返回自增 id");

    let latest = storage
        .get_latest_utt_block_by_session(session_id)
        .await
        .unwrap()
        .expect("会话应有最新话语块");
    assert_eq!(latest.id, id);
    assert_eq!(latest.persona_uid, persona_uid);
    assert_eq!(latest.msg_count, 3);
    assert_eq!(latest.time_span_ms, 120_000);
    assert_eq!(latest.block_text, "你好呀\n最近怎么样\n挺好的");
    assert!(latest.embedding.is_none(), "未设置 embedding 时应为 None");
}

#[tokio::test]
async fn utt_block_list_by_persona_isolation() {
    let (storage, persona_uid, session_id) = setup_utt_context().await;
    let messages = storage.list_messages(session_id).await.unwrap();

    // persona A 插入 2 个块
    for n in 0..2 {
        let block = UttBlock::new(
            persona_uid.clone(),
            session_id,
            messages[0].id,
            messages[2].id,
            format!("块{n}"),
            1,
            0,
        );
        storage.insert_utt_block(&block).await.unwrap();
    }

    // persona B（不同 uid）查询 → 严格隔离，看不到 persona A 的块
    let other = storage
        .list_utt_blocks_by_persona("char-9999")
        .await
        .unwrap();
    assert!(other.is_empty(), "跨 persona 不应看到原文块");

    let mine = storage
        .list_utt_blocks_by_persona(&persona_uid)
        .await
        .unwrap();
    assert_eq!(mine.len(), 2, "应返回本人 persona 的全部块");
    assert_eq!(mine[0].block_text, "块0");
    assert_eq!(mine[1].block_text, "块1");
}

#[tokio::test]
async fn utt_block_latest_returns_newest() {
    let (storage, persona_uid, session_id) = setup_utt_context().await;
    let messages = storage.list_messages(session_id).await.unwrap();

    // 按时间顺序插入 3 个块
    let mut last_id = 0;
    for n in 0..3 {
        let block = UttBlock::new(
            persona_uid.clone(),
            session_id,
            messages[0].id,
            messages[2].id,
            format!("块{n}"),
            1,
            0,
        );
        last_id = storage.insert_utt_block(&block).await.unwrap();
    }

    let latest = storage
        .get_latest_utt_block_by_session(session_id)
        .await
        .unwrap()
        .expect("应有最新块");
    assert_eq!(latest.id, last_id, "应返回最后插入的块");
    assert_eq!(latest.block_text, "块2");
}

#[tokio::test]
async fn utt_block_delete_by_session() {
    let (storage, persona_uid, session_id) = setup_utt_context().await;
    let messages = storage.list_messages(session_id).await.unwrap();

    for n in 0..3 {
        let block = UttBlock::new(
            persona_uid.clone(),
            session_id,
            messages[0].id,
            messages[2].id,
            format!("块{n}"),
            1,
            0,
        );
        storage.insert_utt_block(&block).await.unwrap();
    }

    let deleted = storage
        .delete_utt_blocks_by_session(session_id)
        .await
        .unwrap();
    assert_eq!(deleted, 3, "应删除 3 个块");

    let remaining = storage
        .list_utt_blocks_by_persona(&persona_uid)
        .await
        .unwrap();
    assert!(remaining.is_empty(), "删除后不应残留块");

    // 幂等：再次删除返回 0
    let again = storage
        .delete_utt_blocks_by_session(session_id)
        .await
        .unwrap();
    assert_eq!(again, 0);
}

#[tokio::test]
async fn utt_block_empty_session_returns_none() {
    let (storage, _, session_id) = setup_utt_context().await;
    let latest = storage
        .get_latest_utt_block_by_session(session_id)
        .await
        .unwrap();
    assert!(latest.is_none(), "无块会话应返回 None");
}

#[tokio::test]
async fn utt_block_embedding_roundtrip() {
    let (storage, persona_uid, session_id) = setup_utt_context().await;
    let messages = storage.list_messages(session_id).await.unwrap();

    // 构造 4 维 f32 向量的小端 BLOB
    let vector = vec![0.1f32, 0.2, 0.3, 0.4];
    let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();

    let mut block = UttBlock::new(
        persona_uid,
        session_id,
        messages[0].id,
        messages[2].id,
        "带向量的块".to_string(),
        3,
        60_000,
    );
    block.embedding = Some(blob);
    storage.insert_utt_block(&block).await.unwrap();

    let latest = storage
        .get_latest_utt_block_by_session(session_id)
        .await
        .unwrap()
        .expect("应有块");
    let stored = latest.embedding.expect("embedding 应往返保留");
    assert_eq!(stored.len(), 16, "4 × f32 = 16 字节");
    let back: Vec<f32> = stored
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(back, vector);
}
