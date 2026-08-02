//! rust/crates/ramaria-app/tests/model_manager_tests.rs — 模型管理器集成测试
//!
//! 设计特点:
//! - 使用临时目录进行所有测试，测试后自动清理
//! - 覆盖：目录创建、模型就绪检查、已安装模型列表、删除模型、校验和验证
//! - 不执行真实下载（需要网络），仅测试文件系统操作

use ramaria_app::model_manager::ModelManager;
use std::fs;

// =========================================================
// 测试辅助函数
// =========================================================

fn temp_models_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ramaria_test_models_{}", uuid::Uuid::new_v4()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn cleanup(dir: &std::path::Path) {
    let _ = fs::remove_dir_all(dir);
}

// =========================================================
// 基本操作测试
// =========================================================

#[test]
fn create_model_manager_creates_directory() {
    let root = temp_models_root();
    assert!(!root.exists());

    let _ = ModelManager::new(&root).unwrap();
    assert!(root.exists());

    cleanup(&root);
}

#[test]
fn model_not_ready_when_empty() {
    let root = temp_models_root();
    let mgr = ModelManager::new(&root).unwrap();
    assert!(!mgr.is_model_ready("bge-small-zh-v1.5"));
    cleanup(&root);
}

#[test]
fn list_installed_models_empty() {
    let root = temp_models_root();
    let mgr = ModelManager::new(&root).unwrap();
    let models = mgr.list_installed_models().unwrap();
    assert!(models.is_empty());
    cleanup(&root);
}

#[test]
fn model_ready_when_files_exist() {
    let root = temp_models_root();
    let mgr = ModelManager::new(&root).unwrap();

    let model_dir = mgr.model_dir("bge-small-zh-v1.5");
    fs::create_dir_all(&model_dir).unwrap();
    fs::write(model_dir.join("config.json"), b"{}").unwrap();
    fs::write(model_dir.join("model.safetensors"), b"dummy weights").unwrap();
    fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();

    assert!(mgr.is_model_ready("bge-small-zh-v1.5"));

    // 列表应包含此模型
    let models = mgr.list_installed_models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0], "bge-small-zh-v1.5");

    cleanup(&root);
}

#[test]
fn model_not_ready_when_missing_files() {
    let root = temp_models_root();
    let mgr = ModelManager::new(&root).unwrap();

    let model_dir = mgr.model_dir("bge-small-zh-v1.5");
    fs::create_dir_all(&model_dir).unwrap();
    // 只创建 config.json，缺少 model.safetensors 和 tokenizer.json
    fs::write(model_dir.join("config.json"), b"{}").unwrap();

    assert!(!mgr.is_model_ready("bge-small-zh-v1.5"));
    cleanup(&root);
}

#[test]
fn remove_model_deletes_directory() {
    let root = temp_models_root();
    let mgr = ModelManager::new(&root).unwrap();

    let model_dir = mgr.model_dir("test-model");
    fs::create_dir_all(&model_dir).unwrap();
    fs::write(model_dir.join("config.json"), b"{}").unwrap();
    fs::write(model_dir.join("model.safetensors"), b"dummy").unwrap();
    fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();

    assert!(mgr.is_model_ready("test-model"));

    mgr.remove_model("test-model").unwrap();
    assert!(!mgr.is_model_ready("test-model"));
    assert!(!model_dir.exists());

    cleanup(&root);
}

#[test]
fn model_size_calculation() {
    let root = temp_models_root();
    let mgr = ModelManager::new(&root).unwrap();

    let model_dir = mgr.model_dir("test-model");
    fs::create_dir_all(&model_dir).unwrap();
    fs::write(model_dir.join("config.json"), vec![0u8; 1024]).unwrap();
    fs::write(model_dir.join("model.safetensors"), vec![0u8; 2048]).unwrap();
    fs::write(model_dir.join("tokenizer.json"), vec![0u8; 512]).unwrap();

    let size = mgr.model_size("test-model");
    assert!(size >= 3584); // at least 1024 + 2048 + 512
    cleanup(&root);
}

#[test]
fn verify_checksum_matches() {
    let root = temp_models_root();
    fs::create_dir_all(&root).unwrap();

    let test_file = root.join("test.bin");
    fs::write(&test_file, b"hello world").unwrap();

    let mgr = ModelManager::new(&root).unwrap();

    // SHA-256 of "hello world" from `sha256sum`
    let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    assert!(mgr.verify_checksum(&test_file, expected).unwrap());

    // Wrong hash
    assert!(
        !mgr.verify_checksum(
            &test_file,
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        )
        .unwrap()
    );

    cleanup(&root);
}

// （原 model_dir_consistent_path 仅断言 model_dir(name) 以 name 结尾，
//  为 getter 复述实现，已删除）

#[test]
fn repeated_creation_is_idempotent() {
    let root = temp_models_root();
    let mgr1 = ModelManager::new(&root).unwrap();
    // 第二次创建不应报错
    let mgr2 = ModelManager::new(&root).unwrap();

    assert_eq!(
        mgr1.model_dir("test").to_string_lossy(),
        mgr2.model_dir("test").to_string_lossy()
    );

    cleanup(&root);
}
