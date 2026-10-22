//! crates/ramaria-service/src/image_source/tests.rs - 发图目录用例测试
//!
//! 覆盖: 默认目录解析与创建 / 自定义目录往返与恢复默认 / 非法目录拒绝 /
//! 文件目录归属校验 / 自定义目录失效回退。

use super::dir as resolve_dir;
use super::*;
use crate::test_support::engine_with_db;
use ramaria_core::traits::StoreInfrastructure;

/// 创建测试用目录并返回真实路径。
fn make_dir(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).expect("创建测试目录应成功");
    path.canonicalize().expect("测试目录应可解析")
}

/// 创建测试文件并返回真实路径。
fn make_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").expect("写入测试文件应成功");
    path
}

/// 默认目录：位于数据目录下的 `outgoing/`，首次读取即创建，且不写设置键。
#[tokio::test]
async fn default_dir_is_created_without_setting_row() {
    let (engine, storage, dir) = engine_with_db("image-source-default").await;

    let resolved = resolve_dir(&engine).await.expect("默认目录应可解析");
    assert_eq!(
        resolved,
        dir.join(DEFAULT_DIR_NAME)
            .canonicalize()
            .expect("默认目录应已创建"),
        "默认目录应为数据目录下的 outgoing"
    );
    assert!(resolved.is_dir(), "默认目录应已创建");
    assert!(
        storage
            .get_setting(SETTING_KEY)
            .await
            .expect("读取设置应成功")
            .is_none(),
        "使用默认目录时不应写设置键"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 自定义目录：写入设置键、读回一致；恢复默认删除键并返回默认目录。
#[tokio::test]
async fn custom_dir_roundtrip_and_restore_default() {
    let (engine, storage, dir) = engine_with_db("image-source-custom").await;
    let custom = make_dir(&dir, "pics");

    let applied = set(&engine, Some(custom.to_str().expect("路径应为 UTF-8")))
        .await
        .expect("设置自定义目录应成功");
    assert_eq!(applied, custom, "生效目录应为规范化后的自定义目录");
    assert_eq!(
        storage
            .get_setting(SETTING_KEY)
            .await
            .expect("读取设置应成功")
            .as_deref(),
        Some(custom.to_string_lossy().as_ref()),
        "设置键应保存规范化路径"
    );
    assert_eq!(
        resolve_dir(&engine).await.expect("读取应成功"),
        custom,
        "读取应返回自定义目录"
    );

    let restored = set(&engine, None).await.expect("恢复默认应成功");
    assert_eq!(
        restored,
        dir.join(DEFAULT_DIR_NAME)
            .canonicalize()
            .expect("默认目录应已创建")
    );
    assert!(
        storage
            .get_setting(SETTING_KEY)
            .await
            .expect("读取设置应成功")
            .is_none(),
        "恢复默认应删除设置键"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 非法目录：不存在 / 非目录 / 盘符根与系统目录名一律拒绝（校验错误）。
#[tokio::test]
async fn invalid_dirs_are_rejected() {
    let (engine, _storage, dir) = engine_with_db("image-source-invalid").await;

    let missing = dir.join("not-there");
    let err = set(&engine, Some(missing.to_str().expect("路径应为 UTF-8")))
        .await
        .expect_err("不存在的目录应拒绝");
    assert_eq!(err.category(), "validation");

    let file = make_file(&dir, "plain.png");
    let err = set(&engine, Some(file.to_str().expect("路径应为 UTF-8")))
        .await
        .expect_err("文件路径应拒绝");
    assert_eq!(err.category(), "validation");

    // 系统目录名（组件命中即拒绝），不依赖真实系统路径
    let system_like = make_dir(&dir, "Windows");
    let err = set(&engine, Some(system_like.to_str().expect("路径应为 UTF-8")))
        .await
        .expect_err("系统目录名应拒绝");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("系统目录"),
        "文案应说明系统目录限制: {err}"
    );

    // 盘符根 / 文件系统根
    #[cfg(target_os = "windows")]
    let root = Path::new("C:\\");
    #[cfg(not(target_os = "windows"))]
    let root = Path::new("/");
    let err = validate_dir(root).expect_err("盘符根应拒绝");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 文件校验：目录内允许，目录外拒绝，目录缺失视为不可用。
#[tokio::test]
async fn file_validation_requires_file_inside_dir() {
    let (_engine, _storage, dir) = engine_with_db("image-source-file").await;
    let allowed_dir = make_dir(&dir, "outgoing");
    let other_dir = make_dir(&dir, "pics");

    let inside = make_file(&allowed_dir, "a.png");
    let nested = make_dir(&allowed_dir, "nested");
    let nested_file = make_file(&nested, "b.png");
    let outside = make_file(&other_dir, "c.png");

    assert_eq!(
        validate_file(&allowed_dir, &inside).expect("目录内文件应通过"),
        inside.canonicalize().expect("文件应可解析")
    );
    assert!(
        validate_file(&allowed_dir, &nested_file).is_ok(),
        "子目录文件应允许"
    );

    let err = validate_file(&allowed_dir, &outside).expect_err("目录外文件应拒绝");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("发图目录"),
        "文案应指向发图目录约束: {err}"
    );

    let err = validate_file(&dir.join("missing-dir"), &inside)
        .expect_err("目录不存在时应拒绝（无法解析目录）");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 自定义目录被删除：读取回退默认目录，且不覆盖设置键（目录恢复后自动生效）。
#[tokio::test]
async fn missing_custom_dir_falls_back_to_default() {
    let (engine, storage, dir) = engine_with_db("image-source-fallback").await;
    let custom = make_dir(&dir, "pics");

    set(&engine, Some(custom.to_str().expect("路径应为 UTF-8")))
        .await
        .expect("设置自定义目录应成功");
    std::fs::remove_dir_all(&custom).expect("删除自定义目录应成功");

    let resolved = resolve_dir(&engine).await.expect("回退读取应成功");
    assert_eq!(
        resolved,
        dir.join(DEFAULT_DIR_NAME)
            .canonicalize()
            .expect("默认目录应已创建"),
        "自定义目录失效时应回退默认目录"
    );
    assert!(
        storage
            .get_setting(SETTING_KEY)
            .await
            .expect("读取设置应成功")
            .is_some(),
        "回退不应删除设置键"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
