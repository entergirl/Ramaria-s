//! crates/ramaria-channel/tests/channel_e2e.rs - QQ 通道端到端用例（mock NapCat）
//!
//! 设计特点:
//! - 全链路：mock 协议端推送事件 → 客户端解析 / 下载 / 落库 → 服务层生成 →
//!   出站发送（echo 配对）经同一连接返回
//! - 真实 SQLite 临时库 + 桩 LLM / 桩下载器：不依赖外部服务与网络
//! - 覆盖私聊 / 群 @ / 重复事件幂等 / 断线重连恢复 / 图片链路
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实账号 / 网络调用 / 用户数据。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    MiniLlm, MockNapCatServer, RecordingSink, StubImageDownloader, e2e_engine, e2e_runtime,
    e2e_transport, seed_binding_persona, wait_for,
};
use ramaria_channel::{
    ChannelEventSink, ChannelWorker, ConnectionState, ImageDownloader, OneBotTransport, Transport,
    WorkerOptions,
};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{AttachmentStatus, MessageRole};
use ramaria_storage::SqliteStorage;
use serde_json::json;

// =========================================================
// 装配与辅助
// =========================================================

/// e2e 上下文（mock 协议端 / 存储 / 传输 / 编排任务）。
struct E2e {
    server: MockNapCatServer,
    storage: Arc<SqliteStorage>,
    sink: Arc<RecordingSink>,
    transport: Arc<OneBotTransport>,
    worker: Option<ChannelWorker>,
    dir: std::path::PathBuf,
}

/// 装配一套完整链路并建立连接。
async fn setup(tag: &str, token: Option<&str>, downloader: Arc<dyn ImageDownloader>) -> E2e {
    let server = MockNapCatServer::start(token).await;
    let llm = MiniLlm::new("你好，我是 Ramaria。", "一只橘猫");
    let (engine, storage, dir) = e2e_engine(tag, llm).await;
    seed_binding_persona(&storage).await;

    let runtime = e2e_runtime();
    let sink = Arc::new(RecordingSink::default());
    let transport = e2e_transport(
        &server,
        Arc::clone(&sink) as Arc<dyn ChannelEventSink>,
        token,
    );
    let worker = ChannelWorker::spawn(
        Arc::clone(&engine),
        Arc::clone(&transport) as Arc<dyn Transport>,
        Arc::clone(&runtime),
        downloader,
        WorkerOptions {
            min_send_interval: Duration::from_millis(10),
            send_retry_delay: Duration::from_millis(10),
            send_max_attempts: 2,
        },
    );
    transport.connect().await.expect("连接应成功");
    E2e {
        server,
        storage,
        sink,
        transport,
        worker: Some(worker),
        dir,
    }
}

impl E2e {
    /// 等待客户端连接上线。
    async fn wait_online(&self) {
        let transport = Arc::clone(&self.transport);
        wait_for("通道上线", Duration::from_secs(5), move || {
            let transport = Arc::clone(&transport);
            async move { transport.status().is_online() }
        })
        .await;
    }

    /// 等待指定动作的请求累计达到 N 条。
    async fn wait_requests(&self, action: &'static str, count: usize) {
        let server = &self.server;
        wait_for("出站请求到达", Duration::from_secs(5), move || async move {
            server.received_actions(action).len() >= count
        })
        .await;
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

/// 私聊文本事件。
fn private_event(text: &str, message_id: i64) -> String {
    json!({
        "post_type": "message",
        "message_type": "private",
        "sub_type": "friend",
        "message_id": message_id,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_000,
        "message": [{ "type": "text", "data": { "text": text } }],
        "sender": { "user_id": 10001, "nickname": "小明" }
    })
    .to_string()
}

/// 群聊 @ 本机事件。
fn group_at_event(text: &str, message_id: i64) -> String {
    json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "message_id": message_id,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_001,
        "message": [
            { "type": "at", "data": { "qq": "42" } },
            { "type": "text", "data": { "text": text } }
        ],
        "sender": { "user_id": 10001, "nickname": "小明" }
    })
    .to_string()
}

/// 私聊图片事件。
fn private_image_event(url: &str, message_id: i64) -> String {
    json!({
        "post_type": "message",
        "message_type": "private",
        "message_id": message_id,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_002,
        "message": [{
            "type": "image",
            "data": { "url": url, "file_size": 8, "sub_type": "0" }
        }],
        "sender": { "user_id": 10001, "nickname": "小明" }
    })
    .to_string()
}

// =========================================================
// 用例
// =========================================================

/// 私聊全链路：收事件 → 落库 → 生成 → 出站发送（echo 配对）。
#[tokio::test]
async fn private_message_end_to_end() {
    let ctx = setup("private", None, StubImageDownloader::failing()).await;
    ctx.wait_online().await;

    ctx.server.push_event(&private_event("你好", 101));
    ctx.wait_requests("send_private_msg", 1).await;

    let sent = ctx.server.received_actions("send_private_msg");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["params"]["user_id"], 10001);
    assert_eq!(
        sent[0]["params"]["message"][0]["data"]["text"],
        "你好，我是 Ramaria。"
    );
    assert!(
        sent[0]["echo"]
            .as_str()
            .is_some_and(|echo| echo.starts_with("ramaria-")),
        "出站请求应携带配对 echo: {}",
        sent[0]
    );

    // 库内：会话按外部标识建档；消息为入站用户行 + 生成助手行
    let session = ctx
        .storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应已创建");
    let messages = ctx
        .storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::User)
    );
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant)
    );

    ctx.teardown().await;
}

