//! crates/ramaria-service/tests/suites/session_lifecycle.rs - 会话生命周期与 L1 摘要用例
//!
//! 设计特点:
//! - 覆盖活跃会话状态机：自动建会话、显式会话复用、关闭后新消息另起会话
//! - 覆盖关闭语义：封存后会话不可续写、无活跃会话关闭为空操作、shutdown 关闭活跃会话
//! - 覆盖 L1 摘要口径：`max_tokens` 取后端配置并以下限钳制、人格取库内会话归属
//! - 覆盖长会话分段 L1 与待定 L1 任务补扫
//! - 全部使用内存存储 + mock LLM，无真实数据库与网络

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{LlmProvider, StoreCrud, StoreInfrastructure};
use ramaria_core::types::{BackendConfig, MessageRole, MessageSource};
use ramaria_service::{Engine, LifecycleOptions};

use crate::support::engine_env::{
    build_engine, mark_ready, seal_full, send_stream, stream_request, try_send_stream,
};
use crate::support::mock_backend::{MockLlm, MockStorage};

// =========================================================
// 测试辅助函数
// =========================================================

/// 构造并推进到对话可用状态的引擎（含 MockStorage + MockLlm）。
async fn build_ready_engine(
    storage: &Arc<MockStorage>,
    llm: &Arc<MockLlm>,
    config: RamariaConfig,
) -> Arc<Engine> {
    let engine = build_engine(
        Arc::clone(storage),
        Arc::clone(llm) as Arc<dyn LlmProvider>,
        config,
    );
    mark_ready(&engine).await.expect("就绪推进应成功");
    engine
}

/// 轮询等待异步条件成立：每 5ms 检查一次，最长 5s；超时即 panic（附带等待目标描述）。
async fn wait_until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if check().await {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("等待超时（5s）：{what}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

// =========================================================
// 手动关闭
// =========================================================

#[tokio::test]
async fn save_and_close_without_active_session_is_noop() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("测试回复"));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());

    // 无活跃 session 时调用手动关闭应成功（不报错）
    let result = lifecycle.close_active_session().await;
    assert!(result.is_ok(), "无活跃 session 时 save_and_close 应返回 Ok");
}

// =========================================================
// 新消息自动创建 session
// =========================================================

#[tokio::test]
async fn new_message_auto_creates_session() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("自动创建测试"));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 初始无活跃 session
    assert!(
        storage
            .list_active_sessions()
            .await
            .expect("读取活跃会话应成功")
            .is_empty()
    );

    // 发送消息 → 自动创建 session
    let outcome = send_stream(&engine, "第一条消息", None, None).await;
    let sid1 = outcome.session_id;

    // 同一 session 继续发消息 → 不创建新 session
    let outcome = send_stream(&engine, "第二条消息", None, Some(sid1)).await;
    let sid2 = outcome.session_id;
    assert_eq!(sid1, sid2, "使用同一 session 发消息不应创建新 session");
}

// =========================================================
// 已关闭 session 只读约束
// =========================================================

