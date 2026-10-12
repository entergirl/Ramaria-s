//! crates/ramaria-channel/tests/telegram_e2e.rs - Telegram 通道端到端用例（mock Bot API）
//!
//! 设计特点:
//! - 全链路：mock Bot API 长轮询投递更新 → 客户端解析 / 下载 / 落库 → 服务层生成 →
//!   出站 `sendMessage`（HTTP 请求—应答）
//! - 真实 SQLite 临时库 + 桩 LLM；图片链路使用真实 [`TelegramFileDownloader`]（两步下载）
//! - 覆盖私聊 / 群 @ / 群回复触发与不触发 / 重复事件幂等 / 图片链路
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实账号 / 网络调用 / 用户数据。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::telegram::MockTelegramServer;
use common::{
    MiniLlm, RecordingSink, StubImageDownloader, e2e_engine, e2e_runtime, seed_binding_persona,
    wait_for,
};
use ramaria_channel::{
    ChannelEventSink, ChannelWorker, ImageDownloader, TelegramFileDownloader,
    TelegramRuntimeConfig, TelegramTransport, TelegramTransportOptions, Transport, WorkerOptions,
};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{AttachmentStatus, MessageRole};
use ramaria_storage::SqliteStorage;
use serde_json::{Value, json};

// =========================================================
// 装配与辅助
// =========================================================

/// e2e 上下文（mock 服务端 / 存储 / 传输 / 编排任务）。
struct E2e {
    server: MockTelegramServer,
    engine: Arc<ramaria_service::Engine>,
    storage: Arc<SqliteStorage>,
    transport: Arc<TelegramTransport>,
    worker: Option<ChannelWorker>,
    dir: std::path::PathBuf,
}

/// 装配一套完整链路并建立连接。
async fn setup(tag: &str, server: MockTelegramServer, downloader: Arc<dyn ImageDownloader>) -> E2e {
    let llm = MiniLlm::new("你好，我是 Ramaria。", "一只橘猫");
    let (engine, storage, dir) = e2e_engine(tag, llm).await;
    seed_binding_persona(&storage).await;

    let runtime = e2e_runtime();
    let sink = Arc::new(RecordingSink::default());
    let transport = Arc::new(TelegramTransport::new(
        TelegramRuntimeConfig {
            bot_token: "test-token".to_string(),
        },
        TelegramTransportOptions {
            api_base_url: server.base_url().to_string(),
            poll_timeout_secs: 1,
            request_timeout: Duration::from_secs(2),
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(400),
            empty_batch_delay: Duration::from_millis(20),
        },
        Arc::clone(&sink) as Arc<dyn ChannelEventSink>,
    ));
    let worker = ChannelWorker::spawn(
        Arc::clone(&engine),
        Arc::clone(&transport) as Arc<dyn Transport>,
        Arc::clone(&runtime),
        downloader,
        WorkerOptions {
            min_send_interval: Duration::from_millis(10),
            send_retry_delay: Duration::from_millis(10),
            send_max_attempts: 2,
            ..WorkerOptions::default()
        },
    );
    transport.connect().await.expect("连接应成功");
    E2e {
        server,
        engine,
        storage,
        transport,
        worker: Some(worker),
        dir,
    }
}

/// 装配一套桩下载器链路（非图片用例）。
async fn setup_stub(tag: &str) -> E2e {
    let server = MockTelegramServer::start("test-token", Vec::new()).await;
    setup(tag, server, StubImageDownloader::failing()).await
}

impl E2e {
    /// 显式放行绑定（自动建档默认禁用；白名单语义：启用后才响应）。
    async fn allow(&self, external_ref: &str) {
        self.engine
            .channel_set_binding("telegram", external_ref, None, Some(true), None)
            .await
            .expect("显式放行绑定应成功");
    }

    /// 等待客户端连接上线。
    async fn wait_online(&self) {
        let transport = Arc::clone(&self.transport);
        wait_for("通道上线", Duration::from_secs(5), move || {
            let transport = Arc::clone(&transport);
            async move { transport.status().is_online() }
        })
        .await;
    }

    /// 等待 `sendMessage` 累计达到 N 条。
    async fn wait_sent(&self, count: usize) {
        let server = &self.server;
        wait_for("出站发送到达", Duration::from_secs(5), move || async move {
            server.sent_messages().len() >= count
        })
        .await;
    }

