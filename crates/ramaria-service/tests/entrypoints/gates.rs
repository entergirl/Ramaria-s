//! crates/ramaria-service/tests/entrypoints/gates.rs - 入口开关门禁用例
//!
//! 设计特点:
//! - 封存许可门禁：关闭时封存 / 空闲检查 / finalize 全部跳过（只写不封存），
//!   会话保持活跃且无 L1；恢复许可后同一超时会话被正常封存
//! - 原文闸门：召回策略 `allow_raw_text` 关闭时召回不含原文块，显式放开后含原文块
//! - 原文断言基于封存链路真实产出的 utt 话语块（不手工造块）

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, StoreCrud};
use ramaria_core::types::{Message, MessageRole, MessageSource};
use ramaria_service::types::{ChatRole, ChatTurn, IngestRequest, RecallLayer, RecallRequest};
use ramaria_service::{CHANNEL_MCP, RecallPolicy};

use crate::support::{DeterministicEmbedding, ScriptedLlm, TestDb, seed_timed_out_session};

/// 脚本回复：封存摘要（无特殊检索需求）。
const L1_JSON: &str = r#"{
  "summary": "用户聊了聊近况。",
  "keywords": "近况",
  "time_period": "今天",
  "atmosphere": "平静",
  "valence": 0.0,
  "salience": 0.5,
  "situation_strength": 2
}"#;

/// 封存许可门禁：关闭时只写不封存（超时会话保持活跃），恢复后正常封存。
#[tokio::test]
async fn seal_gate_disabled_keeps_session_open_until_reenabled() {
    const PERSONA: &str = "char-entry-seal-gate";

    let db = TestDb::new("entry-seal-gate");
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
    let (engine, storage) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(L1_JSON)),
            Some(embedding),
            RamariaConfig::default(),
        )
        .await
        .expect("引擎应可装配");

    crate::support::fixtures::seed_persona(storage.as_ref(), PERSONA)
        .await
        .expect("种子人格应写入成功");
    let idle_minutes = engine.config().session.l1_idle_minutes;
    let session = seed_timed_out_session(storage.as_ref(), PERSONA, idle_minutes)
        .await
        .expect("超时会话应造数成功");

    // ---- 门禁关闭：封存用例跳过（不关闭、不生成摘要） ----
    engine.set_seal_allowed(false);
    let blocked = engine
        .seal(session)
        .await
        .expect("门禁关闭时封存应正常返回（跳过）");
    assert!(
        !blocked.sealed,
        "门禁关闭时不应抢到关闭权（session={session}）"
    );
    assert_eq!(
        blocked.l1_count, 0,
        "门禁关闭时不应生成 L1（session={session}）"
    );
    let row = storage
        .get_session(session)
        .await
        .expect("读取会话应成功")
        .expect("会话应存在");
    assert!(
        row.ended_at.is_none(),
        "门禁关闭时会话应保持活跃（session={session}）"
    );
    assert!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "门禁关闭时不应有 L1 落库（session={session}）"
    );

    // ---- 门禁关闭：空闲检查不封存超时会话 ----
    assert_eq!(
        engine.tick_idle().await.expect("空闲检查应成功"),
        0,
        "门禁关闭时空闲检查不应封存超时会话（session={session}）"
    );

    // ---- 门禁关闭：写用例照常，finalize 被跳过 ----
    let ingest = engine
        .ingest(IngestRequest {
            messages: vec![
                ChatTurn {
                    role: ChatRole::User,
                    content: "门禁期间的写入".to_string(),
                },
                ChatTurn {
                    role: ChatRole::Assistant,
                    content: "好的，先记下来".to_string(),
                },
            ],
            persona: Some(PERSONA.to_string()),
            conversation_id: Some("gate-entry-1".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        })
        .await
        .expect("门禁关闭时写入应照常完成");
    assert_eq!(
        ingest.written, 2,
        "门禁关闭时写入仍应落库（session={}）",
        ingest.session_id
    );
    assert!(
        !ingest.finalized,
        "门禁关闭时 finalize 不应触发封存（session={}）",
        ingest.session_id
    );
    assert!(
        storage
            .list_memory_l1(ingest.session_id)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "门禁关闭时回流写入不应生成 L1（session={}）",
        ingest.session_id
    );

    // ---- 恢复许可：同一超时会话被正常封存 ----
    engine.set_seal_allowed(true);
    let sealed = engine.seal(session).await.expect("恢复封存许可后应成功");
    assert!(sealed.sealed, "恢复后应抢到关闭权（session={session}）");
    assert_eq!(
        sealed.l1_count, 1,
        "恢复后应生成一条 L1（session={session}）"
    );
    let row = storage
        .get_session(session)
        .await
        .expect("读取会话应成功")
        .expect("会话应存在");
    assert!(
        row.ended_at.is_some(),
        "恢复后会话应已关闭（session={session}）"
    );

    db.cleanup().await;
}

