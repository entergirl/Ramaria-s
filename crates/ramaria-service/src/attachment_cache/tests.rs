//! crates/ramaria-service/src/attachment_cache/tests.rs - Ramaria 附件图片缓存单元测试
//!
//! 设计特点:
//! - 覆盖文件名生成（md5 大小写归一 / 缺失与空白回退）、候选扩展名查找与写入回读
//! - 覆盖清理口径（不存在目录 / 多文件统计 / 子目录不递归）与不可写目录降级
//! - 全部数据为合成样例；临时目录用后即删

use super::*;
use crate::test_support::temp_dir;

/// PNG 魔数样例字节。
fn png_bytes() -> Vec<u8> {
    vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x01]
}

/// 文件名字干：md5 大小写归一到小写；缺失 / 空串 / 纯空白回退 id 命名。
#[test]
fn cache_stem_normalizes_and_falls_back() {
    assert_eq!(
        cache_stem(1, Some("AABBCCDDEEFF00112233445566778899")),
        "aabbccddeeff00112233445566778899"
    );
    assert_eq!(cache_stem(7, None), "id-7");
    assert_eq!(cache_stem(7, Some("")), "id-7");
    assert_eq!(cache_stem(7, Some("   ")), "id-7");
}

/// 五种候选扩展名依次查找：命中任意一种即可定位。
#[test]
fn find_cached_probes_all_extensions() {
    let dir = temp_dir("cache-ext");
    let md5 = "aabbccddeeff00112233445566778899";
    for ext in EXTENSIONS {
        let path = dir.join(format!("{md5}.{ext}"));
        std::fs::write(&path, b"x").expect("写入候选文件应成功");
        assert_eq!(
            find_cached(&dir, 1, Some(md5)),
            Some(path.clone()),
            "扩展名 {ext} 应命中"
        );
        std::fs::remove_file(&path).expect("清理候选文件应成功");
    }
    assert_eq!(
        find_cached(&dir, 1, Some(md5)),
        None,
        "无候选文件时返回 None"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// store 按魔数写扩展名并可回读：PNG → `.png`；未知格式回退 `.jpg`；md5 缺失用 id 命名。
#[test]
fn store_writes_by_magic_and_reads_back() {
    let dir = temp_dir("cache-store");
    let md5 = "d41d8cd98f00b204e9800998ecf8427e";
    let png = png_bytes();
    let stored = store(&dir, 3, Some(md5), &png).expect("缓存写入应成功");
    assert!(stored.ends_with(format!("{md5}.png")));
    assert_eq!(std::fs::read(&stored).expect("读回应成功"), png);
    assert_eq!(find_cached(&dir, 3, Some(md5)), Some(stored));

    // 未知格式回退 .jpg；md5 缺失用 id- 命名
    let unknown = store(&dir, 9, None, b"not-an-image").expect("未知格式缓存写入应成功");
    assert!(unknown.ends_with("id-9.jpg"));
    assert_eq!(find_cached(&dir, 9, None), Some(unknown));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 清理：不存在目录返回全 0；多文件按成功删除统计；子目录不递归。
#[test]
fn clear_statistics_and_non_recursive() {
    let dir = temp_dir("cache-clear");

    // 不存在目录 → 全 0
    let missing = dir.join("missing");
    let stat = clear(&missing).expect("不存在目录清理应成功");
    assert_eq!(stat, AttachmentCacheStat::default());

    // 多文件：统计文件数与字节数
    std::fs::create_dir_all(&dir).expect("创建缓存目录应成功");
    std::fs::write(dir.join("a.png"), vec![0u8; 10]).expect("写入应成功");
    std::fs::write(dir.join("b.jpg"), vec![0u8; 32]).expect("写入应成功");
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).expect("创建子目录应成功");
    std::fs::write(sub.join("inner.png"), vec![0u8; 64]).expect("写入应成功");

    let stat = clear(&dir).expect("清理应成功");
    assert_eq!(stat.files, 2);
    assert_eq!(stat.bytes, 42);
    assert!(sub.join("inner.png").is_file(), "子目录内容不被递归删除");
    assert!(!dir.join("a.png").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 缓存目录不可写（路径上是文件）：store 返回 None 且不 panic。
#[test]
fn store_on_unwritable_dir_returns_none() {
    let dir = temp_dir("cache-unwritable");
    let occupied = dir.join("occupied");
    std::fs::write(&occupied, b"x").expect("占位文件写入应成功");
    assert_eq!(
        store(
            &occupied,
            1,
            Some("aabbccddeeff00112233445566778899"),
            b"data"
        ),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}
