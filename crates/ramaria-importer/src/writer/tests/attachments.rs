//! crates/ramaria-importer/src/writer/tests/attachments.rs - 导入附件采集单元测试
//!
//! 设计特点:
//! - 覆盖附件行随消息落库：字段完整与 md5 大写输入小写存储
//! - 覆盖定位口径：本地文件命中 pending；缺失 / 服务器链接 / http 链接 / 无导出目录 skipped
//! - 覆盖失败补偿路径无附件残留（附件写入在消息落库之后）

use super::*;

/// 图片附件引用样例（source_ref 直传；md5 大写输入用于断言小写存储）。
fn image_ref(source_ref: &str) -> InboundAttachmentRef {
    InboundAttachmentRef {
        kind: InboundAttachmentKind::Image,
        source_ref: source_ref.to_string(),
        md5: Some("AABBCCDDEEFF00112233445566778899".to_string()),
        size: Some(2048),
        width: Some(640),
        height: Some(480),
        sub_type: Some("photo".to_string()),
    }
}

/// 附件取样文件名（小写 md5 + 下划线 + 大写 md5，与导出落盘命名一致）。
const SAMPLE_FILE_NAME: &str =
    "aabbccddeeff00112233445566778899_AABBCCDDEEFF00112233445566778899.jpg";

/// 构造带一条图片附件的 session（单条 self 消息，指纹固定）。
fn make_attachment_session(fingerprint: &str) -> crate::traits::ImportedSession {
    let mut session = make_dedup_session("看这张图 [图片#aabbccdd]", fingerprint);
    session.messages[0].attachments =
        vec![image_ref(&format!("resources/images/{SAMPLE_FILE_NAME}"))];
    session
}

/// 创建附件测试目录（含 resources/images 层级，按标签区分并行用例）。
fn attachment_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ramaria_writer_att_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("resources").join("images")).expect("创建测试目录应成功");
    dir
}

/// 附件行随消息落库：本地文件命中 → pending，字段完整（md5 小写存储、归属消息正确）。
#[tokio::test]
async fn write_l0_persists_pending_attachment_with_fields() {
    let pool = test_pool().await;
    let dir = attachment_dir("pending");
    std::fs::write(
        dir.join("resources").join("images").join(SAMPLE_FILE_NAME),
        b"jpeg-bytes",
    )
    .expect("写入图片文件应成功");

    let sessions = vec![make_attachment_session("fp-att-pending")];
    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        PersonaDispatch::Dual {
            self_uid: "SELF_UID",
            self_persona_uid: Some("user-0001"),
            other_persona_uid: None,
        },
        ImportSide::Me,
        Some(dir.as_path()),
    )
    .await
    .unwrap();
    assert_eq!(outcome.messages_written, 1);

    type AttachmentRow = (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<String>,
        String,
        i64,
        i64,
    );
    let rows: Vec<AttachmentRow> = sqlx::query_as(
        "SELECT source_ref, kind, md5, size, width, height, sub_type, status, created_at, updated_at \
         FROM message_attachments",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "每条附件引用应落一行");
    let (source_ref, kind, md5, size, width, height, sub_type, status, created_at, updated_at) =
        &rows[0];
    assert_eq!(
        source_ref,
        &format!("resources/images/{SAMPLE_FILE_NAME}"),
        "source_ref 应原样落库"
    );
    assert_eq!(kind, "image");
    assert_eq!(
        md5.as_deref(),
        Some("aabbccddeeff00112233445566778899"),
        "md5 应小写存储"
    );
    assert_eq!(*size, Some(2048));
    assert_eq!(*width, Some(640));
    assert_eq!(*height, Some(480));
    assert_eq!(sub_type.as_deref(), Some("photo"));
    assert_eq!(status, "pending", "本地文件命中应置 pending");
    assert!(*created_at > 0 && *updated_at > 0, "时间戳应取导入时刻");

    // 附件行归属实际写入的消息
    let bound: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM message_attachments a JOIN messages m ON m.id = a.message_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bound, 1, "附件行应关联到已写入的消息");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 不可定位的附件（文件缺失 / 服务器链接 / http 链接）→ skipped，不影响消息写入。
#[tokio::test]
async fn write_l0_marks_unresolvable_attachments_skipped() {
    let pool = test_pool().await;
    let dir = attachment_dir("skipped");
    let mut session = make_dedup_session("三张图", "fp-att-skipped");
    session.messages[0].attachments = vec![
        // 相对路径但文件缺失
        image_ref("resources/images/missing.jpg"),
        // 旧导出服务器链接（"/" 开头）
        image_ref("/download?appid=1406&fileid=EXAMPLE"),
        // http 链接
        image_ref("https://example.com/a.jpg"),
    ];

    let outcome = ImportWriter::write_l0(
        &pool,
        &[session],
        PersonaDispatch::Dual {
            self_uid: "SELF_UID",
            self_persona_uid: Some("user-0001"),
            other_persona_uid: None,
        },
        ImportSide::Me,
        Some(dir.as_path()),
    )
    .await
    .unwrap();
    assert_eq!(outcome.messages_written, 1, "附件不可定位不阻塞消息写入");

    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM message_attachments ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec!["skipped", "skipped", "skipped"],
        "缺失 / 服务器链接 / http 链接应一律 skipped"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// export_dir 为 None（不定位）→ 全部 skipped；引用与指纹仍完整落库。
#[tokio::test]
async fn write_l0_none_export_dir_keeps_attachments_skipped() {
    let pool = test_pool().await;
    let sessions = vec![make_attachment_session("fp-att-none")];

    ImportWriter::write_l0(
        &pool,
        &sessions,
        PersonaDispatch::Dual {
            self_uid: "SELF_UID",
            self_persona_uid: Some("user-0001"),
            other_persona_uid: None,
        },
        ImportSide::Me,
        None,
    )
    .await
    .unwrap();

    let (source_ref, md5, status): (String, Option<String>, String) =
        sqlx::query_as("SELECT source_ref, md5, status FROM message_attachments")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        source_ref.starts_with("resources/images/"),
        "无导出目录时 source_ref 仍原样落库"
    );
    assert_eq!(md5.as_deref(), Some("aabbccddeeff00112233445566778899"));
    assert_eq!(status, "skipped", "无导出目录应全部 skipped");
}

/// 批量写入失败补偿：失败批次不留附件行（附件写入在消息落库之后）。
#[tokio::test]
async fn write_l0_batch_failure_leaves_no_attachment_rows() {
    let pool = test_pool().await;
    // 空指纹绕过批内去重与跨文件查重，两条消息落入同一 batch 触发 UNIQUE 冲突
    let mut session = make_dedup_session("第一条", "");
    session.messages[0].attachments = vec![image_ref("resources/images/missing.jpg")];
    session.messages.push(crate::traits::ParsedMessage {
        role: "user".to_string(),
        content: "第二条".to_string(),
        created_at: 1200,
        fingerprint: String::new(),
        sender_uid: "SELF_UID".to_string(),
        sender_uin: Some("10001".to_string()),
        sender_name: "我".to_string(),
        group_nickname: None,
        member_role: None,
        attachments: vec![image_ref("resources/images/missing.jpg")],
    });

    let result = ImportWriter::write_l0(
        &pool,
        &[session],
        PersonaDispatch::Dual {
            self_uid: "SELF_UID",
            self_persona_uid: Some("user-0001"),
            other_persona_uid: None,
        },
        ImportSide::Me,
        None,
    )
    .await;

    assert!(result.is_err(), "批量写入失败应中止本批导入");
    let attachment_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message_attachments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attachment_count, 0, "失败批次不应残留附件行");
}
