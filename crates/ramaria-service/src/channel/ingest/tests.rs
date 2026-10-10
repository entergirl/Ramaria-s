//! crates/ramaria-service/src/channel/ingest/tests.rs - 社交通道入站写入单元测试
//!
//! 设计特点:
//! - 真实 SQLite 临时库 + 无回复 / 固定回复 mock LLM，不依赖外部服务
//! - 覆盖会话建档与续写、平台消息幂等、多发言者归属、本机侧映射、
//!   人格锚点复用与 seq 递增、改绑、空闲封存另起、附件行与校验矩阵
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实账号 / 网络调用 / 用户数据。

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    AttachmentStatus, InboundAttachmentKind, InboundAttachmentRef, InboundMessage, InboundSender,
    MessageRole, Persona, PersonaKind, now_ms,
};

use crate::channel::ingest::run;
use crate::recall::RecallPolicy;
use crate::test_support::{L1_JSON_REPLY, engine_with_db, engine_with_l1_reply, seed_persona};
use crate::types::{ChannelIngestRequest, DEFAULT_PERSONA_UID};

// =========================================================
// 测试脚手架
// =========================================================

/// 构造发送者（uin / 昵称固定；群名片可选）。
fn sender(uin: &str, display_name: &str) -> InboundSender {
    InboundSender {
        platform_id: uin.to_string(),
        uin: Some(uin.to_string()),
        display_name: display_name.to_string(),
        group_nickname: None,
        role: None,
    }
}

/// 构造规范入站消息（无附件）。
fn inbound(
    platform_message_id: Option<&str>,
    uin: &str,
    display_name: &str,
    text: &str,
    created_at: i64,
) -> InboundMessage {
    InboundMessage {
        channel: "qq".to_string(),
        platform_message_id: platform_message_id.map(str::to_string),
        reply_to: None,
        created_at,
        text: text.to_string(),
        sender: sender(uin, display_name),
        attachments: Vec::new(),
    }
}

/// 构造写入请求（通道 qq；本机侧人格默认）。
fn request(external_ref: &str, message: InboundMessage) -> ChannelIngestRequest {
    ChannelIngestRequest {
        channel: "qq".to_string(),
        external_ref: external_ref.to_string(),
        persona_uid: DEFAULT_PERSONA_UID.to_string(),
        self_id: None,
        message,
    }
}

// =========================================================
// 会话与幂等
// =========================================================

/// 首条消息：建档会话（通道 / 外部标识 / 归属）并落库消息行（人类成员锚点）。
#[tokio::test]
async fn first_message_creates_session_and_persists() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-first").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let created_at = now_ms();
    let outcome = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-1"), "10001", "小明", "你好", created_at),
        ),
    )
    .await
    .expect("写入应成功");

    assert!(outcome.written);
    assert!(outcome.images.is_empty());

    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert_eq!(session.channel, "qq");
    assert_eq!(session.external_ref.as_deref(), Some("c2c:10001"));
    assert_eq!(session.persona_uid.as_deref(), Some(DEFAULT_PERSONA_UID));

    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.role, MessageRole::User);
    assert_eq!(message.persona_uid.as_deref(), Some("char-10001"));
    assert_eq!(message.sender_ref.as_deref(), Some("10001"));
    assert_eq!(message.sender_name.as_deref(), Some("小明"));
    assert_eq!(message.platform_message_id.as_deref(), Some("m-1"));
    assert_eq!(message.created_at, created_at);
    assert!(message.fingerprint.is_some());

    let anchor = storage
        .get_persona_by_uid("char-10001")
        .await
        .expect("查询人格应成功")
        .expect("成员锚点应已创建");
    assert_eq!(anchor.kind, PersonaKind::Char);
    assert_eq!(anchor.source, "qq");
    assert_eq!(anchor.ref_id.as_deref(), Some("10001"));

    let members = storage
        .list_session_members(session.id)
        .await
        .expect("查询成员应成功");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].platform_ref, "10001");
    assert_eq!(members[0].name, "小明");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 续写：同一外部标识的第二条消息落入同一会话。
#[tokio::test]
async fn second_message_continues_same_session() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-continue").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let first = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
        ),
    )
    .await
    .expect("首条应成功");
    let second = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-2"), "10001", "小明", "在吗", now_ms()),
        ),
    )
    .await
    .expect("第二条应成功");

    assert_eq!(first.session_id, second.session_id);
    let messages = storage
        .list_messages(first.session_id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 幂等：平台消息 ID 重复的事件不产生二次写入。
#[tokio::test]
async fn duplicate_platform_message_is_idempotent() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-idempotent").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let first = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-dup"), "10001", "小明", "重复", now_ms()),
        ),
    )
    .await
    .expect("首条应成功");
    let second = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-dup"), "10001", "小明", "重复", now_ms()),
        ),
    )
    .await
    .expect("重复事件应正常返回");

    assert!(first.written);
    assert!(!second.written);
    assert_eq!(first.session_id, second.session_id);
    assert!(second.images.is_empty());
    let messages = storage
        .list_messages(first.session_id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 1, "重复事件不应二次入库");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无平台消息 ID：按内容事件指纹兜底（同内容同时间重复视为重复）。
