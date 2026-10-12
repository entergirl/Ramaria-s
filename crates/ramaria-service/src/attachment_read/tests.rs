//! crates/ramaria-service/src/attachment_read/tests.rs - Ramaria 附件图片读取单元测试
//!
//! 设计特点:
//! - 纯函数：槽位扫描三形态与失配口径、附件按序对齐（hash 前缀 / 描述全等 / 无文件跳过）
//! - 端到端：真实临时库 + 临时导出目录与文件，覆盖 源命中 → 缓存命中 → 缓存兜底 链路
//! - 导出根登记（幂等 / 保序 / 容错 / 导入集成）按 `importer` feature 门控
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网

use super::*;
use crate::test_support::{engine_with_db, temp_dir};
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AttachmentStatus, Message, MessageRole, MessageSource, now_ms};
use ramaria_storage::SqliteStorage;

// ---- 测试脚手架 ----

/// 样例图片 md5（小写，32 位）。
const IMAGE_MD5: &str = "fccb86f2a1b695df3c37a1042a967a44";

/// JPEG 魔数样例字节。
fn jpeg_bytes() -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46]
}

/// 构造一个图片附件行（描述给出时按已完成处置）。
fn attachment(id: i64, md5: Option<&str>, description: Option<&str>) -> MessageAttachment {
    MessageAttachment {
        id,
        message_id: Uuid::new_v4(),
        kind: InboundAttachmentKind::Image,
        source_ref: format!("resources/images/{id}.jpg"),
        md5: md5.map(str::to_string),
        size: None,
        width: None,
        height: None,
        sub_type: Some("photo".to_string()),
        status: if description.is_some() {
            AttachmentStatus::Done
        } else {
            AttachmentStatus::Pending
        },
        description: description.map(str::to_string),
        description_model: description.map(|_| "mock-vision".to_string()),
        created_at: 100,
        updated_at: 100,
    }
}

/// 造一条含图片附件的消息（返回消息 id）。
async fn seed_image_message(
    storage: &SqliteStorage,
    session_id: Uuid,
    source_ref: &str,
    md5: Option<&str>,
    content: &str,
    description: Option<&str>,
) -> Uuid {
    let message = Message::new(
        session_id,
        MessageRole::User,
        content.to_string(),
        MessageSource::Local,
    );
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
    let now = now_ms();
    let row = MessageAttachment {
        id: 0,
        message_id: message.id,
        kind: InboundAttachmentKind::Image,
        source_ref: source_ref.to_string(),
        md5: md5.map(str::to_string),
        size: None,
        width: None,
        height: None,
        sub_type: Some("photo".to_string()),
        status: if description.is_some() {
            AttachmentStatus::Done
        } else {
            AttachmentStatus::Pending
        },
        description: description.map(str::to_string),
        description_model: description.map(|_| "mock-vision".to_string()),
        created_at: now,
        updated_at: now,
    };
    storage
        .insert_message_attachments(&[row])
        .await
        .expect("写入附件应成功");
    message.id
}

/// 把一个目录登记为导出根（直接写入设置键，与导入侧登记口径一致）。
async fn register_root(storage: &SqliteStorage, root: &Path) {
    let value =
        serde_json::to_string(&vec![root.to_string_lossy().to_string()]).expect("序列化应成功");
    storage
        .set_setting("import_export_roots", &value)
        .await
        .expect("写入导出根设置应成功");
}

// ---- 槽位扫描（纯函数） ----

/// 三种形态按出现顺序识别；混排其余文本不干扰。
#[test]
fn scan_recognizes_three_forms_in_order() {
    let text = "开头 [图片#AABBCCDD] 中间 [图片: 一只橘猫] 末尾 [图片] 结束";
    assert_eq!(
        scan_image_slots(text),
        vec![
            ImageSlot::Hash("AABBCCDD".to_string()),
            ImageSlot::Desc("一只橘猫".to_string()),
            ImageSlot::NoFile,
        ]
    );
}

