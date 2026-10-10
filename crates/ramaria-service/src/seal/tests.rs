//! crates/ramaria-service/src/seal/tests.rs - 封存用例单元测试
//!
//! 设计特点:
//! - 由 父模块 以 #[cfg(test)] mod tests; 收纳，经 use super::* 取用被测项。
//! - 覆盖封存幂等 / 门禁 / 空会话 / 失败补偿 / 钩子调用 / 社交会话投影。
//! - 以真实 SQLite 临时库与 mock LLM 装配引擎，不依赖网络与真实模型。
//! - 隐私: 测试仅用合成消息与固定回复，不依赖真实数据。

use super::*;
use crate::test_support::{
    L1_JSON_REPLY, MockLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply,
    seed_persona, seed_session_with_messages,
};
use crate::types::{RecallLayer, RecallRequest};
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{Message, MessageRole, MessageSource};
use ramaria_storage::SqliteStorage;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 造一个社交通道会话（通道 / 外部标识 + 双发言人消息：本机侧 assistant + 成员 user）。
///
/// 前置条件:
/// - `rama-0001` / `char-0001` persona 已存在（消息外键依赖）。
async fn seed_social_session(storage: &SqliteStorage, channel: &str, external_ref: &str) -> Uuid {
    let session = storage
        .create_session_in_channel(Some("rama-0001"), channel, Some(external_ref))
        .await
        .expect("创建通道会话应成功");
    let mut self_msg = Message::new(
        session.id,
        MessageRole::Assistant,
        "今天好累".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some("rama-0001".to_string()));
    self_msg.created_at = 1_000;
    self_msg.sender_name = Some("小拉".to_string());
    storage
        .save_message(&self_msg)
        .await
        .expect("写入消息应成功");
    let mut member_msg = Message::new(
        session.id,
        MessageRole::User,
        "我也累了".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some("char-0001".to_string()));
    member_msg.created_at = 1_001;
    member_msg.sender_name = Some("小明".to_string());
    storage
        .save_message(&member_msg)
        .await
        .expect("写入消息应成功");
    session.id
}

