//! crates/ramaria-app/tests/session_lifecycle_tests.rs - Session 生命周期集成测试
//!
//! 设计特点:
//! - 使用 MockStorage + MockLlm 验证 session 生命周期的完整行为
//! - 覆盖场景：手动关闭、空闲自动关闭、shutdown 关闭、新消息自动创建 session
//! - 覆盖只读约束：向已关闭 session 写入消息被拒绝
//! - 对齐 Python SessionManager 行为
//!
//! 安全约束:
//! - 所有测试使用 mock，不触碰真实数据库或 LLM 服务

// 测试中 send_message 返回值可能不需要消费，抑制 unused_must_use 警告
#![allow(unused_must_use)]

mod mock_backend;

use ramaria_app::App;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StorageBackend, StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig, MessageRole, MessageSource};
use ramaria_llm::keychain::Keychain;
use std::sync::Arc;

use crate::mock_backend::{MockLlm, MockStorage};

// =========================================================
// 测试辅助函数
// =========================================================

/// 构造一个 Ready 状态的 App 实例（含 MockStorage + MockLlm）。
#[allow(dead_code)]
fn build_ready_app() -> App {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("你好，我是 Ramaria。"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());

    let app = App::new_without_embedding(storage, llm, config, keychain);

    // 模拟设置完成：写入后端配置 + 设置状态为 Ready
    // 注意：由于 run_setup 需要 async，这里直接设置状态
    // app.run_setup 在测试中可通过直接调用来模拟
    app.set_state(AppState::Ready);

    app
}

/// 为 App 写入后端配置并确保状态为 Ready（异步辅助）。
async fn setup_app_ready(storage: &dyn StorageBackend) {
    // 写入 LM Studio 默认配置（本地 provider，无需隐私确认）
    let cfg = BackendConfig::lm_studio_default();
    storage.save_backend_config(&cfg).await.unwrap();
}

// =========================================================
// 手动关闭
// （原 manual_save_and_close_session 的完整流程已被
//  new_session_created_after_save_and_close 覆盖，已删除）
// =========================================================

#[tokio::test]
async fn save_and_close_without_active_session_is_noop() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("测试回复"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(storage, llm, config, keychain);

    app.set_state(AppState::Ready);

    // 无活跃 session 时调用 save_and_close 应成功（不报错）
    let result = app.save_and_close_session(None).await;
    assert!(result.is_ok(), "无活跃 session 时 save_and_close 应返回 Ok");
}

// =========================================================
// 新消息自动创建 session
// =========================================================

#[tokio::test]
async fn new_message_auto_creates_session() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("自动创建测试"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );

    setup_app_ready(storage.as_ref()).await;
    app.set_state(AppState::Ready);

    // 初始无活跃 session
    assert!(app.get_active_session_id().is_none());

    // 发送消息 → 自动创建 session
    app.send_message("第一条消息", None, None).await.unwrap();

    let sid1 = app
        .get_active_session_id()
        .expect("第一条消息后应有活跃 session");

    // 同一 session 继续发消息 → 不创建新 session
    app.send_message("第二条消息", None, Some(sid1))
        .await
        .unwrap();

    let sid2 = app
        .get_active_session_id()
        .expect("第二条消息后仍应有活跃 session");
    assert_eq!(sid1, sid2, "使用同一 session 发消息不应创建新 session");
}

// =========================================================
// 已关闭 session 只读约束
// =========================================================

#[tokio::test]
async fn cannot_send_message_to_closed_session() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("测试回复"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );

    setup_app_ready(storage.as_ref()).await;
    app.set_state(AppState::Ready);

    // 1. 发送消息创建 session
    app.send_message("你好", None, None).await.unwrap();
    let sid = app.get_active_session_id().unwrap();

    // 2. 关闭 session
    storage.close_session(sid).await.unwrap();
    let closed = storage.get_session(sid).await.unwrap().unwrap();
    assert!(closed.ended_at.is_some(), "session 应已关闭");

    // 3. 尝试向已关闭 session 发送消息 → 应失败
    let result = app.send_message("还能说话吗？", None, Some(sid)).await;
    match result {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("已关闭") || err_msg.contains("closed"),
                "错误消息应提示 session 已关闭，实际: {err_msg}"
            );
        }
        Ok(_stream) => panic!("向已关闭 session 发消息应返回错误，但成功了"),
    }
}

