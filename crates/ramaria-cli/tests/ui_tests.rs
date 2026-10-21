//! tests/ui_tests.rs - UI 工具函数单元测试
//!
//! 覆盖: mask_key, truncate, format_timestamp 等纯函数
//! 安全: 不调用真实 LLM、不访问 keychain、不读写文件系统

mod common;

// =========================================================
// mask_key 测试
// =========================================================

#[test]
fn mask_key_normal() {
    let masked = ramaria_cli::ui::mask_key("sk-abc123def456");
    assert_eq!(masked, "sk-****456");
    assert!(!masked.contains("abc123def456"));
}

#[test]
fn mask_key_short_4_chars() {
    // key 长度 ≤4 时应显示 ****
    assert_eq!(ramaria_cli::ui::mask_key("abc"), "****");
    assert_eq!(ramaria_cli::ui::mask_key("abcd"), "****");
}

// （原 mask_key_short_3_chars 与 mask_key_short_4_chars 首行断言完全重复，已删除）

#[test]
fn mask_key_empty() {
    assert_eq!(ramaria_cli::ui::mask_key(""), "****");
}

/// 非 ASCII key 按字符取首尾，不在 UTF-8 边界中间切断。
#[test]
fn mask_key_multibyte_chars() {
    // 8 个汉字（24 字节）：首尾各保留 3 个字符
    assert_eq!(
        ramaria_cli::ui::mask_key("密钥密钥密钥密钥"),
        "密钥密****钥密钥"
    );
    // 4 个汉字（12 字节）即便字节长度 > 4 也按字符数回退
    assert_eq!(ramaria_cli::ui::mask_key("密钥密钥"), "****");
}

#[test]
fn mask_key_very_long() {
    let long_key = "sk-".to_string() + &"a".repeat(100);
    let masked = ramaria_cli::ui::mask_key(&long_key);
    assert!(masked.starts_with("sk-"));
    assert!(masked.contains("****"));
    assert_eq!(masked.len(), "sk-****".len() + 3); // prefix(3) + **** + suffix(3)
}

// =========================================================
// labeled 输出测试
// （原 labeled_formats_correctly 为空占位测试，无任何断言，已删除）
// =========================================================

// =========================================================
// truncate / format_timestamp 测试
// 注: 这两个函数已提取至 ramaria_cli::util 模块（pub），
// 其完整单元测试在 util.rs 的 #[cfg(test)] 块中。
// 此处原有的集成冒烟测试与 util.rs 单元测试完全重复，已删除。
// =========================================================
