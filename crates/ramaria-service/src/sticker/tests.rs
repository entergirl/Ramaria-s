//! crates/ramaria-service/src/sticker/tests.rs - Ramaria 表情资产用例测试
//!
//! 设计特点:
//! - 真实 SQLite 临时库 + 临时资产目录，断言文件落盘与行记录两侧
//! - 覆盖入库校验（空 / 非图片）、更新三态（保持 / 覆盖 / 清空）、删除文件跟随
//! - 主题匹配为纯函数断言（评分优先级与并列取小 id）

use super::*;
use crate::test_support::engine_with_db;

/// 最小合法 PNG 头字节（魔数识别口径）。
const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x01, 0x02];

/// 构造资产行（匹配纯函数用）。
fn asset(id: i64, tags: &[&str], description: Option<&str>) -> StickerAsset {
    StickerAsset {
        id,
        file_name: format!("{id}.png"),
        file_path: format!("{id}.png"),
        md5: None,
        size: None,
        description: description.map(str::to_string),
        tags: tags.iter().map(|tag| tag.to_string()).collect(),
        created_at: 0,
        updated_at: 0,
    }
}

/// 新增 → 列表 → 更新三态 → 删除（含文件跟随）全链路。
#[tokio::test]
async fn add_list_update_remove_roundtrip() {
    let (engine, _storage, dir) = engine_with_db("sticker-roundtrip").await;

    let tags = vec!["开心".to_string()];
    let added = add(&engine, "开心.png", PNG_BYTES, Some("开心的表情"), &tags)
        .await
        .expect("新增应成功");
    assert_eq!(added.file_name, "开心.png");
    assert_eq!(added.tags, tags);
    assert!(added.md5.is_some(), "md5 应已计算");

    // 资产文件按 md5 命名落盘（stickers/ 目录）
    let file_path = dir.join("stickers").join(&added.file_path);
    assert!(file_path.is_file(), "资产文件应落盘: {file_path:?}");
    assert_eq!(
        std::fs::read(&file_path).expect("读取资产文件应成功"),
        PNG_BYTES
    );

    let assets = list(&engine).await.expect("列表应成功");
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].id, added.id);

    // 更新：None 保持现值
    assert!(
        update(&engine, added.id, None, None)
            .await
            .expect("更新应成功")
    );
    let after = list(&engine).await.expect("列表应成功");
    assert_eq!(after[0].description.as_deref(), Some("开心的表情"));

    // 更新：描述与标签覆盖（描述 trim）
    let new_tags = vec!["高兴".to_string()];
    assert!(
        update(&engine, added.id, Some(" 开心的脸 "), Some(&new_tags))
            .await
            .expect("更新应成功")
    );
    let after = list(&engine).await.expect("列表应成功");
    assert_eq!(after[0].description.as_deref(), Some("开心的脸"));
    assert_eq!(after[0].tags, new_tags);

    // 更新：描述空串清空（存 NULL）
    assert!(
        update(&engine, added.id, Some("  "), None)
            .await
            .expect("更新应成功")
    );
    let after = list(&engine).await.expect("列表应成功");
    assert_eq!(after[0].description, None, "空描述应清空为 NULL");

    // 删除：行与文件一并删除；重复删除返回 false
    assert!(remove(&engine, added.id).await.expect("删除应成功"));
    assert!(list(&engine).await.expect("列表应成功").is_empty());
    assert!(!file_path.is_file(), "资产文件应随行删除");
    assert!(
        !remove(&engine, added.id).await.expect("重复删除应成功"),
        "不存在返回 false"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 入库校验：空字节 / 非图片字节拒绝。
#[tokio::test]
async fn add_rejects_invalid_bytes() {
    let (engine, _storage, dir) = engine_with_db("sticker-reject").await;

    let err = add(&engine, "x.png", &[], None, &[])
        .await
        .expect_err("空字节应拒绝");
    assert!(err.to_string().contains("图片字节不能为空"), "{err}");

    let err = add(&engine, "x.bin", b"not an image", None, &[])
        .await
        .expect_err("非图片应拒绝");
    assert!(err.to_string().contains("不是可识别的图片格式"), "{err}");
    assert!(!dir.join("stickers").exists(), "拒绝路径不应产生资产目录");

    let _ = std::fs::remove_dir_all(dir);
}

/// 主题解析：命中返回字节；关键词空 / 无匹配 / 文件缺失降级 None。
#[tokio::test]
async fn resolve_returns_bytes_and_degrades_without_file() {
    let (engine, _storage, dir) = engine_with_db("sticker-resolve").await;

    let tags = vec!["开心".to_string()];
    let added = add(&engine, "开心.png", PNG_BYTES, Some("笑"), &tags)
        .await
        .expect("新增应成功");

    let hit = resolve(&engine, "开心")
        .await
        .expect("解析应成功")
        .expect("应命中");
    assert_eq!(hit.id, added.id);
    assert_eq!(hit.bytes, PNG_BYTES);

    assert!(
        resolve(&engine, "  ").await.expect("解析应成功").is_none(),
        "空白关键词应未命中"
    );
    assert!(
        resolve(&engine, "不存在")
            .await
            .expect("解析应成功")
            .is_none(),
        "无匹配应未命中"
    );

    // 文件缺失：降级 None（不报错）
    let file_path = dir.join("stickers").join(&added.file_path);
    std::fs::remove_file(&file_path).expect("删除资产文件应成功");
    assert!(
        resolve(&engine, "开心")
            .await
            .expect("解析应成功")
            .is_none(),
        "文件缺失应降级未命中"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 匹配评分优先级：标签全等 > 标签包含 > 描述包含；并列取 id 最小。
#[test]
fn match_asset_scores_and_breaks_ties() {
    let assets = vec![
        asset(1, &["开心的猫"], None),
        asset(2, &["开心"], None),
        asset(3, &["汪"], Some("有点开心")),
        asset(5, &["开心"], None),
    ];

    // 全等优先于包含（id 更大仍胜出）
    let hit = match_asset(&assets, "开心").expect("应命中");
    assert_eq!(hit.id, 2, "标签全等应优先于包含");

    // 无全等时：包含优先于描述包含
    let assets = vec![
        asset(1, &["开心的猫"], None),
        asset(3, &["汪"], Some("有点开心")),
    ];
    let hit = match_asset(&assets, "开心").expect("应命中");
    assert_eq!(hit.id, 1, "标签包含应优先于描述包含");

    // 仅描述命中
    let assets = vec![asset(3, &["汪"], Some("有点开心"))];
    let hit = match_asset(&assets, "开心").expect("应命中");
    assert_eq!(hit.id, 3);

    // 并列取 id 最小
    let assets = vec![asset(5, &["开心"], None), asset(2, &["开心"], None)];
    let hit = match_asset(&assets, "开心").expect("应命中");
    assert_eq!(hit.id, 2);

    assert!(match_asset(&[], "开心").is_none());
    assert!(match_asset(&assets, "  ").is_none());
    assert!(match_asset(&assets, "不存在").is_none());
}
