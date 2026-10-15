//! crates/ramaria-service/src/import/resume_tests.rs - Ramaria 导入"继续处理"用例
//!
//! 设计特点:
//! - 端到端：真实临时库 + 含图导出 + 共享 mock LLM，覆盖 附件回补 → L1 补跑 → 级联 三阶段
//! - 覆盖直连 / md5 反查定位、门禁保持待处理、幂等重跑与请求参数校验
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    AttachmentStatus, InboundAttachmentKind, Message, MessageAttachment, MessageRole,
    MessageSource, now_ms,
};
use uuid::Uuid;

use super::*;
use crate::test_support::{L1_JSON_REPLY, MockLlm, engine_with_shared_llm};

// ---- 测试脚手架 ----

/// 样例图片 md5（小写）。
const IMAGE_MD5: &str = "fccb86f2a1b695df3c37a1042a967a44";

/// 记录进度事件的测试 sink。
struct RecordingSink {
    events: Mutex<Vec<ImportL1Progress>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<ImportL1Progress> {
        self.events.lock().expect("进度事件锁不应中毒").clone()
    }
}

impl ImportProgressSink for RecordingSink {
    fn on_l1_progress(&self, p: &ImportL1Progress) {
        self.events
            .lock()
            .expect("进度事件锁不应中毒")
            .push(p.clone());
    }

    fn on_done(&self, _summary: &ImportDoneSummary) {}
}

/// 开启图片理解的配置（默认声明关）。
fn vision_on_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.vision.model_supports_vision = true;
    config
}

/// 装配"真实临时库 + 共享 mock LLM"引擎（配置按用例覆写）。
async fn resume_env(
    tag: &str,
    llm: Arc<MockLlm>,
    config: RamariaConfig,
) -> (Engine, Arc<ramaria_storage::SqliteStorage>, PathBuf) {
    engine_with_shared_llm(tag, llm, config, None).await
}

/// 默认"继续处理"请求（无导出根、不重置、无节流、不限量、无级联、不建缓存）。
fn continue_request() -> ImportContinueRequest {
    ImportContinueRequest {
        export_roots: Vec::new(),
        reset_skipped: false,
        throttle_ms: 0,
        l1_limit: 0,
        cascade_deep: false,
        cache_dir: None,
    }
}

/// 造一条含附件的消息（附件状态与引用由参数指定；返回消息 id）。
async fn seed_attachment_message(
    storage: &ramaria_storage::SqliteStorage,
    session_id: Uuid,
    source_ref: &str,
    md5: Option<&str>,
    status: AttachmentStatus,
) -> Uuid {
    let message = Message::new(
        session_id,
        MessageRole::User,
        "看这张图".to_string(),
        MessageSource::Local,
    );
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
    let now = now_ms();
    let attachment = MessageAttachment {
        id: 0,
        message_id: message.id,
        kind: InboundAttachmentKind::Image,
        source_ref: source_ref.to_string(),
        md5: md5.map(str::to_string),
        size: None,
        width: None,
        height: None,
        sub_type: Some("photo".to_string()),
        status,
        description: None,
        description_model: None,
        created_at: now,
        updated_at: now,
    };
    storage
        .insert_message_attachments(&[attachment])
        .await
        .expect("写入附件应成功");
    message.id
}

/// 读回消息的附件行（断言状态与引用）。
async fn load_attachments(
    storage: &ramaria_storage::SqliteStorage,
    message_id: Uuid,
) -> Vec<MessageAttachment> {
    storage
        .list_attachments_by_messages(&[message_id])
        .await
        .expect("读回附件应成功")
}

/// 造一个带消息的已关闭会话（无 L1；persona 归属随消息写入）。
///
/// 参数:
/// - `personas`: 消息的 persona 归属序列（None = 无归属；按序轮流分配给角色交替的消息）。
async fn seed_closed_session(
    storage: &ramaria_storage::SqliteStorage,
    personas: &[Option<&str>],
) -> Uuid {
    // messages.persona_uid 有外键：先确保 persona 行存在（幂等）
    for persona in personas.iter().flatten() {
        let exists = storage
            .get_persona_by_uid(persona)
            .await
            .expect("查询 persona 应成功");
        if exists.is_none() {
            crate::test_support::seed_persona(storage, persona).await;
        }
    }
    let session = storage.create_session(None).await.expect("创建会话应成功");
    for (index, persona) in personas.iter().enumerate() {
        let mut message = Message::new(
            session.id,
            if index % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            },
            format!("消息 {index}"),
            MessageSource::Local,
        );
        message.persona_uid = persona.map(str::to_string);
        message.created_at = now_ms() + index as i64 * 1000;
        storage
            .save_message(&message)
            .await
            .expect("写入消息应成功");
    }
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");
    session.id
}