#[tokio::test]
async fn mock_storage_save_rejects_closed_session() {
    // 验证 MockStorage 层面也拒绝了向已关闭 session 写入
    let storage = MockStorage::new();

    let sid = storage.create_session(None).await.unwrap().id;

    // 写入一条消息到活跃 session → 应成功
    let msg = ramaria_core::types::Message::new(
        sid,
        MessageRole::User,
        "测试".into(),
        MessageSource::Local,
    );
    storage
        .save_message(&msg)
        .await
        .expect("活跃 session 写入应成功");

    // 关闭 session
    storage.close_session(sid).await.unwrap();

    // 写入消息到已关闭 session → 应失败
    let msg2 = ramaria_core::types::Message::new(
        sid,
        MessageRole::User,
        "再测试".into(),
        MessageSource::Local,
    );
    let result = storage.save_message(&msg2).await;
    assert!(result.is_err(), "已关闭 session 写入应被拒绝");
}

// =========================================================
// shutdown 自动关闭活跃 session
// =========================================================

#[tokio::test]
async fn shutdown_closes_active_session() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("shutdown 测试"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );

    setup_app_ready(storage.as_ref()).await;
    app.set_state(AppState::Ready);

    // 发送消息创建活跃 session
    app.send_message("你好", None, None).await.unwrap();
    let sid = app.get_active_session_id().unwrap();

    // 验证 session 活跃
    let session = storage.get_session(sid).await.unwrap().unwrap();
    assert!(session.ended_at.is_none(), "session 应为活跃状态");

    // 调用 shutdown
    app.shutdown().await;

    // 验证活跃 session 已清除
    assert!(
        app.get_active_session_id().is_none(),
        "shutdown 后活跃 session 应为 None"
    );

    // 验证 session 已关闭
    let session = storage.get_session(sid).await.unwrap().unwrap();
    assert!(session.ended_at.is_some(), "shutdown 后 session 应已关闭");
}

// =========================================================
// save_and_close 后新消息创建新 session
// =========================================================

#[tokio::test]
async fn new_session_created_after_save_and_close() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("创建新 session 测试"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );

    setup_app_ready(storage.as_ref()).await;
    app.set_state(AppState::Ready);

    // 1. 发送消息 → 创建 session A
    app.send_message("消息A", None, None).await.unwrap();
    let sid_a = app.get_active_session_id().unwrap();

    // 2. 手动保存并关闭 session A
    app.save_and_close_session(None).await.unwrap();
    assert!(app.get_active_session_id().is_none());

    // 3. 再次发送消息 → 应自动创建 session B（不同于 A）
    app.send_message("消息B", None, None).await.unwrap();
    let sid_b = app.get_active_session_id().unwrap();

    assert_ne!(sid_a, sid_b, "save_and_close 后新消息应创建不同的 session");
}

// =========================================================
// 指定 session_id 发消息到活跃 session
// =========================================================

#[tokio::test]
async fn send_message_with_explicit_session_id() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("显式 session 测试"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );

    setup_app_ready(storage.as_ref()).await;
    app.set_state(AppState::Ready);

    // 手动创建 session
    let session = storage.create_session(None).await.unwrap();
    let sid = session.id;

    // 使用指定 session_id 发消息（需消费流以等待消息保存完成）
    use futures::StreamExt;
    let mut stream = app
        .send_message("显式 session 消息", None, Some(sid))
        .await
        .unwrap();

    // 消费流直到 Done（消息在后台保存，Done 事件后才确保已写入）
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(ramaria_app::StreamEvent::Done { .. }) => break,
            Err(_) => { /* 忽略流错误 */ }
            _ => {}
        }
    }

    // 给 tokio 一点时间完成 spawn 任务中的 save_message
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // 验证消息已写入
    let msgs = storage.list_messages(sid).await.unwrap();
    assert!(!msgs.is_empty(), "消息应已写入指定 session");
}

// =========================================================
// v1.4 截断修复：L1 摘要 max_tokens 从 backend_config 传播
// =========================================================

