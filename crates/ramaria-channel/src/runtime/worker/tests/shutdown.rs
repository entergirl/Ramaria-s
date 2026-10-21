//! crates/ramaria-channel/src/runtime/worker/tests/shutdown.rs - 通道编排关停单元测试
//!
//! 设计特点:
//! - 事件流终止：循环退出后可正常复用句柄
//! - 在途编排可打断：生成挂起时停止立即生效，未完成的回复不发送

use super::*;

/// 事件队列空转：桩传输 next_event 返回 None 即事件流终止（循环退出）。
#[tokio::test]
async fn worker_shutdown_stops_loop() {
    let llm = StubLlm::new("不应调用", "不应调用");
    let (engine, storage, dir) = worker_engine("worker-shutdown", llm).await;
    seed_binding_persona(&storage).await;
    let transport: Arc<dyn Transport> = StubTransport::new();
    let downloader = StubDownloader::failing();
    let runtime = Arc::new(ChannelRuntime::new(
        crate::config::ChannelRuntimeConfig {
            default_persona_uid: "rama-0001".to_string(),
            qq: None,
            telegram: None,
            proactive_enabled: false,
        },
        Arc::new(crate::events::NoopSink),
    ));

    let worker = ChannelWorker::spawn(
        Arc::clone(&engine),
        transport,
        Arc::clone(&runtime),
        downloader,
        worker_options(),
    );
    assert!(Arc::ptr_eq(worker.runtime(), &runtime));
    worker.shutdown().await;

    let _ = std::fs::remove_dir_all(&dir);
}

/// 关停打断在途编排：生成挂起时停止立即生效，未完成的回复不发送。
#[tokio::test]
async fn worker_shutdown_abandons_inflight_reply() {
    let llm = StubLlm::new("不应发出", "不应调用");
    llm.set_hang_chat();
    let (engine, storage, dir) = worker_engine("worker-shutdown-inflight", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = Arc::new(EventOnceTransport::new(private_event("你好", "m-hang")));
    let downloader = StubDownloader::failing();
    let runtime = Arc::new(ChannelRuntime::new(
        crate::config::ChannelRuntimeConfig {
            default_persona_uid: "rama-0001".to_string(),
            qq: None,
            telegram: None,
            proactive_enabled: false,
        },
        Arc::new(crate::events::NoopSink),
    ));

    let worker = ChannelWorker::spawn(
        Arc::clone(&engine),
        Arc::clone(&transport) as Arc<dyn Transport>,
        Arc::clone(&runtime),
        downloader,
        worker_options(),
    );

    // 等待事件进入生成（此时编排挂在 LLM 调用上）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while llm.chat_calls() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "等待回复生成超时");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::timeout(Duration::from_secs(2), worker.shutdown())
        .await
        .expect("关停应放弃在途回复并立即返回");
    assert!(transport.sent().is_empty(), "在途回复不应发出");

    let _ = std::fs::remove_dir_all(&dir);
}