#[tokio::test]
async fn cannot_send_message_to_closed_session() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("测试回复"));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 1. 发送消息创建 session
    let outcome = send_stream(&engine, "你好", None, None).await;
    let sid = outcome.session_id;

    // 2. 关闭 session
    storage.close_session(sid).await.unwrap();
    let closed = storage.get_session(sid).await.unwrap().unwrap();
    assert!(closed.ended_at.is_some(), "session 应已关闭");

    // 3. 尝试向已关闭 session 发送消息 → 应失败
    let result = try_send_stream(&engine, stream_request("还能说话吗？", None, Some(sid))).await;
    match result {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("已关闭") || err_msg.contains("closed"),
                "错误消息应提示 session 已关闭，实际: {err_msg}"
            );
        }
        Ok(_outcome) => panic!("向已关闭 session 发消息应返回错误，但成功了"),
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
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 发送消息创建活跃 session
    let outcome = send_stream(&engine, "你好", None, None).await;
    let sid = outcome.session_id;

    // 验证 session 活跃
    let session = storage.get_session(sid).await.unwrap().unwrap();
    assert!(session.ended_at.is_none(), "session 应为活跃状态");

    // 调用 shutdown（宿主在收到事件流句柄后维护活跃指针）
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
    lifecycle.set_active_session_id(Some(sid));
    lifecycle.touch_session(sid);
    lifecycle.shutdown().await;

    // 验证活跃 session 已清除
    assert!(
        lifecycle.active_session_id().is_none(),
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
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());

    // 1. 发送消息 → 创建 session A
    let outcome = send_stream(&engine, "消息A", None, None).await;
    let sid_a = outcome.session_id;
    lifecycle.set_active_session_id(Some(sid_a));
    lifecycle.touch_session(sid_a);

    // 2. 手动保存并关闭 session A
    lifecycle.close_active_session().await.unwrap();
    assert!(lifecycle.active_session_id().is_none());

    // 3. 再次发送消息 → 应自动创建 session B（不同于 A）
    let outcome = send_stream(&engine, "消息B", None, None).await;
    let sid_b = outcome.session_id;

    assert_ne!(sid_a, sid_b, "save_and_close 后新消息应创建不同的 session");
}

// =========================================================
// 指定 session_id 发消息到活跃 session
// =========================================================

#[tokio::test]
async fn send_message_with_explicit_session_id() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("显式 session 测试"));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 手动创建 session
    let session = storage.create_session(None).await.unwrap();
    let sid = session.id;

    // 使用指定 session_id 发消息（消费事件流以等待消息保存完成）
    let _outcome = send_stream(&engine, "显式 session 消息", None, Some(sid)).await;

    // 等待后台任务把用户与助手消息落库
    wait_until("指定 session 的消息已落库", || {
        let storage = Arc::clone(&storage);
        async move {
            storage
                .list_messages(sid)
                .await
                .map(|msgs| !msgs.is_empty())
                .unwrap_or(false)
        }
    })
    .await;

    // 验证消息已写入
    let msgs = storage.list_messages(sid).await.unwrap();
    assert!(!msgs.is_empty(), "消息应已写入指定 session");
}

// =========================================================
// L1 摘要 max_tokens 从 backend_config 传播
// =========================================================

