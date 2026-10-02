//! crates/ramaria-desktop/src/path_guard/tests.rs - 路径安全校验与日志脱敏单元测试
//!
//! 设计特点:
//! - 覆盖导入/导出路径校验的拒绝与通过路径
//! - 覆盖用户主目录探测与日志脱敏标签（稳定性、可区分性、不含路径/用户名）
//! - 覆盖只读允许根目录集合与读取校验（越权、`..` 穿越、空集合）
//! - 全程使用临时目录并在用后清理，不依赖真实用户文件

use std::fs::File;
use std::io::Write;

use super::*;

// ── 导入路径校验测试 ──

#[test]
fn test_validate_import_file_path_file_not_exists() {
    let result = validate_import_file_path(r"C:\This\Path\Does\Not\Exist.json");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("不存在") || err.contains("拒绝访问"));
}

#[test]
fn test_validate_import_file_path_is_directory() {
    // 使用临时目录（但目录不是文件）
    let tmp = std::env::temp_dir();
    let result = validate_import_file_path(tmp.to_str().unwrap());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("不是普通文件") || err.contains("拒绝访问"));
}

#[test]
fn test_validate_import_file_path_valid_temp_file() {
    // 在临时目录中创建有效文件（临时目录应白名单通过或失败清晰）
    let tmp = std::env::temp_dir();
    let test_file = tmp.join("__ramaria_test_import_valid.tmp");
    let mut f = File::create(&test_file).expect("创建临时文件失败");
    writeln!(f, "test content").ok();

    let result = validate_import_file_path(test_file.to_str().unwrap());

    // 清理
    let _ = std::fs::remove_file(&test_file);

    // temp 目录通常不在白名单内，预期失败但错误消息应清晰
    // 如果 temp 目录恰好是用户主目录下的子目录（极少），可能成功
    if let Err(err) = result {
        assert!(
            err.contains("授权") || err.contains("拒绝"),
            "错误消息应说明白名单限制: {}",
            err
        );
    }
}

#[test]
fn test_validate_import_file_path_empty_path() {
    let result = validate_import_file_path("");
    assert!(result.is_err());
}

// ── 导出路径校验测试 ──

#[test]
fn test_validate_export_path_parent_not_exists() {
    let result = validate_export_path(r"C:\NonExistentDir\output.json");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("不存在") || err.contains("拒绝"),
        "错误消息应说明目录不存在: {}",
        err
    );
}

#[test]
fn test_validate_export_path_no_filename() {
    // 纯目录路径无文件名
    let tmp = std::env::temp_dir();
    let result = validate_export_path(tmp.to_str().unwrap());
    // 纯目录在导出场景应被拒绝（缺少文件名）
    assert!(result.is_err());
}

#[test]
fn test_validate_export_path_system_dir() {
    // 系统目录应被拒绝
    let result = validate_export_path(r"C:\Windows\output.json");
    assert!(result.is_err());
}

// ── 用户主目录探测测试 ──

#[test]
fn test_get_user_home_dir_success() {
    let home = get_user_home_dir();
    assert!(home.is_ok(), "应能获取用户主目录: {:?}", home.err());
    let path = home.unwrap();
    assert!(path.is_absolute(), "主目录应为绝对路径");
    assert!(path.exists(), "主目录应存在");
}

// ── 日志脱敏标签测试 ──

/// 创建测试用唯一临时目录（调用方负责删除）。
fn temp_test_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ramaria-path-guard-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("创建测试临时目录失败");
    dir
}

#[test]
fn redact_path_label_hides_directory_structure() {
    let label = redact_path_label(Path::new(r"C:\Users\Alice\Documents\chat.json"));
    assert!(label.starts_with("chat.json#"), "标签应保留文件名: {label}");
    assert_eq!(label.matches('#').count(), 1, "标签应恰含一个哈希分隔符");
    assert!(!label.contains('\\'), "标签不得含路径分隔符: {label}");
    assert!(!label.contains('/'), "标签不得含路径分隔符: {label}");
    assert!(!label.contains(':'), "标签不得含盘符: {label}");
    assert!(!label.contains("Alice"), "标签不得含用户名: {label}");
}