/// 最小私聊导出（单 session 双方各一条；切割间隔 10 分钟）。
fn qq_private_export_json() -> String {
    let base = 1_701_000_000_000i64;
    let body = [
        (base, "u_self", "小明", "早上好"),
        (base + 60_000, "u_peer", "小红", "早上好呀"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (ts, uid, name, text))| {
        format!(
            r#"{{"id":"c_{i}","timestamp":{ts},"type":"text","recalled":false,"system":false,"content":{{"text":"{text}","elements":[]}},"sender":{{"uid":"{uid}","name":"{name}"}}}}"#
        )
    })
    .collect::<Vec<_>>()
    .join(",");
    format!(
        r#"{{"chatInfo":{{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"}},"messages":[{body}]}}"#
    )
}

/// 私聊 L0 导入请求。
fn private_import_request(file_path: &Path) -> ImportRequest {
    ImportRequest {
        file_path: file_path.to_path_buf(),
        mode: ImportMode::Fast,
        gap_minutes: 10,
        side: ramaria_importer::qq::ImportSide::Both,
        persona_name: None,
        self_persona_uid: None,
        other_persona_name: None,
        other_persona_uid: None,
    }
}

// ---- 附件回补 ----

/// 存量跳过回补：反查命中 → 重置 + 理解完成 + 引用修正；源文件缺失保持跳过。
#[tokio::test]
async fn continue_resets_locatable_skipped_via_direct_and_md5() {
    let llm =
        Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply("一只橘猫趴在窗台上。"));
    let (engine, storage, dir) = resume_env("resume-reset", llm, vision_on_config()).await;

    // 反查素材：resources/images/{md5}_{大写 md5}.jpg（文件名以 md5 开头）
    let file_name = "fccb86f2a1b695df3c37a1042a967a44_FCCB86F2A1B695DF3C37A1042A967A44.jpg";
    let image_path = dir.join("resources").join("images").join(file_name);
    std::fs::create_dir_all(image_path.parent().expect("图片文件应有父目录"))
        .expect("创建图片目录应成功");
    std::fs::write(&image_path, b"fake-jpeg-bytes").expect("写入图片应成功");

    let session = storage.create_session(None).await.expect("创建会话应成功");
    // 不可直连的服务器链接（md5 命中 → 反查兜底）
    let reverse_id = seed_attachment_message(
        &storage,
        session.id,
        "/download?fileid=1",
        Some(IMAGE_MD5),
        AttachmentStatus::Skipped,
    )
    .await;
    // 源文件缺失（md5 不命中任何文件）→ 保持跳过
    let missing_id = seed_attachment_message(
        &storage,
        session.id,
        "resources/images/missing.jpg",
        Some("00000000000000000000000000000000"),
        AttachmentStatus::Skipped,
    )
    .await;

    let mut req = continue_request();
    req.export_roots = vec![dir.clone()];
    req.reset_skipped = true;
    let outcome = engine
        .continue_import(req, None)
        .await
        .expect("继续处理应成功");

    assert_eq!(outcome.reset_count, 1, "仅可定位的存量跳过被重置");
    assert_eq!(outcome.reverse_lookup_hits, 1, "经 md5 反查命中");
    let stat = outcome.vision.expect("附件阶段应执行");
    assert_eq!(stat.scanned, 1, "仅重置行进入重扫");
    assert_eq!(stat.done, 1, "重置行应完成理解");

    // 反查行：done + 引用修正为命中相对引用；缺失行：保持 skipped
    let rows = load_attachments(&storage, reverse_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(
        rows[0].source_ref,
        format!("resources/images/{file_name}"),
        "重置时引用应修正为反查命中路径"
    );
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫趴在窗台上。"));
    let rows = load_attachments(&storage, missing_id).await;
    assert_eq!(
        rows[0].status,
        AttachmentStatus::Skipped,
        "无文件可定位保持跳过"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 缓存回填：传入缓存目录时，反查命中的源文件被写入客户端缓存。
#[tokio::test]
async fn continue_backfills_client_cache_for_reverse_hits() {
    let llm =
        Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply("一只橘猫趴在窗台上。"));
    let (engine, storage, dir) = resume_env("resume-cache", llm, vision_on_config()).await;

    // 反查素材：resources/images/{md5}_{大写 md5}.jpg（JPEG 魔数）
    let file_name = "fccb86f2a1b695df3c37a1042a967a44_FCCB86F2A1B695DF3C37A1042A967A44.jpg";
    let image_path = dir.join("resources").join("images").join(file_name);
    std::fs::create_dir_all(image_path.parent().expect("图片文件应有父目录"))
        .expect("创建图片目录应成功");
    let jpeg: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
    std::fs::write(&image_path, &jpeg).expect("写入图片应成功");

    let session = storage.create_session(None).await.expect("创建会话应成功");
    // 不可直连的服务器链接（md5 命中 → 反查兜底）
    seed_attachment_message(
        &storage,
        session.id,
        "/download?fileid=1",
        Some(IMAGE_MD5),
        AttachmentStatus::Skipped,
    )
    .await;

    let cache_dir = dir.join("image_cache");
    let mut req = continue_request();
    req.export_roots = vec![dir.clone()];
    req.reset_skipped = true;
    req.cache_dir = Some(cache_dir.clone());
    let outcome = engine
        .continue_import(req, None)
        .await
        .expect("继续处理应成功");
    assert_eq!(outcome.reset_count, 1);
    assert_eq!(outcome.reverse_lookup_hits, 1, "经 md5 反查命中");

    // 缓存文件落地：{md5}.jpg（JPEG 魔数 → jpg 扩展名），内容与源文件一致
    let cached = cache_dir.join(format!("{IMAGE_MD5}.jpg"));
    assert!(cached.is_file(), "反查命中应回填客户端缓存: {cached:?}");
    assert_eq!(std::fs::read(&cached).expect("读取缓存应成功"), jpeg);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 门禁声明关：附件保持待处理（不落库），统计 skipped 仍计入。
#[tokio::test]
async fn continue_gate_keeps_pending() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply("不会调用"));
    let (engine, storage, dir) = resume_env("resume-gate", llm, RamariaConfig::default()).await;
    // 有效导出根：含 resources/（用例聚焦门禁分支，根有效性由独立用例覆盖）
    std::fs::create_dir_all(dir.join("resources")).expect("创建 resources 目录应成功");

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_attachment_message(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some(IMAGE_MD5),
        AttachmentStatus::Pending,
    )
    .await;

    let mut req = continue_request();
    req.export_roots = vec![dir.clone()];
    let outcome = engine
        .continue_import(req, None)
        .await
        .expect("继续处理应成功");

    let stat = outcome.vision.expect("附件阶段应执行");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.skipped, 1, "门禁未过计入跳过统计");
    assert_eq!(stat.done, 0);
    // 库内状态零变化：行保持待处理（不被批量标记跳过）
    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Pending, "行保持待处理");
    assert_eq!(
        storage
            .list_all_skipped_attachments(0)
            .await
            .expect("查询不应失败")
            .len(),
        0,
        "不应产生跳过行"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 幂等重跑：二次执行零副作用（重置 / 扫描 / L1 补跑全为零）。
#[tokio::test]
async fn continue_idempotent() {
    let llm =
        Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply("一只橘猫趴在窗台上。"));
    let (engine, storage, dir) = resume_env("resume-idempotent", llm, vision_on_config()).await;

    // 直连可定位的存量跳过 + 缺 L1 的已关闭会话
    let file_path = dir.join("resources").join("images").join("a.jpg");
    std::fs::create_dir_all(file_path.parent().expect("图片文件应有父目录"))
        .expect("创建图片目录应成功");
    std::fs::write(&file_path, b"fake-jpeg-bytes").expect("写入图片应成功");
    let session = storage.create_session(None).await.expect("创建会话应成功");
    seed_attachment_message(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some(IMAGE_MD5),
        AttachmentStatus::Skipped,
    )
    .await;
    let closed = seed_closed_session(&storage, &[Some("char-0001"), Some("char-0001")]).await;

    let mut req = continue_request();
    req.export_roots = vec![dir.clone()];
    req.reset_skipped = true;
    let first = engine
        .continue_import(req.clone(), None)
        .await
        .expect("首次继续处理应成功");
    assert_eq!(first.reset_count, 1);
    assert_eq!(first.reverse_lookup_hits, 0, "直连命中不计反查");
    assert_eq!(first.vision.expect("附件阶段应执行").done, 1);
    assert_eq!(first.l1.l1_success, 1, "缺 L1 会话应被补跑");

    // 二次执行：存量已重置 / 待处理已清空 / 摘要已存在 → 全零
    let second = engine
        .continue_import(req, None)
        .await
        .expect("二次继续处理应成功");
    assert_eq!(second.reset_count, 0);
    assert_eq!(second.vision.expect("附件阶段应执行").scanned, 0);
    assert_eq!(second.l1.l1_success, 0);
    assert_eq!(second.l1.l1_total, 0);
    let rows = storage
        .list_memory_l1(closed)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(rows.len(), 1, "补跑结果保持一份，不重复生成");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- L1 补跑 ----

/// L1 补跑仅补缺失会话：已有摘要的会话不重复生成，进度序列为起始 + 逐条。
#[tokio::test]
async fn continue_l1_backfills_missing_only() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
    let (engine, storage, dir) = resume_env("resume-l1-only", llm, RamariaConfig::default()).await;

    // 会话 A：已手动生成摘要（私聊口径）
    let session_a = seed_closed_session(&storage, &[Some("char-0001"), Some("char-0001")]).await;
    super::l1::regenerate_l1_fanout(&engine, session_a, true)
        .await
        .expect("预生成 L1 应成功");
    // 会话 B：无摘要
    let session_b = seed_closed_session(&storage, &[Some("char-0002"), Some("char-0002")]).await;

    let sink = RecordingSink::new();
    let outcome = engine
        .continue_import(continue_request(), Some(&sink))
        .await
        .expect("继续处理应成功");

    assert_eq!(outcome.l1.l1_total, 1, "候选仅缺摘要的会话");
    assert_eq!(outcome.l1.l1_success, 1);
    assert_eq!(outcome.l1.l1_failed, 0);
    assert!(
        storage
            .list_memory_l1(session_a)
            .await
            .expect("读取 L1 应成功")
            .len()
            == 1,
        "已有摘要的会话保持一份"
    );
    assert_eq!(
        storage
            .list_memory_l1(session_b)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "缺摘要的会话被补跑"
    );

    // 进度序列：起始（0/1）+ 逐条（1/1），阶段均为 l1
    let events = sink.events();
    assert_eq!(events.len(), 2, "起始 + 每会话一条");
    assert!(events.iter().all(|e| e.phase == "l1"));
    assert_eq!(events[0].current, 0);
    assert_eq!(events[0].total, 1);
    assert_eq!(events[0].l1_total, Some(1));
    assert_eq!(events[1].current, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 风格统计触发：L1 补跑完成后按画像增量统计并落库（样本不足也算已触发）。
#[tokio::test]
async fn continue_triggers_style_stats_after_l1_backfill() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
    let (engine, storage, dir) =
        resume_env("resume-style-stats", llm, RamariaConfig::default()).await;

    seed_closed_session(&storage, &[Some("char-0001"), Some("char-0001")]).await;

    let outcome = engine
        .continue_import(continue_request(), None)
        .await
        .expect("继续处理应成功");
    assert_eq!(outcome.l1.l1_success, 1);

    let stats = engine
        .style_stats("char-0001")
        .await
        .expect("读取风格统计应成功")
        .expect("补跑后应触发风格统计并落库");
    assert!(stats.sample_count >= 1, "补跑会话的消息应计入风格统计样本");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 私聊导入续跑：继续处理产生双行摘要（双方 persona 各一、无无归属行、内容一致）。
#[tokio::test]
async fn continue_private_import_produces_dual_l1_rows() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
    let (engine, storage, dir) =
        resume_env("resume-private-dual", llm, RamariaConfig::default()).await;

    let export_path = dir.join("export.json");
    std::fs::write(&export_path, qq_private_export_json()).expect("写入导出文件应成功");
    let outcome = engine
        .import_qq_l0(private_import_request(&export_path))
        .await
        .expect("L0 导入应成功");
    assert_eq!(outcome.session_ids.len(), 1);

    let continue_outcome = engine
        .continue_import(continue_request(), None)
        .await
        .expect("继续处理应成功");
    assert_eq!(continue_outcome.l1.l1_success, 1);

    let rows = storage
        .list_memory_l1(outcome.session_ids[0])
        .await
        .expect("读取 L1 应成功");
    assert_eq!(rows.len(), 2, "私聊续跑应产生双方各一行");
    let personas: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.persona_uid.as_deref())
        .collect();
    assert_eq!(personas.len(), 2, "不应存在无归属行");
    assert!(
        personas.contains(&"user-10001"),
        "导出者行应存在: {personas:?}"
    );
    assert!(
        personas.contains(&"char-90002"),
        "对方行应存在: {personas:?}"
    );
    assert_eq!(
        rows[0].summary, rows[1].summary,
        "分发行摘要内容一致（同一生成结果复制）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 群聊续跑：继续处理的分发恒含"我"与块内他人——各 persona 一行、摘要内容一致。
#[tokio::test]
async fn continue_group_import_produces_rows_per_speaker() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
    let (engine, storage, dir) =
        resume_env("resume-group-fanout", llm, RamariaConfig::default()).await;

    let session = seed_closed_session(
        &storage,
        &[
            Some("user-10001"),
            Some("char-90002"),
            Some("user-10001"),
            Some("char-90003"),
        ],
    )
    .await;

    let outcome = engine
        .continue_import(continue_request(), None)
        .await
        .expect("继续处理应成功");
    assert_eq!(outcome.l1.l1_success, 1);

    let rows = storage
        .list_memory_l1(session)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(rows.len(), 3, "群聊续跑应产生三位发言者各一行");
    let personas: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.persona_uid.as_deref())
        .collect();
    assert_eq!(personas.len(), 3, "不应存在无归属行");
    for uid in ["user-10001", "char-90002", "char-90003"] {
        assert!(personas.contains(&uid), "应含 {uid} 行: {personas:?}");
    }
    assert!(
        rows.iter().all(|row| row.summary == rows[0].summary),
        "分发行摘要内容一致（同一生成结果复制）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 深度级联：cascade_deep=true 且补跑成功时触发 L2 → L3（进度含 l2 / l3 阶段）。
#[tokio::test]
async fn continue_cascade_deep_triggers_after_success() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
    let (engine, storage, dir) = resume_env("resume-cascade", llm, RamariaConfig::default()).await;

    seed_closed_session(&storage, &[Some("char-0003"), Some("char-0003")]).await;

    let sink = RecordingSink::new();
    let mut req = continue_request();
    req.cascade_deep = true;
    let outcome = engine
        .continue_import(req, Some(&sink))
        .await
        .expect("继续处理应成功");

    assert_eq!(outcome.l1.l1_success, 1);
    assert!(outcome.l2_triggered, "补跑成功应触发 L2");
    assert!(outcome.l3_triggered, "补跑成功应触发 L3");
    let events = sink.events();
    assert!(
        events.iter().any(|e| e.phase == "l2") && events.iter().any(|e| e.phase == "l3"),
        "级联进度应含 l2 / l3 阶段"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 请求校验 ----

/// 无效导出根（无 resources 目录且无导出 JSON）：附件步骤停（不标记任何附件行），L1 补跑照常。
#[tokio::test]
async fn continue_invalid_root_skips_attachment_steps_but_keeps_l1() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply("不会调用"));
    let (engine, storage, dir) = resume_env("resume-invalid-root", llm, vision_on_config()).await;

    // 待处理附件（根无效时不得被标记为跳过）+ 缺 L1 的已关闭会话
    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_attachment_message(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some(IMAGE_MD5),
        AttachmentStatus::Pending,
    )
    .await;
    seed_closed_session(&storage, &[Some("char-0005"), Some("char-0005")]).await;

    // 目录存在但不含 resources / 导出 JSON → 无效导出根
    let mut req = continue_request();
    req.export_roots = vec![dir.clone()];
    let outcome = engine
        .continue_import(req, None)
        .await
        .expect("无效根不应整体中止");

    assert!(outcome.vision.is_none(), "附件阶段应被跳过");
    assert_eq!(outcome.reset_count, 0, "不应重置任何行");
    // 附件行保持待处理（绝不被批量标记跳过），也不产生跳过行
    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(
        rows[0].status,
        AttachmentStatus::Pending,
        "附件行应保持待处理"
    );
    assert_eq!(
        storage
            .list_all_skipped_attachments(0)
            .await
            .expect("查询不应失败")
            .len(),
        0,
        "不应产生跳过行"
    );
    assert_eq!(outcome.l1.l1_success, 1, "L1 补跑应照常执行");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 导出根非法：不存在的目录以业务校验错误拒绝。
#[tokio::test]
async fn continue_export_root_invalid_rejected() {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
    let (engine, _storage, dir) =
        resume_env("resume-bad-root", llm, RamariaConfig::default()).await;

    let mut req = continue_request();
    req.export_roots = vec![dir.join("does-not-exist")];
    let err = engine
        .continue_import(req, None)
        .await
        .expect_err("导出根不存在应报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.context().contains("导出目录不存在或不是目录"),
        "错误应说明导出根非法: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