    /// 等待指定会话消息数达标（返回消息列表）。
    async fn wait_messages(
        &self,
        external_ref: &str,
        count: usize,
    ) -> Vec<ramaria_core::types::Message> {
        let storage = Arc::clone(&self.storage);
        let probe_ref = external_ref.to_string();
        wait_for("消息落库", Duration::from_secs(5), move || {
            let storage = Arc::clone(&storage);
            let external_ref = probe_ref.clone();
            async move {
                let Ok(Some(session)) = storage
                    .find_active_session_by_channel("telegram", Some(&external_ref))
                    .await
                else {
                    return false;
                };
                storage
                    .list_messages(session.id)
                    .await
                    .map(|messages| messages.len() >= count)
                    .unwrap_or(false)
            }
        })
        .await;
        let session = self
            .storage
            .find_active_session_by_channel("telegram", Some(external_ref))
            .await
            .expect("查询会话应成功")
            .expect("会话应已创建");
        self.storage
            .list_messages(session.id)
            .await
            .expect("查询消息应成功")
    }

    /// 关停编排与传输并清理临时目录。
    async fn teardown(self) {
        self.transport.shutdown().await.expect("关停应成功");
        if let Some(worker) = self.worker {
            worker.shutdown().await;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// =========================================================
// 更新样例
// =========================================================

/// 私聊文本更新。
fn private_text_update(update_id: i64, message_id: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": message_id,
            "from": { "id": 10001, "is_bot": false, "first_name": "小明" },
            "chat": { "id": 10001, "first_name": "小明", "type": "private" },
            "date": 1_700_000_000,
            "text": text
        }
    })
}

/// 群内 @ 本机更新（mention 实体覆盖 `@testbot`）。
fn group_mention_update(update_id: i64, message_id: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": message_id,
            "from": { "id": 10001, "is_bot": false, "first_name": "小明" },
            "chat": { "id": -1001234567890i64, "title": "测试群", "type": "supergroup" },
            "date": 1_700_000_001,
            "text": text,
            "entities": [{ "offset": 0, "length": 8, "type": "mention" }]
        }
    })
}

/// 群内回复更新（被回复者 ID 由调用方指定）。
fn group_reply_update(update_id: i64, message_id: i64, reply_from: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": message_id,
            "from": { "id": 10001, "is_bot": false, "first_name": "小明" },
            "chat": { "id": -1001234567890i64, "title": "测试群", "type": "supergroup" },
            "date": 1_700_000_002,
            "text": text,
            "reply_to_message": {
                "message_id": 900,
                "from": { "id": reply_from, "is_bot": reply_from == 42, "first_name": "对方" },
                "date": 1_700_000_000,
                "text": "上一条"
            }
        }
    })
}

/// 私聊图片更新（photo 数组含小图与大图，大图 file_id 由调用方指定）。
fn private_photo_update(update_id: i64, message_id: i64, file_id: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": message_id,
            "from": { "id": 10001, "is_bot": false, "first_name": "小明" },
            "chat": { "id": 10001, "first_name": "小明", "type": "private" },
            "date": 1_700_000_003,
            "photo": [
                { "file_id": "small", "file_unique_id": "u1", "width": 90, "height": 60, "file_size": 1024 },
                { "file_id": file_id, "file_unique_id": "u2", "width": 900, "height": 600, "file_size": 20480 }
            ]
        }
    })
}

// =========================================================
// 用例
// =========================================================

/// 私聊全链路：更新 → 落库（用户 + 助手两行）→ sendMessage（聊天 ID 与回复文本）。
#[tokio::test]
async fn private_message_end_to_end() {
    let ctx = setup_stub("private").await;
    ctx.wait_online().await;
    ctx.allow("c2c:10001").await;

    ctx.server.push_update(private_text_update(1, 101, "你好"));
    ctx.wait_sent(1).await;

    let sent = ctx.server.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["chat_id"], 10001);
    assert_eq!(sent[0]["text"], "你好，我是 Ramaria。");

    let messages = ctx.wait_messages("c2c:10001", 2).await;
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::User),
        "应有用户行"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant),
        "应有助手行"
    );

    ctx.teardown().await;
}

/// 群 @ 触发：mention 实体命中 → 以群会话（负聊天 ID）回发。
#[tokio::test]
async fn group_mention_end_to_end() {
    let ctx = setup_stub("group-mention").await;
    ctx.wait_online().await;
    ctx.allow("group:-1001234567890").await;

    ctx.server
        .push_update(group_mention_update(2, 201, "@testbot 在吗"));
    ctx.wait_sent(1).await;

    let sent = ctx.server.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["chat_id"], -1001234567890i64, "群聊天 ID 应为负值");
    assert_eq!(sent[0]["text"], "你好，我是 Ramaria。");

    let messages = ctx.wait_messages("group:-1001234567890", 2).await;
    assert_eq!(messages.len(), 2);

    ctx.teardown().await;
}