#[tokio::test]
async fn fingerprint_fallback_without_platform_id() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-fallback").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let created_at = 1_700_000_000_000;
    let first = run(
        &engine,
        request(
            "c2c:10001",
            inbound(None, "10001", "小明", "兜底", created_at),
        ),
    )
    .await
    .expect("首条应成功");
    let second = run(
        &engine,
        request(
            "c2c:10001",
            inbound(None, "10001", "小明", "兜底", created_at),
        ),
    )
    .await
    .expect("重复应正常返回");
    assert!(first.written);
    assert!(!second.written);

    // 不同时间戳视为不同事件，正常写入
    let third = run(
        &engine,
        request(
            "c2c:10001",
            inbound(None, "10001", "小明", "兜底", created_at + 1),
        ),
    )
    .await
    .expect("不同事件应成功");
    assert!(third.written);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 发言人归属
// =========================================================

/// 双发言者：同一会话内各自锚点人格、成员两行。
#[tokio::test]
async fn two_speakers_share_session_with_anchors() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-two-speakers").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let first = run(
        &engine,
        request(
            "group:20002",
            inbound(Some("g-1"), "10001", "小明", "大家好", now_ms()),
        ),
    )
    .await
    .expect("成员一应成功");
    let second = run(
        &engine,
        request(
            "group:20002",
            inbound(Some("g-2"), "20002", "小红", "你好", now_ms()),
        ),
    )
    .await
    .expect("成员二应成功");

    assert_eq!(first.session_id, second.session_id);
    let messages = storage
        .list_messages(first.session_id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2);
    let personas: Vec<Option<&str>> = messages
        .iter()
        .map(|message| message.persona_uid.as_deref())
        .collect();
    assert!(personas.contains(&Some("char-10001")));
    assert!(personas.contains(&Some("char-20002")));

    let members = storage
        .list_session_members(first.session_id)
        .await
        .expect("查询成员应成功");
    assert_eq!(members.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 本机侧消息（self_id 命中）：归属绑定人格、角色 assistant、不建成员锚点。
#[tokio::test]
async fn self_message_maps_to_binding_persona() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-self").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let mut req = request(
        "c2c:10001",
        inbound(Some("m-self"), "42", "Ramaria", "本机回复", now_ms()),
    );
    req.self_id = Some("42".to_string());
    let outcome = run(&engine, req).await.expect("本机消息应成功");

    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert_eq!(
        messages[0].persona_uid.as_deref(),
        Some(DEFAULT_PERSONA_UID)
    );
    assert!(
        storage
            .get_persona_by_uid("char-42")
            .await
            .expect("查询人格应成功")
            .is_none(),
        "本机侧不应创建成员锚点"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 成员锚点：已存在时复用；新建时 seq 按同来源最大值递增。
#[tokio::test]
async fn char_persona_reuse_and_seq_increment() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-anchor").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_persona(&storage, "char-10001").await;
    let old_anchor = Persona::new(
        "char-9999".to_string(),
        "旧成员".to_string(),
        PersonaKind::Char,
        7,
        "qq".to_string(),
    );
    storage
        .create_persona(&old_anchor)
        .await
        .expect("预置人格应成功");

    // 已存在锚点：复用（不重复创建）
    let first = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-a"), "10001", "小明", "你好", now_ms()),
        ),
    )
    .await
    .expect("复用路径应成功");
    assert!(first.written);

    // 新锚点：seq = 同来源最大值 7 + 1 = 8
    run(
        &engine,
        request(
            "c2c:20002",
            inbound(Some("m-b"), "20002", "小红", "你好", now_ms()),
        ),
    )
    .await
    .expect("新建路径应成功");
    let created = storage
        .get_persona_by_uid("char-20002")
        .await
        .expect("查询人格应成功")
        .expect("新锚点应已创建");
    assert_eq!(created.seq, 8);
    assert_eq!(created.name, "小红");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 改绑：会话归属与绑定人格不一致时更新会话归属后续写。