/// backend_config.max_tokens 高于 L1 默认值时，L1 摘要请求使用 backend 值
/// （结构化输出预算过紧易被截断）。
#[tokio::test]
async fn l1_summary_uses_backend_config_max_tokens() {
    use ramaria_memory::l1::L1SummarizerConfig;

    // MockLlm 需返回合法 L1 JSON，确保封存走完整成功路径
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 自定义 backend_config：max_tokens = 2048（高于 L1 默认值）
    let mut bc = BackendConfig::lm_studio_default();
    bc.max_tokens = 2048;
    storage.save_backend_config(&bc).await.unwrap();

    // 发送消息创建活跃 session（消费事件流等待消息保存完成）
    let outcome = send_stream(&engine, "你好", None, None).await;
    assert!(
        !storage
            .list_active_sessions()
            .await
            .expect("读取活跃会话应成功")
            .is_empty(),
        "send_message 后应有活跃 session"
    );
    let sid = outcome.session_id;
    wait_until("用户与助手消息均已落库", || {
        let storage = Arc::clone(&storage);
        async move {
            storage
                .list_messages(sid)
                .await
                .map(|msgs| msgs.len() >= 2)
                .unwrap_or(false)
        }
    })
    .await;

    seal_full(&engine, sid).await.unwrap();

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
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 自定义 backend_config：max_tokens = 128（低于 L1 默认值）
    let mut bc = BackendConfig::lm_studio_default();
    bc.max_tokens = 128;
    storage.save_backend_config(&bc).await.unwrap();

    // 发送消息创建活跃 session（消费事件流等待消息保存完成）
    let outcome = send_stream(&engine, "你好", None, None).await;
    assert!(
        !storage
            .list_active_sessions()
            .await
            .expect("读取活跃会话应成功")
            .is_empty(),
        "send_message 后应有活跃 session"
    );
    let sid = outcome.session_id;
    wait_until("用户与助手消息均已落库", || {
        let storage = Arc::clone(&storage);
        async move {
            storage
                .list_messages(sid)
                .await
                .map(|msgs| msgs.len() >= 2)
                .unwrap_or(false)
        }
    })
    .await;

    seal_full(&engine, sid).await.unwrap();

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
// 封存归属统一以 DB sessions.persona_uid 为真相源
// =========================================================

/// 手动封存不接收调用方人格，但 DB 中 session 已绑定 persona →
/// L1 归属应取 DB 值，不依赖调用方内存态。
#[tokio::test]
async fn save_and_close_l1_uses_db_session_persona() {
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 发送消息：交互式生成创建并绑定 char-0001 的 session
    let outcome = send_stream(&engine, "你好", Some("char-0001"), None).await;
    let sid = outcome.session_id;
    wait_until("用户与助手消息均已落库", || {
        let storage = Arc::clone(&storage);
        async move {
            storage
                .list_messages(sid)
                .await
                .map(|msgs| msgs.len() >= 2)
                .unwrap_or(false)
        }
    })
    .await;

    // 封存以库内会话归属为准（seal 不接收调用方人格参数）
    seal_full(&engine, sid).await.unwrap();

    // L1 归属应为 DB 会话绑定的 char-0001，而非 NULL
    let l1s = storage.list_memory_l1(sid).await.unwrap();
    assert!(!l1s.is_empty(), "L1 应已生成");
    assert_eq!(
        l1s[0].persona_uid.as_deref(),
        Some("char-0001"),
        "L1 归属应取 DB sessions.persona_uid"
    );
}

/// 封存以库内会话归属为准：DB 已绑定时，L1 归属只取 DB 值，
/// 不依赖调用方内存态（seal 不接收人格参数）。
#[tokio::test]
async fn save_and_close_ignores_stale_input_persona() {
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    let outcome = send_stream(&engine, "你好", Some("char-0001"), None).await;
    let sid = outcome.session_id;
    wait_until("用户与助手消息均已落库", || {
        let storage = Arc::clone(&storage);
        async move {
            storage
                .list_messages(sid)
                .await
                .map(|msgs| msgs.len() >= 2)
                .unwrap_or(false)
        }
    })
    .await;

    // 封存以库内会话归属为准（seal 不接收调用方人格参数）
    seal_full(&engine, sid).await.unwrap();

    let l1s = storage.list_memory_l1(sid).await.unwrap();
    assert!(!l1s.is_empty(), "L1 应已生成");
    assert_eq!(
        l1s[0].persona_uid.as_deref(),
        Some("char-0001"),
        "过期前端内存值不应覆盖 DB 会话归属"
    );
}

// =========================================================
// 渐进式摘要封存协同
// =========================================================

/// 渐进式开启 + 长会话封存 → 分段生成**多段** absorbed=false
/// 的 L1（入候选池），而非单条整会话摘要。
///
/// 封存协同语义:
/// - 段 L1 全部 `absorbed=false`（`list_unabsorbed_l1` 天然可见）；
/// - 后续 `check_l2_trigger` 按未吸收 L1 计数时把段 L1 计入（候选池计数），
///   一次长会话即可达到触发阈值，无需等待多次会话累积；
/// - 尾段 L1 覆盖最新对话（按 `tail_msg_count` 切段、全段生成）。
#[tokio::test]
async fn save_and_close_progressive_long_session_writes_segment_l1s_to_candidate_pool() {
    const L1_JSON: &str = r#"{"summary":"用户讨论了长会话片段","keywords":"长会话,排期","time_period":"下午","atmosphere":"平静","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let mut config = RamariaConfig::default();
    config.l1.progressive.enabled = true;
    config.l1.progressive.msg_threshold = 4; // 6 条 > 4 → 触发
    config.l1.progressive.tail_msg_count = 2; // 每段 ≤ 2 条 → 6 条切 3 段
    let engine = build_ready_engine(&storage, &llm, config).await;

    // 手动创建绑定 char-0001 的会话，随后每轮**显式**发送到同一 session
    // （显式 session_id 复用已存在会话），避免自动创建分散到多 session。
    let session = storage.create_session(Some("char-0001")).await.unwrap();
    let sid = session.id;
    for i in 0..3 {
        let _outcome = send_stream(
            &engine,
            &format!("长会话片段消息 {i}"),
            Some("char-0001"),
            Some(sid),
        )
        .await;
        // 等待本轮用户与助手消息全部落库，确保封存时 6 条齐全
        let expected_len = 2 * (i as usize + 1);
        wait_until("本轮用户与助手消息均已落库", || {
            let storage = Arc::clone(&storage);
            async move {
                storage
                    .list_messages(sid)
                    .await
                    .map(|msgs| msgs.len() == expected_len)
                    .unwrap_or(false)
            }
        })
        .await;
    }
    let msgs = storage.list_messages(sid).await.unwrap();
    assert_eq!(msgs.len(), 6, "3 轮发送应同 session 累积 6 条消息");

    // 封存：渐进式感知管线应产出多段 absorbed=false 的 L1
    seal_full(&engine, sid).await.unwrap();

    let l1s = storage.list_memory_l1(sid).await.unwrap();
    assert_eq!(
        l1s.len(),
        3,
        "6 条消息按 tail=2 应生成 3 段 L1（封存协同的候选池地基），实际 {}",
        l1s.len()
    );
    assert!(
        l1s.iter().all(|l| !l.absorbed),
        "段 L1 必须 absorbed=false（L2 候选池可见）"
    );
    assert!(
        l1s.iter()
            .all(|l| l.persona_uid.as_deref() == Some("char-0001")),
        "段 L1 归属应来自 DB session persona"
    );
}

// =========================================================
// L1 失败任务补扫（pending 任务的消费点）
// =========================================================

/// 封存时 L1 失败登记的 pending 任务，在补扫时自动补跑并标记完成；
/// 已补跑（或已有 L1）的任务不会重复调用 LLM。
#[tokio::test]
async fn pending_l1_job_is_retried_and_completed() {
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let engine = build_ready_engine(&storage, &llm, RamariaConfig::default()).await;

    // 模拟"L1 生成失败"的遗留现场：已关闭、绑定 char-0001 且含消息的会话（无 L1）
    let session = storage.create_session(Some("char-0001")).await.unwrap();
    let sid = session.id;
    let msg = ramaria_core::types::Message::new(
        sid,
        MessageRole::User,
        "我们讨论一下项目安排".into(),
        MessageSource::Local,
    );
    storage.create_session_with_messages(sid, vec![msg]);
    storage.close_session(sid).await.unwrap();

    let payload = serde_json::json!({
        "session_id": sid.to_string(),
        "persona_uid": "char-0001",
        "reason": "auto_retry_on_close"
    })
    .to_string();
    // 补偿登记专用类型（与在途生成任务 l1_summary 区分，见 JobType::L1SummaryRetry）
    let job_id = storage.add_pending_job("l1_summary_retry", Some(&payload));

    // 补扫：应补跑 1 条并标记完成
    let retried = engine.retry_pending_l1_jobs().await;
    assert_eq!(retried, 1, "应有 1 条 L1 任务被补跑");

    let l1s = storage.list_memory_l1(sid).await.unwrap();
    assert!(!l1s.is_empty(), "补扫后 L1 摘要应已生成");
    assert_eq!(
        l1s[0].persona_uid.as_deref(),
        Some("char-0001"),
        "补扫生成的 L1 归属应取自任务 payload"
    );
    assert_eq!(
        storage.job_status(job_id).as_deref(),
        Some("completed"),
        "补跑成功后任务不应再 pending"
    );

    // 幂等：再次补扫不应重复调用 LLM（任务已完成、L1 已存在）
    let retried_again = engine.retry_pending_l1_jobs().await;
    assert_eq!(retried_again, 0, "已完成任务不应再次补跑");
}