/// 群 @ 触发：事件带 at 本机段 → 以群接口回发。
#[tokio::test]
async fn group_mention_end_to_end() {
    let ctx = setup("group-mention", None, StubImageDownloader::failing()).await;
    ctx.wait_online().await;

    ctx.server.push_event(&group_at_event("在吗", 201));
    ctx.wait_requests("send_group_msg", 1).await;

    let sent = ctx.server.received_actions("send_group_msg");
    assert_eq!(sent[0]["params"]["group_id"], 20002);
    assert_eq!(
        sent[0]["params"]["message"][0]["data"]["text"],
        "你好，我是 Ramaria。"
    );
    assert!(
        ctx.server.received_actions("send_private_msg").is_empty(),
        "群聊回复不应走私聊接口"
    );

    let session = ctx
        .storage
        .find_active_session_by_channel("qq", Some("group:20002"))
        .await
        .expect("查询会话应成功")
        .expect("群会话应已创建");
    let user_row = ctx
        .storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功")
        .into_iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    assert!(user_row.content.contains("@42"), "At 段应折叠进正文");

    ctx.teardown().await;
}

/// 重复事件幂等：同平台消息 ID 推送两次 → 库内一条用户行、仅一次回复。
#[tokio::test]
async fn duplicate_event_single_write_and_reply() {
    let ctx = setup("duplicate", None, StubImageDownloader::failing()).await;
    ctx.wait_online().await;

    let event = private_event("重复消息", 301);
    ctx.server.push_event(&event);
    ctx.server.push_event(&event);

    // 等第一条链路完成（助手行落库 + 出站发送）
    ctx.wait_requests("send_private_msg", 1).await;
    let storage = Arc::clone(&ctx.storage);
    wait_for("库内两条消息", Duration::from_secs(5), move || {
        let storage = Arc::clone(&storage);
        async move {
            let Ok(Some(session)) = storage
                .find_active_session_by_channel("qq", Some("c2c:10001"))
                .await
            else {
                return false;
            };
            storage
                .list_messages(session.id)
                .await
                .map(|messages| messages.len() == 2)
                .unwrap_or(false)
        }
    })
    .await;

    // 稳定窗口：确认重复事件没有产生第二次写入与回复
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        ctx.server.received_actions("send_private_msg").len(),
        1,
        "重复事件不应二次回复"
    );
    let session = ctx
        .storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let messages = ctx
        .storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2, "重复事件不应二次入库");

    ctx.teardown().await;
}

/// 断线重连：服务端断开 → 客户端退避重连 → 第二连接链路继续。
#[tokio::test]
async fn disconnect_reconnects_and_resumes() {
    let ctx = setup("reconnect", None, StubImageDownloader::failing()).await;
    ctx.wait_online().await;

    ctx.server.push_event(&private_event("第一条", 401));
    ctx.wait_requests("send_private_msg", 1).await;

    ctx.server.disconnect_all();
    let server = &ctx.server;
    wait_for("重连建立", Duration::from_secs(5), move || async move {
        server.connections() >= 2
    })
    .await;
    ctx.wait_online().await;

    // 状态序列应出现错误态与第二次上线
    let states = ctx.sink.states();
    assert!(
        states.contains(&ConnectionState::Error),
        "断线应产生错误态: {states:?}"
    );
    assert!(
        states
            .iter()
            .filter(|state| **state == ConnectionState::Online)
            .count()
            >= 2,
        "应出现重连后的第二次上线: {states:?}"
    );

    ctx.server.push_event(&private_event("第二条", 402));
    ctx.wait_requests("send_private_msg", 2).await;
    let sent = ctx.server.received_actions("send_private_msg");
    assert_eq!(
        sent[1]["params"]["message"][0]["data"]["text"],
        "你好，我是 Ramaria。"
    );

    ctx.teardown().await;
}

/// 图片链路：收图 → 下载 → 占位符落库 → 现场理解回填 → 回复。
#[tokio::test]
async fn image_message_end_to_end() {
    let image_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let downloader = StubImageDownloader::succeeding(&image_bytes);
    let ctx = setup("image", None, downloader).await;
    ctx.wait_online().await;

    ctx.server
        .push_event(&private_image_event("https://example.com/a.png", 501));
    ctx.wait_requests("send_private_msg", 1).await;

    let session = ctx
        .storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应已创建");
    let user_row = ctx
        .storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功")
        .into_iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    let expected_hash = ramaria_channel::runtime::assemble::md5_hex(&image_bytes)[..8].to_string();
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
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫"));

    ctx.teardown().await;
}
