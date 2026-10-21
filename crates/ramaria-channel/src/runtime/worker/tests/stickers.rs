//! crates/ramaria-channel/src/runtime/worker/tests/stickers.rs - 通道表情回合单元测试
//!
//! 设计特点:
//! - 指令剥离与资产命中：命中随首段发出，未命中降级纯文字
//! - 仅表情回复：传输支持纯附件时发空文本带附件，否则退化为表情占位文本
//! - 资产按数据库同级目录造数，全部离线

use super::*;

// =========================================================
// 表情指令
// =========================================================

/// 表情指令剥离：无资产命中时按纯文字发送（标记不残留在出站文本）。
#[tokio::test]
async fn sticker_directive_stripped_when_asset_missing() {
    let llm = StubLlm::new("送你[表情:猫]到家", "不应调用");
    let (engine, storage, dir) = worker_engine("sticker-miss", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-sticker"),
    )
    .await;

    let sends = transport.sends.lock().expect("出站记录锁不应中毒").clone();
    assert_eq!(sends.len(), 1, "无资产命中应只发纯文字一条");
    assert_eq!(sends[0].1.text, "送你到家", "标记应剥离且不残留");
    assert_eq!(sends[0].1.attachment, None, "未命中不应携带附件");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 仅表情指令且无资产命中：净化后为空，跳过发送。
#[tokio::test]
async fn sticker_only_reply_without_asset_skips_send() {
    let llm = StubLlm::new("[表情:猫]", "不应调用");
    let (engine, storage, dir) = worker_engine("sticker-only", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-sticker-only"),
    )
    .await;

    assert_eq!(transport.attempts(), 0, "无附件且无文本应跳过发送");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 仅表情指令且资产命中、传输支持纯附件：发送空文本带附件的一条消息。
#[tokio::test]
async fn sticker_only_reply_sends_attachment_when_supported() {
    let llm = StubLlm::new("[表情:猫]", "不应调用");
    let (engine, storage, dir) = worker_engine("sticker-hit", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    seed_sticker(&storage, &dir, "猫", b"PNGDATA").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-sticker-hit"),
    )
    .await;

    assert_eq!(transport.attempts(), 1, "纯附件应一次发出");
    let sends = transport.sends.lock().expect("出站记录锁不应中毒").clone();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].1.text, "", "支持附件时正文保持为空");
    assert!(
        matches!(
            sends[0].1.attachment,
            Some(OutboundAttachment::Bytes { .. })
        ),
        "支持附件时应携带表情字节"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 仅表情指令且资产命中、传输不支持纯附件：退化为表情占位文本，一次发出不重试。
#[tokio::test]
async fn sticker_only_reply_falls_back_to_text_when_unsupported() {
    let llm = StubLlm::new("[表情:猫]", "不应调用");
    let (engine, storage, dir) = worker_engine("sticker-text-fallback", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    seed_sticker(&storage, &dir, "猫", b"PNGDATA").await;
    let transport = StubTransport::new();
    transport.set_supports_attachment(false);
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-sticker-text"),
    )
    .await;

    assert_eq!(transport.attempts(), 1, "占位文本应一次发出（不触发重试）");
    assert_eq!(transport.sent_texts(), vec!["[表情:猫]".to_string()]);
    let sends = transport.sends.lock().expect("出站记录锁不应中毒").clone();
    assert_eq!(sends[0].1.attachment, None, "退化文本不应携带附件");

    let _ = std::fs::remove_dir_all(&dir);
}
