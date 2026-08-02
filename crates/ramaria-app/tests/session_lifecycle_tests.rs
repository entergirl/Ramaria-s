//! rust/crates/ramaria-app/tests/session_lifecycle_tests.rs - Session 生命周期集成测试
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
use ramaria_core::traits::StorageBackend;
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
// T-V11-0-008.1: 手动关闭
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
// T-V11-0-008.2: 新消息自动创建 session
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
// T-V11-0-008.3: 已关闭 session 只读约束
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
// T-V11-0-008.4: shutdown 自动关闭活跃 session
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
// T-V11-0-008.5: send_message 在非 Ready 状态被拒绝
// （原 send_message_rejected_in_needs_setup 与 app_integration.rs 的
//  send_message_rejects_when_not_ready 同 setup 同断言，已删除）
// =========================================================

// =========================================================
// T-V11-0-008.6: 后台任务幂等启动
// （原 background_tasks_start_only_once 无任何断言，仅观察日志，已删除）
// =========================================================

// =========================================================
// T-V11-0-008.7: get_active_session_id 线程安全
// （原 active_session_id_default_is_none 为琐碎 getter 初始值断言，已删除）
// =========================================================

// =========================================================
// T-V11-0-008.8: save_and_close 后新消息创建新 session
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
// T-V11-0-008.9: 指定 session_id 发消息到活跃 session
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