/// 群回复判定：回复 Bot 触发；回复他人不触发（仅自身一行入库，无发送）。
#[tokio::test]
async fn group_reply_decision_end_to_end() {
    let ctx = setup_stub("group-reply").await;
    ctx.wait_online().await;
    ctx.allow("group:-1001234567890").await;

    // 回复 Bot → 触发
    ctx.server
        .push_update(group_reply_update(3, 301, 42, "回复你"));
    ctx.wait_sent(1).await;
    let messages = ctx.wait_messages("group:-1001234567890", 2).await;
    assert_eq!(messages.len(), 2, "首轮应有用户行与助手行");

    // 回复他人 → 不触发：第二条仅自身一行入库，无第二次发送
    ctx.server
        .push_update(group_reply_update(4, 302, 99, "回复别人"));
    let messages = ctx.wait_messages("group:-1001234567890", 3).await;
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .count(),
        1,
        "回复他人不应产生助手行"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ctx.server.sent_messages().len(), 1, "回复他人不应触发发送");

    ctx.teardown().await;
}

/// 重复更新幂等：同平台消息 ID（不同 update_id）→ 仅一次写入与一次发送。
#[tokio::test]
async fn duplicate_update_single_write_and_reply() {
    let ctx = setup_stub("duplicate").await;
    ctx.wait_online().await;
    ctx.allow("c2c:10001").await;

    ctx.server
        .push_update(private_text_update(10, 401, "重复消息"));
    ctx.server
        .push_update(private_text_update(11, 401, "重复消息"));

    ctx.wait_sent(1).await;
    let messages = ctx.wait_messages("c2c:10001", 2).await;

    // 稳定窗口：确认重复更新没有产生第二次写入与回复
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ctx.server.sent_messages().len(), 1, "重复更新不应二次回复");
    assert_eq!(messages.len(), 2, "重复更新不应二次入库");

    ctx.teardown().await;
}

/// 图片链路：photo → getFile 两步下载（真实下载器）→ 占位符落库 → 理解回填。
#[tokio::test]
async fn image_message_end_to_end() {
    let image_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let server = MockTelegramServer::start("test-token", image_bytes.clone()).await;
    let downloader: Arc<dyn ImageDownloader> = Arc::new(TelegramFileDownloader::with_options(
        "test-token",
        server.base_url(),
        Duration::from_secs(2),
        0,
    ));
    let ctx = setup("image", server, downloader).await;
    ctx.wait_online().await;
    ctx.allow("c2c:10001").await;

    ctx.server
        .push_update(private_photo_update(20, 501, "file-1"));
    ctx.wait_sent(1).await;

    // 占位符落库：md5 前 8 位
    let messages = ctx.wait_messages("c2c:10001", 2).await;
    let user_row = messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    let md5 = ramaria_channel::runtime::assemble::md5_hex(&image_bytes);
    let expected_hash = md5[..8].to_string();
    assert!(
        user_row
            .content
            .contains(&format!("[图片#{expected_hash}]")),
        "正文应含占位符: {}",
        user_row.content
    );

    // 图片理解为后台任务：等待附件行回填完成
    let storage = Arc::clone(&ctx.storage);
    let message_id = user_row.id;
    let rows = {
        wait_for("图片理解回填", Duration::from_secs(5), {
            let storage = Arc::clone(&storage);
            move || {
                let storage = Arc::clone(&storage);
                async move {
                    storage
                        .list_attachments_by_messages(&[message_id])
                        .await
                        .map(|rows| {
                            rows.first()
                                .is_some_and(|row| row.status == AttachmentStatus::Done)
                        })
                        .unwrap_or(false)
                }
            }
        })
        .await;
        storage
            .list_attachments_by_messages(&[message_id])
            .await
            .expect("查询附件应成功")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫"));
    assert_eq!(rows[0].source_ref, "file-1", "附件源引用应为平台文件标识");
    assert_eq!(rows[0].md5.as_deref(), Some(md5.as_str()));

    // 两步下载均发生：getFile + 文件下载
    let calls = ctx.server.calls();
    assert!(calls.contains(&"getFile".to_string()), "应调用 getFile");
    assert!(calls.contains(&"file".to_string()), "应下载文件字节");

    ctx.teardown().await;
}