/// 封存生成单条 L1，并完成抢占幂等（二次调用不重复生成）。
#[tokio::test]
async fn seal_generates_l1_once_and_is_idempotent() {
    let (engine, storage, dir) = engine_with_l1_reply("seal", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;
    // 先加载索引：验证 L1 生成后的增量镜像链路（检索即可命中）
    engine.ensure_index_loaded().await.expect("索引加载");

    let first = engine.seal(session_id).await.expect("封存应成功");
    assert!(first.sealed, "首次调用应抢到关闭权");
    assert_eq!(first.l1_count, 1, "短会话应生成单条 L1");

    let l1_list = storage
        .list_memory_l1(session_id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(l1_list.len(), 1, "库中应只有一条 L1");
    assert_eq!(
        l1_list[0].persona_uid.as_deref(),
        Some("char-0001"),
        "L1 归属应为会话 persona"
    );

    // 二次调用：已关闭 → 未抢到 → 不重复生成
    let second = engine.seal(session_id).await.expect("封存应幂等成功返回");
    assert!(!second.sealed, "二次调用不应抢到关闭权");
    assert_eq!(second.l1_count, 0);
    assert_eq!(
        storage
            .list_memory_l1(session_id)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "重复封存不得产生第二条 L1"
    );

    // 增量镜像生效：新 L1 可被召回
    let recalled = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回应成功");
    assert!(
        recalled.items.iter().any(|i| i.text.contains("工作压力")),
        "封存后 L1 应立即可检索: {:?}",
        recalled.items
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 并发抢占语义：两个调用方同时封存，只有一方生成 L1。
#[tokio::test]
async fn seal_concurrent_calls_have_single_winner() {
    let (engine, storage, dir) = engine_with_l1_reply("seal-race", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    // 并发触发两个封存（同一会话）
    let engine_a = &engine;
    let (first, second) = tokio::join!(async { engine_a.seal(session_id).await }, async {
        engine_a.seal(session_id).await
    });
    let winners = [first.expect("封存不报错"), second.expect("封存不报错")]
        .into_iter()
        .filter(|outcome| outcome.sealed)
        .count();
    assert_eq!(winners, 1, "并发封存应只有一个赢家（抢占幂等）");
    assert_eq!(
        storage
            .list_memory_l1(session_id)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "并发封存不得产生重复 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 封存门禁（D-V21-009）：许可关闭时封存用例直接跳过
/// （不抢占关闭、不改会话状态、不调用 LLM 生成摘要）。
#[tokio::test]
async fn seal_is_noop_when_seal_disabled() {
    let (engine, storage, dir) = engine_with_l1_reply("seal-gated", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    engine.set_seal_allowed(false);
    let outcome = engine
        .seal(session_id)
        .await
        .expect("封存应正常返回（跳过）");
    assert!(!outcome.sealed, "许可关闭时不应执行封存");
    assert_eq!(outcome.l1_count, 0);

    let session = storage
        .get_session(session_id)
        .await
        .expect("查询应成功")
        .expect("会话应存在");
    assert!(session.ended_at.is_none(), "许可关闭时不应关闭会话");
    assert!(
        storage
            .list_memory_l1(session_id)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "许可关闭时不应生成 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空会话封存：会话关闭但无消息 → 不生成 L1（不调用 LLM）。
#[tokio::test]
async fn seal_empty_session_skips_l1() {
    let (engine, storage, dir) = engine_with_l1_reply("seal-empty", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话");

    let outcome = engine.seal(session.id).await.expect("封存应成功");
    assert!(outcome.sealed);
    assert_eq!(outcome.l1_count, 0);
    assert!(
        storage
            .list_memory_l1(session.id)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "空会话不应生成 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// L1 失败：会话仍关闭、错误可见，并登记 pending 重试任务（不丢补偿）。
#[tokio::test]
async fn seal_l1_failure_registers_retry_job() {
    // 空回复 mock → L1 JSON 解析失败（模拟 LLM 不可用/输出非法）
    let (engine, storage, dir) = engine_with_db("seal-fail").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    let err = engine.seal(session_id).await.expect_err("L1 失败应报错");
    assert!(!err.to_string().is_empty(), "错误信息应可读");

    // 会话已关闭（不阻塞用户继续新会话）
    let session = storage
        .get_session(session_id)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(session.ended_at.is_some(), "会话应已关闭");

    // 登记了 pending 重试任务
    let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
    assert!(
        pending
            .iter()
            .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
        "L1 失败应登记 l1_summary_retry 补偿任务: {pending:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 封存钩子：注册后按位置调用（行为 / 风格 / L2 各一次），未注册则跳过。
#[tokio::test]
async fn seal_invokes_registered_hooks() {
    let (engine, storage, dir) = engine_with_l1_reply("seal-hooks", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    let behavior_calls = Arc::new(AtomicUsize::new(0));
    let style_calls = Arc::new(AtomicUsize::new(0));
    let l2_calls = Arc::new(AtomicUsize::new(0));
    let make_hook = |counter: Arc<AtomicUsize>| -> SealHook {
        Arc::new(move |_engine: &Engine, _persona: &str| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }) as Pin<Box<dyn Future<Output = ()> + Send + '_>>
        })
    };
    engine.set_seal_hooks(SealHooks {
        behavior: Some(make_hook(Arc::clone(&behavior_calls))),
        style: Some(make_hook(Arc::clone(&style_calls))),
        l2_trigger: Some(make_hook(Arc::clone(&l2_calls))),
    });

    engine.seal(session_id).await.expect("封存应成功");
    assert_eq!(behavior_calls.load(Ordering::SeqCst), 1);
    assert_eq!(style_calls.load(Ordering::SeqCst), 1);
    assert_eq!(l2_calls.load(Ordering::SeqCst), 1);

    // 未抢到的封存不得再触发钩子
    engine.seal(session_id).await.expect("重复封存应成功返回");
    assert_eq!(
        behavior_calls.load(Ordering::SeqCst),
        1,
        "重复封存不重复调用钩子"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 社交通道会话：目标相对投影
// =========================================================

/// 社交会话封存：逐目标投影 → 2 目标 2 行（各自 persona_uid）。
#[tokio::test]
async fn seal_social_session_projects_per_target() {
    let (engine, storage, dir) =
        engine_with_l1_reply("seal-social-projection", L1_JSON_REPLY).await;
    seed_persona(&storage, "rama-0001").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_social_session(&storage, CHANNEL_QQ, "c2c:10001").await;

    let outcome = engine.seal(session_id).await.expect("封存应成功");
    assert!(outcome.sealed);
    assert_eq!(outcome.l1_count, 2, "社交会话应逐目标生成 2 行");

    let l1_list = storage
        .list_memory_l1(session_id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(
        l1_list.len(),
        2,
        "库中应有 2 行（目标:rama-0001 / char-0001）"
    );
    let mut personas: Vec<&str> = l1_list
        .iter()
        .filter_map(|l1| l1.persona_uid.as_deref())
        .collect();
    personas.sort_unstable();
    assert_eq!(personas, vec!["char-0001", "rama-0001"]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 本地会话封存：单行（现状回归，与投影口径隔离）。
#[tokio::test]
async fn seal_local_session_single_row() {
    let (engine, storage, dir) = engine_with_l1_reply("seal-local-row", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    let outcome = engine.seal(session_id).await.expect("封存应成功");
    assert_eq!(outcome.l1_count, 1, "本地会话应单行");

    let l1_list = storage
        .list_memory_l1(session_id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(l1_list.len(), 1);
    assert_eq!(l1_list[0].persona_uid.as_deref(), Some("char-0001"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 社交会话失败补偿：payload 携带投影标记，补扫保持投影口径。
#[tokio::test]
async fn seal_retry_payload_carries_projection() {
    let (engine, storage, dir) = engine_with_failing_llm("seal-social-retry").await;
    seed_persona(&storage, "rama-0001").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_social_session(&storage, CHANNEL_QQ, "c2c:10001").await;

    // LLM 不可用 → 封存报错并登记补偿任务
    engine.seal(session_id).await.expect_err("LLM 不可用应报错");
    let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
    let payload = pending
        .iter()
        .find(|(_, job_type, _)| job_type == "l1_summary_retry")
        .and_then(|(_, _, payload)| payload.clone())
        .expect("应登记 l1_summary_retry 补偿任务");
    let value: serde_json::Value = serde_json::from_str(&payload).expect("payload 应为 JSON");
    assert_eq!(
        value.get("target_projection").and_then(|v| v.as_bool()),
        Some(true),
        "补偿 payload 应携带投影标记: {payload}"
    );

    // LLM 恢复 → 补扫（保持投影）→ 2 目标 2 行
    engine.update_llm(Arc::new(MockLlm::with_reply(L1_JSON_REPLY)));
    let completed = engine.retry_pending_l1_jobs().await;
    assert_eq!(completed, 1, "补扫应完成 1 条任务");

    let l1_list = storage
        .list_memory_l1(session_id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(l1_list.len(), 2, "补扫应保持投影口径（2 目标 2 行）");
    let mut personas: Vec<&str> = l1_list
        .iter()
        .filter_map(|l1| l1.persona_uid.as_deref())
        .collect();
    personas.sort_unstable();
    assert_eq!(personas, vec!["char-0001", "rama-0001"]);

    let _ = std::fs::remove_dir_all(&dir);
}
