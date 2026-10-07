//! crates/ramaria-service/src/vision/tests.rs - Ramaria 图片理解用例单元测试
//!
//! 设计特点:
//! - 真实临时 SQLite + 真实临时图片文件 + mock LLM（不依赖外部服务）
//! - 覆盖门禁三分支（声明 / 隐私 / 探测）、md5 去重、批量上限、失败重试与逐行跳过
//! - base64 编码标准向量与描述清洗纯函数断言
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{
    AttachmentStatus, BackendConfig, InboundAttachmentKind, Message, MessageAttachment,
    MessageRole, MessageSource, now_ms,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

use super::*;
use crate::engine::Engine;
use crate::test_support::{MockLlm, engine_with_shared_llm};

// =========================================================
// 测试脚手架
// =========================================================

/// 装配"真实库 + 共享 mock LLM"引擎（配置按用例覆写）。
async fn vision_engine(
    tag: &str,
    llm: Arc<MockLlm>,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_shared_llm(tag, llm, config, None).await
}

/// 开启图片理解的配置（默认声明关）。
fn vision_on_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.vision.model_supports_vision = true;
    config
}

/// 在导出根写一个真实图片文件。
fn write_image(dir: &Path, relative: &str, bytes: &[u8]) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("创建图片目录应成功");
    }
    std::fs::write(&path, bytes).expect("写入图片应成功");
}

/// 造一条含附件指定状态的图片消息（返回消息 id）。
async fn seed_attachment_row(
    storage: &SqliteStorage,
    session_id: Uuid,
    source_ref: &str,
    md5: Option<&str>,
    status: AttachmentStatus,
    description: Option<&str>,
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
        description: description.map(str::to_string),
        description_model: description.map(|_| "先前模型".to_string()),
        created_at: now,
        updated_at: now,
    };
    storage
        .insert_message_attachments(&[attachment])
        .await
        .expect("写入附件应成功");
    message.id
}

/// 造一条 pending 图片附件消息（返回消息 id）。
async fn seed_pending_attachment(
    storage: &SqliteStorage,
    session_id: Uuid,
    source_ref: &str,
    md5: Option<&str>,
) -> Uuid {
    seed_attachment_row(
        storage,
        session_id,
        source_ref,
        md5,
        AttachmentStatus::Pending,
        None,
    )
    .await
}

/// 读回消息的附件行（断言状态与描述）。
async fn load_attachments(storage: &SqliteStorage, message_id: Uuid) -> Vec<MessageAttachment> {
    storage
        .list_attachments_by_messages(&[message_id])
        .await
        .expect("读回附件应成功")
}

// =========================================================
// 纯函数
// =========================================================

