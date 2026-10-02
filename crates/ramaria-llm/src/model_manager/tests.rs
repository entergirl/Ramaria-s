//! crates/ramaria-llm/src/model_manager/tests.rs - 模型管理器单元测试
//!
//! 设计特点:
//! - 使用临时目录，测试后自动清理
//! - 覆盖就绪检查 / 已安装列表 / SHA-256 校验 / 预置查询
//! - 覆盖 Content-Range 起始偏移解析与断点续传追加条件

use super::*;
use std::fs;

/// 测试 ModelManager 创建
#[test]
fn model_manager_creation() {
    let tmp = std::env::temp_dir().join("ramaria_test_models");
    let _ = fs::remove_dir_all(&tmp);

    let _mgr = ModelManager::new(&tmp).unwrap();
    assert!(tmp.exists());

    // 清理
    let _ = fs::remove_dir_all(&tmp);
}

/// 测试模型就绪检查
#[test]
fn is_model_ready_returns_false_for_missing_model() {
    let tmp = std::env::temp_dir().join("ramaria_test_models_empty");
    let _ = fs::remove_dir_all(&tmp);

    let mgr = ModelManager::new(&tmp).unwrap();
    assert!(!mgr.is_model_ready("nonexistent"));

    let _ = fs::remove_dir_all(&tmp);
}

/// 测试列出已安装模型（空目录）
#[test]
fn list_installed_models_empty() {
    let tmp = std::env::temp_dir().join("ramaria_test_models_list");
    let _ = fs::remove_dir_all(&tmp);

    let mgr = ModelManager::new(&tmp).unwrap();
    let models = mgr.list_installed_models().unwrap();
    assert!(models.is_empty());

    let _ = fs::remove_dir_all(&tmp);
}

/// 测试 SHA-256 校验
#[test]
fn verify_checksum_matches() {
    let tmp = std::env::temp_dir().join("ramaria_test_checksum");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let test_file = tmp.join("test.txt");
    fs::write(&test_file, b"hello world").unwrap();

    let mgr = ModelManager::new(&tmp).unwrap();

    // SHA-256 of "hello world"
    let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    assert!(mgr.verify_checksum(&test_file, expected).unwrap());

    // Wrong hash
    assert!(!mgr.verify_checksum(&test_file, "deadbeef").unwrap());

    let _ = fs::remove_dir_all(&tmp);
}

/// 测试获取模型预置信息
#[test]
fn model_preset_lookup() {
    let preset = ModelManager::get_preset("bge-small-zh-v1.5");
    assert!(preset.is_some());
    let p = preset.unwrap();
    assert_eq!(p.hf_repo, "BAAI/bge-small-zh-v1.5");
    assert_eq!(p.files.len(), 3);
}

/// 测试不支持的模型 ID
#[test]
fn unsupported_model_id() {
    let preset = ModelManager::get_preset("nonexistent-model");
    assert!(preset.is_none());
}

/// 测试 Qwen3 预置
#[test]
fn qwen3_preset_exists() {
    let preset = ModelManager::get_preset("Qwen3-Embedding-0.6B");
    assert!(preset.is_some());
    let p = preset.unwrap();
    assert!(p.estimated_size > 1_000_000_000); // >1GB
}

/// 测试模型就绪：创建完整文件后返回 true
#[test]
fn is_model_ready_returns_true_when_files_exist() {
    let tmp = std::env::temp_dir().join("ramaria_test_model_ready");
    let _ = fs::remove_dir_all(&tmp);

    let mgr = ModelManager::new(&tmp).unwrap();
    let model_dir = mgr.model_dir("bge-small-zh-v1.5");
    fs::create_dir_all(&model_dir).unwrap();

    // 创建必需文件
    fs::write(model_dir.join("config.json"), b"{}").unwrap();
    fs::write(model_dir.join("model.safetensors"), b"dummy").unwrap();
    fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();

    assert!(mgr.is_model_ready("bge-small-zh-v1.5"));

    let _ = fs::remove_dir_all(&tmp);
}

/// 测试模型就绪：部分文件缺失
#[test]
fn is_model_ready_false_when_incomplete() {
    let tmp = std::env::temp_dir().join("ramaria_test_model_incomplete");
    let _ = fs::remove_dir_all(&tmp);

    let mgr = ModelManager::new(&tmp).unwrap();
    let model_dir = mgr.model_dir("bge-small-zh-v1.5");
    fs::create_dir_all(&model_dir).unwrap();

    // 只创建 config.json，缺少其他文件
    fs::write(model_dir.join("config.json"), b"{}").unwrap();

    assert!(!mgr.is_model_ready("bge-small-zh-v1.5"));

    let _ = fs::remove_dir_all(&tmp);
}

/// 测试预置模型校验和已填齐：非空、长度为 64、全为小写十六进制字符
#[test]
fn preset_checksums_are_filled() {
    for preset in MODEL_PRESETS {
        for &(filename, checksum) in preset.files {
            assert!(
                !checksum.is_empty(),
                "{} 的 {} 缺少 SHA-256 校验和",
                preset.model_id,
                filename
            );
            assert_eq!(
                checksum.len(),
                64,
                "{} 的 {} SHA-256 校验和长度应为 64",
                preset.model_id,
                filename
            );
            assert!(
                checksum.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "{} 的 {} SHA-256 校验和应为小写十六进制",
                preset.model_id,
                filename
            );
        }
    }
}

/// 测试 Content-Range 起始偏移解析
#[test]
fn parse_content_range_start_cases() {
    assert_eq!(parse_content_range_start("bytes 100-999/1000"), Some(100));
    assert_eq!(parse_content_range_start("bytes 0-0/1"), Some(0));
    // 缺失、缺少 bytes 前缀、起始偏移非数字
    assert_eq!(parse_content_range_start(""), None);
    assert_eq!(parse_content_range_start("100-999/1000"), None);
    assert_eq!(parse_content_range_start("bytes abc-999/1000"), None);
}

/// 测试断点续传追加条件：仅已有数据和 206 响应同时满足才允许追加
#[test]
fn resume_append_allowed_cases() {
    assert!(!resume_append_allowed(0, 200));
    assert!(!resume_append_allowed(100, 200));
    assert!(!resume_append_allowed(0, 206));
    assert!(resume_append_allowed(100, 206));
}