/// backend_config.max_tokens ≥ L1 默认值时，L1 摘要请求使用 backend 值
/// （默认 512 对 evidence_notes 结构化 JSON 输出过紧，易被截断）。
#[tokio::test]
async fn l1_summary_uses_backend_config_max_tokens() {
    use ramaria_memory::l1::L1SummarizerConfig;

    // MockLlm 需返回合法 L1 JSON，确保 save_and_close 走完整成功路径
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    app.set_state(AppState::Ready);

    // 自定义 backend_config：max_tokens = 2048（高于 L1 默认值）
    let mut bc = BackendConfig::lm_studio_default();
    bc.max_tokens = 2048;
    storage.save_backend_config(&bc).await.unwrap();

    // 发送消息创建活跃 session（drain 流等待消息保存完成）
    use futures::StreamExt;
    let mut stream = app.send_message("你好", None, None).await.unwrap();
    while let Some(event_result) = stream.next().await {
        if matches!(event_result, Ok(ramaria_app::StreamEvent::Done { .. })) {
            break;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        app.get_active_session_id().is_some(),
        "send_message 后应有活跃 session"
    );

    app.save_and_close_session(None).await.unwrap();

    let last = llm
        .last_request()
        .expect("save_and_close 应触发 L1 摘要请求");
    assert_eq!(
        last.max_tokens,
        2048,
        "L1 摘要应使用 backend_config.max_tokens（2048），而非 L1 默认 {}",
        L1SummarizerConfig::default().max_tokens
    );
}

/// backend_config.max_tokens 低于 L1 默认值时，钳制到 L1 默认值，
/// 防止用户将 chat max_tokens 配得过小时破坏 L1 完整 JSON 输出。
#[tokio::test]
async fn l1_summary_max_tokens_has_floor() {
    use ramaria_memory::l1::L1SummarizerConfig;

    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    app.set_state(AppState::Ready);

    // 自定义 backend_config：max_tokens = 128（低于 L1 默认值 1024）
    let mut bc = BackendConfig::lm_studio_default();
    bc.max_tokens = 128;
    storage.save_backend_config(&bc).await.unwrap();

    // 发送消息创建活跃 session（drain 流等待消息保存完成）
    use futures::StreamExt;
    let mut stream = app.send_message("你好", None, None).await.unwrap();
    while let Some(event_result) = stream.next().await {
        if matches!(event_result, Ok(ramaria_app::StreamEvent::Done { .. })) {
            break;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        app.get_active_session_id().is_some(),
        "send_message 后应有活跃 session"
    );

    app.save_and_close_session(None).await.unwrap();

    let floor = L1SummarizerConfig::default().max_tokens;
    let last = llm
        .last_request()
        .expect("save_and_close 应触发 L1 摘要请求");
    assert_eq!(
        last.max_tokens, floor,
        "L1 摘要 max_tokens 不应低于 L1 默认值（{floor}），实际 {}",
        last.max_tokens
    );
}

// =========================================================
// save_and_close_session 归属统一以 DB sessions.persona_uid 为真相源
// =========================================================

/// 手动保存时前端传入 None（或旧内存值），但 DB 中 session 已绑定 persona →
/// L1 归属应取 DB 值，不依赖前端内存态。
#[tokio::test]
async fn save_and_close_l1_uses_db_session_persona() {
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    app.set_state(AppState::Ready);

    // 发送消息：resolve_session 创建并绑定 char-0001 的活跃 session
    use futures::StreamExt;
    let mut stream = app
        .send_message("你好", Some("char-0001"), None)
        .await
        .unwrap();
    while let Some(event_result) = stream.next().await {
        if matches!(event_result, Ok(ramaria_app::StreamEvent::Done { .. })) {
            break;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sid = app.get_active_session_id().expect("应有活跃 session");

    // 保存时前端传 None（空闲保存/旧前端可能传 None 或过期内存值）
    app.save_and_close_session(None).await.unwrap();

    // L1 归属应为 DB 会话绑定的 char-0001，而非 NULL
    let l1s = storage.list_memory_l1(sid).await.unwrap();
    assert!(!l1s.is_empty(), "L1 应已生成");
    assert_eq!(
        l1s[0].persona_uid.as_deref(),
        Some("char-0001"),
        "L1 归属应取 DB sessions.persona_uid"
    );
}

/// 空闲自动保存路径（idle.rs 从 DB 读 persona_uid）与手动保存路径
/// 使用同一真相源：DB 已绑定时无论调用方传什么，都以 DB 为准。
#[tokio::test]
async fn save_and_close_ignores_stale_input_persona() {
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    app.set_state(AppState::Ready);

    use futures::StreamExt;
    let mut stream = app
        .send_message("你好", Some("char-0001"), None)
        .await
        .unwrap();
    while let Some(event_result) = stream.next().await {
        if matches!(event_result, Ok(ramaria_app::StreamEvent::Done { .. })) {
            break;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sid = app.get_active_session_id().expect("应有活跃 session");

    // 前端内存态过期（错误地传了 char-9999）→ 不应覆盖 DB 真相源
    app.save_and_close_session(Some("char-9999")).await.unwrap();

    let l1s = storage.list_memory_l1(sid).await.unwrap();
    assert!(!l1s.is_empty(), "L1 应已生成");
    assert_eq!(
        l1s[0].persona_uid.as_deref(),
        Some("char-0001"),
        "过期前端内存值不应覆盖 DB 会话归属"
    );
}