/// base64 编码匹配 RFC 4648 标准向量（含空输入与三种补齐长度）。
#[test]
fn base64_encode_matches_rfc4648_vectors() {
    let cases: &[(&[u8], &str)] = &[
        (b"", ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
    ];
    for (input, expected) in cases {
        assert_eq!(base64_encode(input), *expected, "输入 {input:?}");
    }
}

/// 描述清洗：引号剥离 / 换行与控制字符折叠 / 200 字符截断 / 空串。
#[test]
fn sanitize_description_strips_quotes_folds_whitespace_and_truncates() {
    // 成对引号剥离（循环剥离多层）
    assert_eq!(sanitize_description("\"一只猫\""), "一只猫");
    assert_eq!(sanitize_description("'一只猫'"), "一只猫");
    assert_eq!(sanitize_description("“一只猫”"), "一只猫");
    assert_eq!(sanitize_description("「一只猫」"), "一只猫");
    assert_eq!(sanitize_description("““双层””"), "双层");
    // 换行 / 控制字符 / 连续空白折叠
    assert_eq!(sanitize_description("第一行\n第二行"), "第一行 第二行");
    assert_eq!(sanitize_description("多  个\t空白"), "多 个 空白");
    assert_eq!(sanitize_description("  a\u{0007}b  "), "a b");
    // 空与近空输入
    assert_eq!(sanitize_description(""), "");
    assert_eq!(sanitize_description("   "), "");
    assert_eq!(sanitize_description("\"\"\"\""), "");
    // 不成对引号不剥离
    assert_eq!(sanitize_description("\"半只猫"), "\"半只猫");
    assert_eq!(sanitize_description("\""), "\"");
    // 200 字符截断（按字符而非字节）
    let long = "好".repeat(300);
    assert_eq!(sanitize_description(&long).chars().count(), 200);
}

/// 图片 MIME 按扩展名映射，未知扩展名回退 image/jpeg。
#[test]
fn image_mime_follows_extension_with_fallback() {
    assert_eq!(image_mime_for("a.jpg"), "image/jpeg");
    assert_eq!(image_mime_for("a.JPEG"), "image/jpeg");
    assert_eq!(image_mime_for("a.png"), "image/png");
    assert_eq!(image_mime_for("a.gif"), "image/gif");
    assert_eq!(image_mime_for("a.webp"), "image/webp");
    assert_eq!(image_mime_for("a.bmp"), "image/bmp");
    assert_eq!(image_mime_for("a.unknown"), "image/jpeg");
    assert_eq!(image_mime_for("noext"), "image/jpeg");
}

// =========================================================
// 理解任务
// =========================================================

/// 成功理解：pending + 真实文件 → 调用一次并回填描述与模型标识。
#[tokio::test]
async fn understand_success_fills_description() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("一只橘猫坐在窗台上"));
    let (engine, storage, dir) =
        vision_engine("vision-ok", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    write_image(&dir, "resources/images/a.jpg", b"fake-jpeg-bytes");
    let message_id = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(
        stat,
        VisionRunStat {
            scanned: 1,
            done: 1,
            reused: 0,
            failed: 0,
            skipped: 0,
        }
    );

    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫坐在窗台上"));
    // MockLlm::local 的模型 ID 为空 → 模型标识回退 provider 稳定标识
    assert_eq!(rows[0].description_model.as_deref(), Some("lm_studio"));
    // 探测 1 次 + 内容理解 1 次
    assert_eq!(llm.vision_call_count(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// md5 复用：同 md5 两个 pending 行仅调用一次，两行均 done。
#[tokio::test]
async fn understand_reuses_description_for_same_md5() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("窗台边的橘猫"));
    let (engine, storage, dir) =
        vision_engine("vision-md5-reuse", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    write_image(&dir, "resources/images/a.jpg", b"fake-jpeg-bytes");
    let md5 = "aabbccddeeff00112233445566778899";
    let first =
        seed_pending_attachment(&storage, session.id, "resources/images/a.jpg", Some(md5)).await;
    let second =
        seed_pending_attachment(&storage, session.id, "resources/images/a.jpg", Some(md5)).await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 2);
    assert_eq!(stat.done, 1, "仅第一行真实调用理解");
    assert_eq!(stat.reused, 1, "第二行命中 md5 复用");
    assert_eq!(stat.done + stat.reused, 2, "两行均完成（回填）");
    assert_eq!(stat.failed, 0);
    assert_eq!(stat.skipped, 0);
    // 探测 1 次 + 内容理解 1 次：同 md5 不重复调用
    assert_eq!(llm.vision_call_count(), 2);

    for message_id in [first, second] {
        let rows = load_attachments(&storage, message_id).await;
        assert_eq!(rows[0].status, AttachmentStatus::Done);
        assert_eq!(rows[0].description.as_deref(), Some("窗台边的橘猫"));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 已有完成描述：新 pending 零内容调用直接复用回填（保留原描述模型）。
#[tokio::test]
async fn understand_reuses_existing_done_description_without_calling() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("不会被使用的回复"));
    let (engine, storage, dir) =
        vision_engine("vision-reuse-done", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let md5 = "11223344556677889900aabbccddeeff";
    // 预置同 md5 的 done 行（描述来自"先前模型"）
    let _done_message = seed_attachment_row(
        &storage,
        session.id,
        "resources/images/old.jpg",
        Some(md5),
        AttachmentStatus::Done,
        Some("先前的描述"),
    )
    .await;
    // 新 pending 行（文件不存在——复用路径不读取文件）
    let pending_message =
        seed_pending_attachment(&storage, session.id, "resources/images/new.jpg", Some(md5)).await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 1, "仅 pending 行被扫描");
    assert_eq!(stat.reused, 1);
    assert_eq!(stat.done, 0);
    // 仅探测 1 次：内容零调用
    assert_eq!(llm.vision_call_count(), 1);

    let rows = load_attachments(&storage, pending_message).await;
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(rows[0].description.as_deref(), Some("先前的描述"));
    assert_eq!(
        rows[0].description_model.as_deref(),
        Some("先前模型"),
        "复用保留原描述模型"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 批量上限：`[vision].batch_limit=2` 时每会话仅扫描前 2 行（0 = 不限由其它用例覆盖）。
#[tokio::test]
async fn understand_respects_batch_limit_per_session() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("图片描述"));
    let mut config = vision_on_config();
    config.vision.batch_limit = 2;
    let (engine, storage, dir) = vision_engine("vision-batch", Arc::clone(&llm), config).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let mut messages = Vec::new();
    for index in 0..3 {
        let relative = format!("resources/images/{index}.jpg");
        write_image(&dir, &relative, b"bytes");
        let md5 = format!("{index:0>32}");
        messages.push(seed_pending_attachment(&storage, session.id, &relative, Some(&md5)).await);
    }

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 2, "batch_limit=2 仅扫描前 2 行");
    assert_eq!(stat.done, 2);
    // 探测 1 次 + 内容理解 2 次
    assert_eq!(llm.vision_call_count(), 3);

    for message_id in &messages[..2] {
        let rows = load_attachments(&storage, *message_id).await;
        assert_eq!(rows[0].status, AttachmentStatus::Done);
    }
    let third = load_attachments(&storage, messages[2]).await;
    assert_eq!(
        third[0].status,
        AttachmentStatus::Pending,
        "超限行保持待处理"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 声明关（默认配置）：全部标 skipped，零调用。
#[tokio::test]
async fn understand_skips_all_when_declared_off() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("不会调用"));
    let (engine, storage, dir) =
        vision_engine("vision-off", Arc::clone(&llm), RamariaConfig::default()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.skipped, 1);
    assert_eq!(stat.done + stat.reused + stat.failed, 0);
    assert_eq!(llm.vision_call_count(), 0);

    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Skipped);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 隐私未确认（线上 provider）：保持 pending，零调用（门禁在探测前短路）。