/// 失配形态不识别、不占槽：位数不符 / 含非 ASCII / 缺闭合 / 描述含方括号 / 描述为空。
#[test]
fn scan_rejects_malformed_forms() {
    assert!(scan_image_slots("[图片#aabbccd]").is_empty(), "7 位不识别");
    assert!(
        scan_image_slots("[图片#aabbccdd1]").is_empty(),
        "9 位不识别"
    );
    assert!(
        scan_image_slots("[图片#aabbcc中]").is_empty(),
        "8 字符内含非 ASCII 不识别"
    );
    assert!(
        scan_image_slots("[图片#aabbccdd").is_empty(),
        "缺闭合括号不识别"
    );
    assert!(
        scan_image_slots("[图片: d含[方括号]").is_empty(),
        "描述含左方括号不识别"
    );
    assert!(scan_image_slots("[图片: ]").is_empty(), "空描述不识别");
    assert!(
        scan_image_slots("[图片: 描述").is_empty(),
        "描述未闭合不识别"
    );
}

/// UTF-8 混排（中文 / emoji）不切字符；描述可含换行；相邻槽位无分隔也逐段识别。
#[test]
fn scan_handles_utf8_and_multiline_desc() {
    assert_eq!(
        scan_image_slots("🖼️ [图片: 一只🐱\n第二行] 尾巴"),
        vec![ImageSlot::Desc("一只🐱\n第二行".to_string())]
    );
    assert_eq!(
        scan_image_slots("[图片][图片#aabbccdd][图片: 描述]"),
        vec![
            ImageSlot::NoFile,
            ImageSlot::Hash("aabbccdd".to_string()),
            ImageSlot::Desc("描述".to_string()),
        ]
    );
    assert!(scan_image_slots("").is_empty());
}

// ---- 槽位对齐（纯函数） ----

/// hash 前缀匹配（大小写不敏感）与描述全等匹配按序消费；已消费项不再命中。
#[test]
fn align_matches_hash_and_desc_in_order() {
    let rows = vec![
        attachment(1, Some("aabbccddeeff00112233445566778899"), Some("猫")),
        attachment(2, Some("11223344aabbccdd0011223344556677"), Some("狗")),
    ];
    let slots = vec![
        ImageSlot::Hash("11223344".to_string()),
        ImageSlot::Desc("猫".to_string()),
        ImageSlot::Hash("AABBCCDD".to_string()),
    ];
    assert_eq!(align_slot(&rows, &slots, 0), Some(1), "槽 0 前缀命中 2 号");
    assert_eq!(align_slot(&rows, &slots, 1), Some(0), "槽 1 描述命中 1 号");
    assert_eq!(align_slot(&rows, &slots, 2), None, "两项均已消费，无命中");
}

/// 重复描述按序消费：同描述的两个槽位分别命中前一后一。
#[test]
fn align_duplicate_desc_consumes_in_order() {
    let rows = vec![
        attachment(
            1,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Some("一样的图"),
        ),
        attachment(
            2,
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            Some("一样的图"),
        ),
    ];
    let slots = vec![
        ImageSlot::Desc("一样的图".to_string()),
        ImageSlot::Desc("一样的图".to_string()),
    ];
    assert_eq!(align_slot(&rows, &slots, 0), Some(0));
    assert_eq!(align_slot(&rows, &slots, 1), Some(1));
}

/// 无文件槽不消费附件，后续槽位继续按序对齐；目标槽无命中 / 越界返回 None。
#[test]
fn align_skips_no_file_and_returns_none_on_miss() {
    let rows = vec![attachment(
        1,
        Some("aabbccddeeff00112233445566778899"),
        None,
    )];
    let slots = vec![ImageSlot::NoFile, ImageSlot::Hash("aabbccdd".to_string())];
    assert_eq!(align_slot(&rows, &slots, 0), None, "无文件槽无附件");
    assert_eq!(align_slot(&rows, &slots, 1), Some(0), "后续槽位正常对齐");

    let miss = vec![ImageSlot::Hash("ffffffff".to_string())];
    assert_eq!(align_slot(&rows, &miss, 0), None, "无命中返回 None");
    assert_eq!(align_slot(&rows, &miss, 1), None, "槽位越界返回 None");
}

// ---- 端到端（真实临时库 + 临时导出目录） ----