/// 原文闸门：`allow_raw_text` 关闭时不返回原文块，显式放开后返回。
#[tokio::test]
async fn recall_raw_text_gate_follows_policy() {
    const PERSONA: &str = "char-entry-raw-gate";
    const RAW_MARKER: &str = "乌篷船";

    let db = TestDb::new("entry-raw-gate");
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
    let (engine, storage) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(L1_JSON)),
            Some(embedding),
            RamariaConfig::default(),
        )
        .await
        .expect("引擎应可装配");

    crate::support::fixtures::seed_persona(storage.as_ref(), PERSONA)
        .await
        .expect("种子人格应写入成功");

    // ---- 造一个含原文特征词的会话并封存（真实产出 utt 话语块） ----
    let session_row = storage
        .create_session(Some(PERSONA))
        .await
        .expect("创建会话应成功");
    let session = session_row.id;
    let mut user_message = Message::new(
        session,
        MessageRole::User,
        format!("我爷爷年轻时是{RAW_MARKER}的船工，常跟我讲那段日子"),
        MessageSource::Local,
    )
    .with_persona_uid(Some(PERSONA.to_string()));
    user_message.created_at = crate::support::fixtures::fixture_ts(0);
    storage
        .save_message(&user_message)
        .await
        .expect("写入用户消息应成功");
    let mut assistant_message = Message::new(
        session,
        MessageRole::Assistant,
        "听起来是一段很有画面感的经历。".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(PERSONA.to_string()));
    assistant_message.created_at = crate::support::fixtures::fixture_ts(1);
    storage
        .save_message(&assistant_message)
        .await
        .expect("写入助手消息应成功");

    let sealed = engine.seal(session).await.expect("封存应成功");
    assert!(sealed.sealed, "本次调用应抢到封存权（session={session}）");
    let utt_blocks = storage
        .list_utt_blocks_by_persona(PERSONA)
        .await
        .expect("读取 utt 话语块应成功");
    assert!(
        !utt_blocks.is_empty(),
        "封存链路应产出 utt 话语块（session={session}）"
    );
    // 重建索引：utt 话语块进入内存检索
    engine.rebuild_index().await.expect("重建索引应成功");

    // ---- 保守策略：原文不出端 ----
    engine.set_recall_policy(RecallPolicy::default());
    let blocked = engine
        .recall(RecallRequest {
            query: Some(RAW_MARKER.to_string()),
            persona: Some(PERSONA.to_string()),
            include: Some(vec![RecallLayer::Raw]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回应成功");
    assert!(
        !blocked.context.contains(RAW_MARKER),
        "allow_raw_text=false 时不得返回原文块（session={session}）: {}",
        blocked.context
    );

    // ---- 显式放开原文：返回原文块 ----
    engine.set_recall_policy(RecallPolicy::default().with_allow_raw_text(true));
    let allowed = engine
        .recall(RecallRequest {
            query: Some(RAW_MARKER.to_string()),
            persona: Some(PERSONA.to_string()),
            include: Some(vec![RecallLayer::Raw]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回应成功");
    assert!(
        allowed.context.contains(RAW_MARKER),
        "allow_raw_text=true 时应返回原文块（session={session}）: {}",
        allowed.context
    );

    db.cleanup().await;
}