#[tokio::test]
async fn understand_keeps_pending_when_privacy_pending() {
    let llm = Arc::new(MockLlm::online().with_vision_reply("不会调用"));
    let (engine, storage, dir) =
        vision_engine("vision-privacy", Arc::clone(&llm), vision_on_config()).await;
    storage
        .save_backend_config(&BackendConfig::deepseek_default())
        .await
        .expect("保存后端配置应成功");

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.done + stat.reused + stat.failed + stat.skipped, 0);
    assert_eq!(llm.vision_call_count(), 0, "隐私未确认不触发探测");

    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Pending, "保持待处理");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 探测失败：全部 skipped 且失败结论缓存，后续批次不重复发送探测请求。
#[tokio::test]
async fn understand_probe_failure_skips_and_caches_failure() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("x").vision_failing(100));
    let (engine, storage, dir) =
        vision_engine("vision-probe-fail", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let first = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.skipped, 1);
    assert_eq!(llm.vision_call_count(), 1, "仅探测一次请求");
    let rows = load_attachments(&storage, first).await;
    assert_eq!(rows[0].status, AttachmentStatus::Skipped);

    // 第二次：扫描新 pending → 探测结论缓存命中，不再发请求
    let second = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/b.jpg",
        Some("bbccddeeff00112233445566778899aa"),
    )
    .await;
    let stat2 = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("第二次理解任务应成功");
    assert_eq!(stat2.scanned, 1);
    assert_eq!(stat2.skipped, 1);
    assert_eq!(llm.vision_call_count(), 1, "探测失败已缓存，不重复发送");
    let rows2 = load_attachments(&storage, second).await;
    assert_eq!(rows2[0].status, AttachmentStatus::Skipped);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 调用失败重试：第一次失败后重试成功计 done；两次均失败标 failed。
#[tokio::test]
async fn understand_retries_failed_call_once_then_marks_failed() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("重试成功后的描述"));
    let (engine, storage, dir) =
        vision_engine("vision-retry", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    write_image(&dir, "resources/images/a.jpg", b"fake");
    let first = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    // 第一次：探测成功 + 内容成功（建立探测缓存）
    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("第一次理解任务应成功");
    assert_eq!(stat.done, 1);
    assert_eq!(llm.vision_call_count(), 2);

    // 第二次：内容调用失败 1 次 → 重试成功
    llm.set_vision_failures(1);
    let second = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("bbccddeeff00112233445566778899aa"),
    )
    .await;
    let stat2 = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("第二次理解任务应成功");
    assert_eq!(stat2.scanned, 1);
    assert_eq!(stat2.done, 1, "重试成功计 done");
    assert_eq!(stat2.failed, 0);
    assert_eq!(
        llm.vision_call_count(),
        4,
        "探测缓存命中：失败 1 次 + 重试 1 次"
    );
    let rows = load_attachments(&storage, second).await;
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(rows[0].description.as_deref(), Some("重试成功后的描述"));

    // 第三次：两次尝试均失败 → failed（不落描述）
    llm.set_vision_failures(10);
    let third = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("ccddeeff00112233445566778899aabb"),
    )
    .await;
    let stat3 = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("第三次理解任务应成功");
    assert_eq!(stat3.scanned, 1);
    assert_eq!(stat3.failed, 1);
    assert_eq!(llm.vision_call_count(), 6);
    let rows3 = load_attachments(&storage, third).await;
    assert_eq!(rows3[0].status, AttachmentStatus::Failed);
    assert!(rows3[0].description.is_none(), "失败不落描述");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空描述（清洗后为空）→ failed 不落描述。
