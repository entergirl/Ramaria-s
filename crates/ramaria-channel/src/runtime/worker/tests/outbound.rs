//! crates/ramaria-channel/src/runtime/worker/tests/outbound.rs - 通道出站发送单元测试
//!
//! 设计特点:
//! - 拆条与段间节奏：`||` 切段、段数上限折叠、逐段发送间隔
//! - 发送重试：可重试错误按上限重试，不可重试错误直接放弃
//! - 全部离线：桩传输记录出站内容与发送时刻，不依赖真实平台

use super::*;

// =========================================================
// 拆条与分次发送
// =========================================================

/// 出站拆条：含 `||` 的回复在发送前切段分次发出（库内原文不改）。
#[tokio::test]
async fn outbound_pipe_separator_is_split() {
    let llm = StubLlm::new("你好 || 在的||怎么啦", "不应调用");
    let (engine, storage, dir) = worker_engine("outbound-pipe", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-pipe"),
    )
    .await;

    assert_eq!(
        transport.sent_texts(),
        vec!["你好".to_string(), "在的".to_string(), "怎么啦".to_string()],
        "出站文本应按分隔符拆条，不残留分隔符"
    );
    assert_eq!(transport.attempts(), 3, "每段一次发送");

    // 库内回复原文保留多气泡契约（仅出站拆条）
    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let assistant = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功")
        .into_iter()
        .find(|message| message.role == MessageRole::Assistant)
        .expect("应有助手行");
    assert_eq!(assistant.content, "你好 || 在的||怎么啦");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 多段分次发送：逐段发出，相邻段发送时刻差满足段间最小间隔。
#[tokio::test]
async fn outbound_multi_segment_sends_with_interval() {
    let llm = StubLlm::new("甲||乙||丙", "不应调用");
    let (engine, storage, dir) = worker_engine("outbound-interval", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-seg"),
    )
    .await;

    assert_eq!(
        transport.sent_texts(),
        vec!["甲".to_string(), "乙".to_string(), "丙".to_string()]
    );
    assert_eq!(transport.attempts(), 3);

    let times = transport.send_times();
    assert_eq!(times.len(), 3);
    for pair in times.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(
            gap + Duration::from_millis(1) >= Duration::from_millis(10),
            "相邻段发送间隔应不小于 10ms: {gap:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 段数超限：超出上限的余段以换行并入末段后发送。
#[tokio::test]
async fn outbound_segment_overflow_folds_into_last() {
    let llm = StubLlm::new("甲||乙||丙", "不应调用");
    let (engine, storage, dir) = worker_engine("outbound-fold", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    let mut options = worker_options();
    options.segment_max = 2;
    dispatch(
        &engine,
        &transport,
        &downloader,
        &options,
        private_event("你好", "m-fold"),
    )
    .await;

    assert_eq!(
        transport.sent_texts(),
        vec!["甲".to_string(), "乙\n丙".to_string()],
        "超限余段应并入末段"
    );
    assert_eq!(transport.attempts(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 发送重试
// =========================================================

/// 发送可重试失败一次后成功：尝试两次、最终发出。
#[tokio::test]
async fn send_retry_recovers_after_one_failure() {
    let llm = StubLlm::new("重试后成功", "不应调用");
    let (engine, storage, dir) = worker_engine("send-retry", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    transport.fail_next(1, true);
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-retry"),
    )
    .await;

    assert_eq!(transport.attempts(), 2, "应重试一次");
    assert_eq!(transport.sent_texts(), vec!["重试后成功".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 发送不可重试失败：仅尝试一次、不发出（回复已落库）。
#[tokio::test]
async fn send_non_retryable_failure_not_retried() {
    let llm = StubLlm::new("不会发出", "不应调用");
    let (engine, storage, dir) = worker_engine("send-no-retry", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    transport.fail_next(1, false);
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-no-retry"),
    )
    .await;

    assert_eq!(transport.attempts(), 1, "不可重试错误不应重试");
    assert!(transport.sent_texts().is_empty());

    // 回复已落库（生成不因发送失败回滚）
    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}