#[test]
fn redact_path_label_is_stable_and_distinguishable() {
    let a = redact_path_label(Path::new(r"C:\Users\Alice\a.json"));
    let b = redact_path_label(Path::new(r"C:\Users\Alice\a.json"));
    let c = redact_path_label(Path::new(r"C:\Users\Bob\a.json"));
    assert_eq!(a, b, "同一路径应产生稳定标签（便于跨日志关联）");
    assert_ne!(a, c, "不同路径应可区分");

    // 无文件名形态（盘根）回退占位名，不 panic
    assert!(redact_path_label(Path::new(r"C:\")).starts_with("<unknown>#"));
}

#[test]
fn redact_text_label_keeps_length_only() {
    let label = redact_text_label("秘密关键词");
    assert!(
        label.starts_with("<5 chars>#"),
        "文本标签应记录字符数: {label}"
    );
    assert!(!label.contains("秘密"), "文本标签不得含原文: {label}");
    assert_eq!(label.matches('#').count(), 1);
}

// ── 只读允许根目录集合测试 ──

#[test]
fn read_allowed_roots_includes_extra_and_skips_missing() {
    let base = temp_test_dir("roots");
    let real_base = base.canonicalize().expect("canonicalize base");

    let roots = read_allowed_roots(std::slice::from_ref(&base));
    assert!(roots.contains(&real_base), "追加目录应纳入允许根集合");
    assert!(roots.iter().all(|r| r.is_absolute()), "允许根应为绝对路径");

    let missing = base.join("not-exist-dir");
    let roots_with_missing = read_allowed_roots(std::slice::from_ref(&missing));
    assert!(
        !roots_with_missing.iter().any(|r| r == &missing),
        "不存在的授权目录不应产生无效根"
    );

    let _ = std::fs::remove_dir_all(&base);
}

// ── 只读路径校验测试 ──

#[test]
fn validate_read_dir_path_accepts_dir_within_roots() {
    let base = temp_test_dir("dir-ok");
    let roots = vec![base.canonicalize().expect("canonicalize base")];

    let real = validate_read_dir_path(base.to_str().expect("utf8 路径"), &roots)
        .expect("允许根目录内的目录应通过校验");
    assert_eq!(real, roots[0]);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_read_dir_path_rejects_outside_and_empty_roots() {
    let base = temp_test_dir("dir-deny");
    let outside = temp_test_dir("dir-outside");
    let roots = vec![base.canonicalize().expect("canonicalize base")];

    let err = validate_read_dir_path(outside.to_str().expect("utf8 路径"), &roots)
        .expect_err("允许根目录外的目录应被拒绝");
    assert!(
        err.contains("不在允许读取范围内"),
        "错误消息应说明范围限制: {err}"
    );

    let err_empty = validate_read_dir_path(base.to_str().expect("utf8 路径"), &[])
        .expect_err("无允许目录时应被拒绝");
    assert!(
        err_empty.contains("没有可用的允许目录"),
        "错误消息应说明无可用范围: {err_empty}"
    );

    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn validate_read_file_path_accepts_file_and_rejects_escapes() {
    let base = temp_test_dir("file-ok");
    let outside = temp_test_dir("file-outside");
    let roots = vec![base.canonicalize().expect("canonicalize base")];

    let inside_file = base.join("result.json");
    std::fs::write(&inside_file, b"{}").expect("写入测试文件失败");
    assert!(
        validate_read_file_path(inside_file.to_str().expect("utf8 路径"), &roots).is_ok(),
        "允许根目录内的文件应通过校验"
    );

    // 形态 1：直接指向根目录外的文件
    let outside_file = outside.join("result.json");
    std::fs::write(&outside_file, b"{}").expect("写入测试文件失败");
    let err = validate_read_file_path(outside_file.to_str().expect("utf8 路径"), &roots)
        .expect_err("允许根目录外的文件应被拒绝");
    assert!(
        err.contains("不在允许读取范围内"),
        "错误消息应有范围语义: {err}"
    );

    // 形态 2：`..` 穿越（canonicalize 后落到根目录外）
    let sub = base.join("sub");
    std::fs::create_dir_all(&sub).expect("创建子目录失败");
    let escape = sub
        .join("..")
        .join("..")
        .join(outside.file_name().expect("临时目录名"))
        .join("result.json");
    let err_escape = validate_read_file_path(escape.to_str().expect("utf8 路径"), &roots)
        .expect_err("`..` 穿越到根目录外的文件应被拒绝");
    assert!(
        err_escape.contains("不在允许读取范围内"),
        "错误消息应有范围语义: {err_escape}"
    );

    // 形态 3：目录与不存在路径不得当作文件读取
    assert!(validate_read_file_path(sub.to_str().expect("utf8 路径"), &roots).is_err());
    assert!(
        validate_read_file_path(
            base.join("not-exist.json").to_str().expect("utf8 路径"),
            &roots
        )
        .is_err()
    );

    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&outside);
}