#[tokio::test]
async fn understand_marks_failed_on_empty_description() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("“”"));
    let (engine, storage, dir) =
        vision_engine("vision-empty-desc", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    write_image(&dir, "resources/images/a.jpg", b"fake");
    let message_id = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.failed, 1);

    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Failed);
    assert!(rows[0].description.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 文件缺失与超大文件：均标 skipped（零内容调用；超大用稀疏文件构造）。
#[tokio::test]
async fn understand_skips_missing_and_oversized_files() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("不会用于这两行"));
    let (engine, storage, dir) =
        vision_engine("vision-skip-files", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let missing = seed_pending_attachment(
        &storage,
        session.id,
        "resources/images/missing.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    // 21 MiB 稀疏文件（set_len 不写数据）
    let big_relative = "resources/images/big.jpg";
    let big_path = dir.join(big_relative);
    std::fs::create_dir_all(big_path.parent().expect("应有父目录")).expect("创建目录应成功");
    let file = std::fs::File::create(&big_path).expect("创建大文件应成功");
    file.set_len(21 * 1024 * 1024).expect("设置文件长度应成功");
    drop(file);
    let big = seed_pending_attachment(
        &storage,
        session.id,
        big_relative,
        Some("bbccddeeff00112233445566778899aa"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 2);
    assert_eq!(stat.skipped, 2);
    assert_eq!(stat.done, 0);
    assert_eq!(llm.vision_call_count(), 1, "仅探测，无内容调用");

    for message_id in [missing, big] {
        let rows = load_attachments(&storage, message_id).await;
        assert_eq!(rows[0].status, AttachmentStatus::Skipped);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 不可定位引用（http 链接）：标 skipped，不读取文件。
#[tokio::test]
async fn understand_skips_untrusted_reference() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("不会调用"));
    let (engine, storage, dir) =
        vision_engine("vision-untrusted", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_pending_attachment(
        &storage,
        session.id,
        "https://example.com/a.jpg",
        Some("aabbccddeeff00112233445566778899"),
    )
    .await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.skipped, 1);
    assert_eq!(llm.vision_call_count(), 1, "仅探测，无内容调用");

    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Skipped);

    let _ = std::fs::remove_dir_all(&dir);
}

/// md5 为空（NULL）附件：单行完成路径（`mark_attachment_done`）回填描述。
#[tokio::test]
async fn understand_without_md5_marks_single_row_done() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("无指纹图片描述"));
    let (engine, storage, dir) =
        vision_engine("vision-no-md5", Arc::clone(&llm), vision_on_config()).await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    write_image(&dir, "resources/images/a.jpg", b"bytes");
    let message_id =
        seed_pending_attachment(&storage, session.id, "resources/images/a.jpg", None).await;

    let stat = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("理解任务应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.done, 1);
    assert_eq!(stat.reused, 0);

    let rows = load_attachments(&storage, message_id).await;
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(rows[0].description.as_deref(), Some("无指纹图片描述"));
    assert_eq!(rows[0].description_model.as_deref(), Some("lm_studio"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空输入与无待处理附件：零统计、零调用。
#[tokio::test]
async fn understand_empty_input_returns_zero_stat() {
    let llm = Arc::new(MockLlm::local().with_vision_reply("不会调用"));
    let (engine, storage, dir) =
        vision_engine("vision-empty", Arc::clone(&llm), vision_on_config()).await;

    // 空会话列表
    let stat = understand_attachments(&engine, &[], &dir)
        .await
        .expect("空输入应成功");
    assert_eq!(stat, VisionRunStat::default());

    // 会话无 pending 附件
    let session = storage.create_session(None).await.expect("创建会话应成功");
    let stat2 = understand_attachments(&engine, &[session.id], &dir)
        .await
        .expect("无待处理附件应成功");
    assert_eq!(stat2, VisionRunStat::default());
    assert_eq!(llm.vision_call_count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}
