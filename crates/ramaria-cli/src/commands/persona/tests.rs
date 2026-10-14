//! crates/ramaria-cli/src/commands/persona/tests.rs - Ramaria 人格命令单元测试
//!
//! 设计特点:
//! - 由 `commands::persona` 以 `#[cfg(test)] mod tests;` 收纳，经 `use super::*` 取用被测项
//! - 覆盖 extract_toml_block / summarize_block / PersonaKind::from_uid 与文件收集过滤

use super::*;
use ramaria_core::types::PersonaKind;

// 注: extract_toml_value 的单元测试已移至 crate::util 模块，
// 此处仅保留 extract_toml_block / summarize_block / PersonaKind::from_uid 的测试。

/// extract_toml_block 各输入参数化验证。
#[test]
fn extract_toml_block_cases() {
    // (toml, key, 期望内容（None=不存在）, 是否要求精确相等)
    let cases = [
        (
            "[blocks]\nA_persona = \"\"\"\n我是测试人格。\n性格：温和。\n\"\"\"\nE_rules = \"\"\"\n规则内容\n\"\"\"",
            "A_persona",
            Some("我是测试人格。\n性格：温和。"),
            false,
        ),
        (
            "[blocks]\nA_persona = \"\"\"\n内容\n\"\"\"",
            "E_rules",
            None,
            false,
        ), // key 不存在
        (
            "[blocks]\nA_persona = \"\"\"\"\"\"",
            "A_persona",
            None,
            false,
        ), // 空块
        (
            "[blocks]\nA_persona = \"\"\"\n\n\n核心内容\n\n\n\"\"\"",
            "A_persona",
            Some("核心内容"),
            true, // 空行清理后精确匹配
        ),
    ];
    for (toml, key, expected, exact) in cases {
        let result = extract_toml_block(toml, key);
        match (result, expected) {
            (Some(content), Some(exp)) => {
                if exact {
                    assert_eq!(content, exp, "key={key}");
                } else {
                    assert!(content.contains(exp), "key={key}");
                }
                assert!(!content.contains("\"\"\""), "key={key}");
            }
            (None, None) => {}
            (r, e) => panic!("key={key}: 期望 {e:?}，实际 {r:?}"),
        }
    }
}

/// summarize_block 各输入参数化验证。
#[test]
fn summarize_block_cases() {
    // 短文本原样
    assert_eq!(summarize_block("短文本", 100), "短文本");
    // 超长截断：预算内含统一省略号 → 总长 50
    let long = "a".repeat(200);
    let result = summarize_block(&long, 50);
    assert!(result.ends_with('…'));
    assert_eq!(result.chars().count(), 50);
    // 多行合并为单行
    let text = "第一行\n第二行\n  第三行有空格  ";
    let result = summarize_block(text, 100);
    assert!(result.contains("第一行"));
    assert!(result.contains("第二行"));
    assert!(result.contains("第三行有空格"));
    assert!(!result.contains('\n'));
}

#[test]
fn persona_kind_from_uid_uses_core() {
    // 验证 PersonaKind::from_uid 的行为（依赖 core 实现）
    assert_eq!(PersonaKind::from_uid("rama-0001"), PersonaKind::Rama);
    assert_eq!(PersonaKind::from_uid("user-0001"), PersonaKind::User);
    assert_eq!(PersonaKind::from_uid("char-0001"), PersonaKind::Char);
    assert_eq!(PersonaKind::from_uid("unknown-xyz"), PersonaKind::Char); // 默认回退
}

#[test]
fn collect_toml_files_filters() {
    // 使用 temp dir 测试文件收集逻辑
    let tmp = std::env::temp_dir().join("ramaria_persona_test_collect");
    let _ = std::fs::create_dir_all(&tmp);

    // 创建测试文件
    std::fs::write(tmp.join("rama-0001.toml"), "name = test").unwrap();
    std::fs::write(tmp.join("user-0001.toml"), "name = user").unwrap();
    std::fs::write(tmp.join("readme.txt"), "not a toml").unwrap();

    // 无过滤
    let all = collect_toml_files(&tmp, None).unwrap();
    assert_eq!(all.len(), 2);

    // 按 UID 过滤
    let filtered = collect_toml_files(&tmp, Some("rama-0001")).unwrap();
    assert_eq!(filtered.len(), 1);
    assert!(filtered[0].ends_with("rama-0001.toml"));

    // 不存在的 UID
    let missing = collect_toml_files(&tmp, Some("nonexistent")).unwrap();
    assert!(missing.is_empty());

    // 清理
    let _ = std::fs::remove_dir_all(&tmp);
}