/// 源文件命中 → 二读缓存命中 → 删源后三读仍成功（缓存兜底）。
#[tokio::test]
async fn read_source_then_cache_then_cache_fallback() {
    let (engine, storage, dir) = engine_with_db("attach-read-cache").await;
    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_image_message(
        &storage,
        session.id,
        "resources/images/pic.jpg",
        Some(IMAGE_MD5),
        "[图片#fccb86f2]",
        None,
    )
    .await;

    // 导出目录与源文件（JPEG 魔数）
    let source = dir.join("resources").join("images").join("pic.jpg");
    std::fs::create_dir_all(source.parent().expect("源文件应有父目录")).expect("创建源目录应成功");
    let jpeg = jpeg_bytes();
    std::fs::write(&source, &jpeg).expect("写入源文件应成功");
    register_root(&storage, &dir).await;

    let cache_dir = dir.join("image_cache");

    // 一读：源文件命中
    let first = read_message_image(&engine, message_id, 0, &cache_dir)
        .await
        .expect("首次读取应成功");
    assert!(!first.from_cache, "首读应来自源文件");
    assert_eq!(first.bytes, jpeg);
    assert_eq!(first.mime, "image/jpeg");

    // 二读：缓存命中
    let second = read_message_image(&engine, message_id, 0, &cache_dir)
        .await
        .expect("二次读取应成功");
    assert!(second.from_cache, "二读应命中缓存");
    assert_eq!(second.bytes, jpeg);

    // 三读：删除源文件后仍成功（缓存兜底）
    std::fs::remove_file(&source).expect("删除源文件应成功");
    let third = read_message_image(&engine, message_id, 0, &cache_dir)
        .await
        .expect("缓存兜底读取应成功");
    assert!(third.from_cache);
    assert_eq!(third.bytes, jpeg);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无缓存且不可定位（未登记根 / 源文件缺失）→ 校验错误。
#[tokio::test]
async fn read_without_cache_or_source_fails() {
    let (engine, storage, dir) = engine_with_db("attach-read-miss").await;
    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message_id = seed_image_message(
        &storage,
        session.id,
        "resources/images/gone.jpg",
        Some("0000000000000000000000000000beef"),
        "[图片#00000000]",
        None,
    )
    .await;
    let cache_dir = dir.join("image_cache");

    // 未登记导出根
    let err = read_message_image(&engine, message_id, 0, &cache_dir)
        .await
        .expect_err("未登记导出根应报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("未登记导出目录或图片已不可用"),
        "{err}"
    );

    // 已登记根但源文件缺失 → 同样不可用
    register_root(&storage, &dir).await;
    let err = read_message_image(&engine, message_id, 0, &cache_dir)
        .await
        .expect_err("源文件缺失应报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("未登记导出目录或图片已不可用"),
        "{err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 消息不存在 / slot 越界 / 无文件槽 → 校验错误。
#[tokio::test]
async fn read_rejects_out_of_range_missing_message_and_no_file_slot() {
    let (engine, storage, dir) = engine_with_db("attach-read-reject").await;
    let cache_dir = dir.join("image_cache");
    let session = storage.create_session(None).await.expect("创建会话应成功");

    // 消息不存在
    let err = read_message_image(&engine, Uuid::new_v4(), 0, &cache_dir)
        .await
        .expect_err("消息不存在应报错");
    assert_eq!(err.category(), "validation");
    assert!(err.to_string().contains("消息不存在"), "{err}");

    // 越界：正文一个槽位，请求第 2 个
    let message_id = seed_image_message(
        &storage,
        session.id,
        "resources/images/pic.jpg",
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "[图片#aaaaaaaa]",
        None,
    )
    .await;
    let err = read_message_image(&engine, message_id, 1, &cache_dir)
        .await
        .expect_err("槽位越界应报错");
    assert!(err.to_string().contains("图片序号越界"), "{err}");

    // 无文件槽：`[图片]` 不消费附件
    let nofile = Message::new(
        session.id,
        MessageRole::User,
        "看图 [图片]".to_string(),
        MessageSource::Local,
    );
    storage.save_message(&nofile).await.expect("写入消息应成功");
    let err = read_message_image(&engine, nofile.id, 0, &cache_dir)
        .await
        .expect_err("无文件槽应报错");
    assert!(err.to_string().contains("该图片无本地文件"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 描述槽对齐（渲染后形态）与 md5 反查兜底：服务器链接 + 文件名前缀命中 → 可读。
#[tokio::test]
async fn read_aligns_desc_slot_and_reverse_lookup() {
    let (engine, storage, dir) = engine_with_db("attach-read-desc").await;
    let session = storage.create_session(None).await.expect("创建会话应成功");

    // 附件已完成且有描述 → 正文占位符渲染为描述形态（非直连的服务器链接）
    let message_id = seed_image_message(
        &storage,
        session.id,
        "/download?fileid=1",
        Some(IMAGE_MD5),
        "[图片#fccb86f2]",
        Some("一只橘猫"),
    )
    .await;

    // 反查素材：resources/images/{md5}_大写.jpg
    let file_name = "fccb86f2a1b695df3c37a1042a967a44_FCCB86F2A1B695DF3C37A1042A967A44.jpg";
    let source = dir.join("resources").join("images").join(file_name);
    std::fs::create_dir_all(source.parent().expect("源文件应有父目录")).expect("创建源目录应成功");
    let jpeg = jpeg_bytes();
    std::fs::write(&source, &jpeg).expect("写入源文件应成功");
    register_root(&storage, &dir).await;

    let cache_dir = dir.join("image_cache");
    let read = read_message_image(&engine, message_id, 0, &cache_dir)
        .await
        .expect("反查兜底读取应成功");
    assert_eq!(read.bytes, jpeg);
    assert!(!read.from_cache, "首读应来自源文件");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 导出根登记（importer feature） ----

/// 登记口径：空串不写；同根幂等；多根保序；非法值 / 空值按空处理。
#[cfg(feature = "importer")]
#[tokio::test]
async fn export_root_register_idempotent_and_ordered() {
    let (_engine, storage, dir) = engine_with_db("attach-root-register").await;
    let key = "import_export_roots";

    // 空串：直接成功且不写
    register_export_root(storage.as_ref(), "")
        .await
        .expect("空串登记应成功");
    assert!(
        storage
            .get_setting(key)
            .await
            .expect("读取应成功")
            .is_none(),
        "空串不应写入设置"
    );

    // 同根两次：仅一条
    register_export_root(storage.as_ref(), "C:/exports/a")
        .await
        .expect("首次登记应成功");
    register_export_root(storage.as_ref(), "C:/exports/a")
        .await
        .expect("重复登记应成功");
    assert_eq!(
        list_export_root_paths(storage.as_ref()).await,
        vec![PathBuf::from("C:/exports/a")]
    );

    // 多根保序：按首次登记顺序
    register_export_root(storage.as_ref(), "C:/exports/b")
        .await
        .expect("二次登记应成功");
    assert_eq!(
        list_export_root_paths(storage.as_ref()).await,
        vec![PathBuf::from("C:/exports/a"), PathBuf::from("C:/exports/b")]
    );

    // 非法 JSON / 空值：按空列表处理；再次登记按空重建
    storage
        .set_setting(key, "not-json")
        .await
        .expect("写入应成功");
    assert!(list_export_root_paths(storage.as_ref()).await.is_empty());
    storage.set_setting(key, "").await.expect("写入应成功");
    assert!(list_export_root_paths(storage.as_ref()).await.is_empty());
    register_export_root(storage.as_ref(), "C:/exports/c")
        .await
        .expect("容错后登记应成功");
    assert_eq!(
        list_export_root_paths(storage.as_ref()).await,
        vec![PathBuf::from("C:/exports/c")]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// L0 导入成功后自动登记导出目录。
#[cfg(feature = "importer")]
#[tokio::test]
async fn write_l0_registers_export_root() {
    use crate::import::{ImportMode, ImportRequest};

    let (engine, storage, dir) = engine_with_db("attach-root-write-l0").await;
    let export_path = dir.join("export.json");
    std::fs::write(&export_path, private_export_json()).expect("写入导出文件应成功");

    engine
        .import_qq_l0(ImportRequest {
            file_path: export_path,
            mode: ImportMode::Fast,
            gap_minutes: 10,
            side: ramaria_importer::qq::ImportSide::Both,
            persona_name: None,
            self_persona_uid: None,
            other_persona_name: None,
            other_persona_uid: None,
        })
        .await
        .expect("L0 导入应成功");

    assert_eq!(
        list_export_root_paths(storage.as_ref()).await,
        vec![dir.clone()],
        "导入成功后应登记导出目录"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 最小私聊导出 JSON（单会话双方各一条；切割间隔内）。
#[cfg(feature = "importer")]
fn private_export_json() -> String {
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