#[tokio::test]
async fn rebind_updates_session_persona() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-rebind").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_persona(&storage, "char-0001").await;

    let first = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
        ),
    )
    .await
    .expect("首条应成功");

    // 模拟手工改绑：会话归属改为他人格
    storage
        .bind_session_persona_uid(first.session_id, "char-0001")
        .await
        .expect("预置改绑应成功");

    let second = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-2"), "10001", "小明", "在吗", now_ms()),
        ),
    )
    .await
    .expect("续写应成功");
    assert_eq!(first.session_id, second.session_id);

    let session = storage
        .get_session(first.session_id)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert_eq!(
        session.persona_uid.as_deref(),
        Some(DEFAULT_PERSONA_UID),
        "归属应改回绑定人格"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 空闲封存与附件
// =========================================================

/// 空闲超阈值：旧会话封存关闭，新消息另起会话。
#[tokio::test]
async fn idle_session_sealed_and_new_session_started() {
    let (engine, storage, dir) = engine_with_l1_reply("channel-ingest-idle", L1_JSON_REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let first = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-1"), "10001", "小明", "先聊到这", now_ms()),
        ),
    )
    .await
    .expect("首条应成功");

    // 直连库把消息时间改到 2 小时前（超过 l1_idle_minutes=10 的空闲阈值）
    let pool = engine.sqlite_pool().expect("测试引擎应附着连接池");
    let stale_ts = now_ms() - 2 * 60 * 60 * 1000;
    sqlx::query("UPDATE messages SET created_at = ? WHERE session_id = ?")
        .bind(stale_ts)
        .bind(first.session_id.to_string())
        .execute(&pool)
        .await
        .expect("回拨时间应成功");

    let second = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-2"), "10001", "小明", "回来说话", now_ms()),
        ),
    )
    .await
    .expect("新消息应成功");

    assert_ne!(second.session_id, first.session_id, "空闲应另起新会话");
    let old_session = storage
        .get_session(first.session_id)
        .await
        .expect("查询旧会话应成功")
        .expect("旧会话应存在");
    assert!(old_session.ended_at.is_some(), "旧会话应已被封存关闭");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 图片附件：行落库为 pending 并返回定位（与附件引用同序）。
#[tokio::test]
async fn image_attachment_rows_written_pending() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-image").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let mut message = inbound(
        Some("m-img"),
        "10001",
        "小明",
        "看这个[图片#8895abcd]",
        now_ms(),
    );
    message.attachments.push(InboundAttachmentRef {
        kind: InboundAttachmentKind::Image,
        source_ref: "https://example.com/a.png".to_string(),
        md5: Some("8895abcd11223344556677889900aabb".to_string()),
        size: Some(2048),
        width: None,
        height: None,
        sub_type: Some("0".to_string()),
    });
    let outcome = run(&engine, request("c2c:10001", message))
        .await
        .expect("含图消息应成功");

    assert_eq!(outcome.images.len(), 1);
    assert!(outcome.images[0].attachment_id > 0);
    assert_eq!(outcome.images[0].source_ref, "https://example.com/a.png");

    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("查询消息应成功");
    let stored = storage
        .list_attachments_by_messages(&[messages[0].id])
        .await
        .expect("查询附件应成功");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, AttachmentStatus::Pending);
    assert_eq!(
        stored[0].md5.as_deref(),
        Some("8895abcd11223344556677889900aabb")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 校验矩阵
// =========================================================

/// 校验：空通道 / 空外部标识 / 空人格 / 人格不存在 / 空正文均显式拒绝。
#[tokio::test]
async fn validation_rejects_invalid_requests() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-validation").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let mut empty_channel = request(
        "c2c:10001",
        inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
    );
    empty_channel.channel = "  ".to_string();
    assert!(
        run(&engine, empty_channel)
            .await
            .expect_err("空通道应拒绝")
            .category()
            == "validation"
    );

    let mut empty_ref = request(
        "c2c:10001",
        inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
    );
    empty_ref.external_ref = String::new();
    assert!(
        run(&engine, empty_ref)
            .await
            .expect_err("空外部标识应拒绝")
            .category()
            == "validation"
    );

    let mut empty_persona = request(
        "c2c:10001",
        inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
    );
    empty_persona.persona_uid = " ".to_string();
    assert!(
        run(&engine, empty_persona)
            .await
            .expect_err("空人格应拒绝")
            .category()
            == "validation"
    );

    let mut missing_persona = request(
        "c2c:10001",
        inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
    );
    missing_persona.persona_uid = "char-missing".to_string();
    assert!(
        run(&engine, missing_persona)
            .await
            .expect_err("人格不存在应拒绝")
            .category()
            == "validation"
    );

    let empty_text = request(
        "c2c:10001",
        inbound(Some("m-1"), "10001", "小明", "   ", now_ms()),
    );
    assert!(
        run(&engine, empty_text)
            .await
            .expect_err("空正文应拒绝")
            .category()
            == "validation"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 人格白名单：越权人格按隐私域拒绝。
#[tokio::test]
async fn persona_whitelist_rejects_ingest() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-whitelist").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
    );

    let err = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
        ),
    )
    .await
    .expect_err("越权人格应被拒绝");
    assert_eq!(err.category(), "privacy");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 白名单拒绝时不产生任何写入。
#[tokio::test]
async fn whitelist_rejection_leaves_no_side_effects() {
    let (engine, storage, dir) = engine_with_db("channel-ingest-whitelist-clean").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
    );

    let _ = run(
        &engine,
        request(
            "c2c:10001",
            inbound(Some("m-1"), "10001", "小明", "你好", now_ms()),
        ),
    )
    .await
    .expect_err("越权人格应被拒绝");

    let active = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功");
    assert!(active.is_none(), "拒绝路径不应留下会话残留");
    assert!(
        storage
            .get_persona_by_uid("char-10001")
            .await
            .expect("查询人格应成功")
            .is_none(),
        "拒绝路径不应创建成员锚点"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
