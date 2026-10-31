//! crates/ramaria-eval/src/baseline/config/tests.rs - Ramaria 评测基线模型配置测试
//!
//! 设计特点:
//! - 本地 / 云端两类合法配置各断言一次字段映射
//! - 缺字段与取值非法的错误文案逐项断言字段名
//! - 文件缺失单独覆盖，确认提示里带路径与必填字段
//! - 每个用例独立临时文件，结束即删

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

// =========================================================
// 夹具
// =========================================================

/// 写入临时配置文件并返回路径。
fn write_config(tag: &str, text: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("ramaria-eval-config-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    let path = dir.join("eval.toml");
    std::fs::write(&path, text).expect("临时配置应可写入");
    path
}

/// 删除临时配置所在目录。
fn cleanup(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 合法本地配置（地址留空）。
const LOCAL_CONFIG: &str = r#"
[memory]
kind = "local"
base_url = ""
model = "google/gemma-4-e4b"
embedding_path = "C:/models/qwen3-embedding"
"#;

// =========================================================
// 合法配置
// =========================================================

/// 本地模型允许空地址，字段按 trim 后的值生效。
#[test]
fn loads_local_config_with_empty_base_url() {
    let path = write_config("local", LOCAL_CONFIG);
    let config = MemoryConfig::load(&path).expect("本地配置应可加载");
    assert_eq!(config.kind, SandboxModelKind::Local);
    assert_eq!(config.kind_str(), "local");
    assert_eq!(config.kind_label(), "本地模型");
    assert!(config.base_url.is_empty(), "本地模型允许空地址");
    assert_eq!(config.model, "google/gemma-4-e4b");
    assert_eq!(
        config.embedding_path,
        PathBuf::from("C:/models/qwen3-embedding")
    );
    assert_eq!(config.source, path);
    cleanup(&path);
}

/// 云端模型地址与模型标识齐全时通过。
#[test]
fn loads_cloud_config() {
    let path = write_config(
        "cloud",
        r#"
[memory]
kind = "cloud"
base_url = "https://api.example.com/v1"
model = "cloud-model"
embedding_path = "C:/models/qwen3-embedding"
"#,
    );
    let config = MemoryConfig::load(&path).expect("云端配置应可加载");
    assert_eq!(config.kind, SandboxModelKind::Cloud);
    assert_eq!(config.kind_str(), "cloud");
    assert_eq!(config.base_url, "https://api.example.com/v1");
    cleanup(&path);
}

/// 后端选择携带类别、地址、模型标识与向量模型目录。
#[test]
fn backend_choice_is_independent() {
    let path = write_config("choice", LOCAL_CONFIG);
    let config = MemoryConfig::load(&path).expect("本地配置应可加载");
    match config.backend_choice() {
        SandboxBackendChoice::Independent {
            kind,
            base_url,
            model_id,
            embedding_model_path,
        } => {
            assert_eq!(kind, SandboxModelKind::Local);
            assert!(base_url.is_empty());
            assert_eq!(model_id, config.model);
            assert_eq!(
                embedding_model_path,
                Some(PathBuf::from("C:/models/qwen3-embedding"))
            );
        }
        other => panic!("应为独立配置，实际 {other:?}"),
    }
    cleanup(&path);
}

// =========================================================
// 校验失败
// =========================================================

/// 云端模型缺地址时拒绝，文案带字段名。
#[test]
fn rejects_cloud_without_base_url() {
    let path = write_config(
        "cloud-no-url",
        r#"
[memory]
kind = "cloud"
model = "cloud-model"
embedding_path = "C:/models/qwen3-embedding"
"#,
    );
    let error = MemoryConfig::load(&path).expect_err("云端缺地址应报错");
    assert!(
        error.to_string().contains("base_url"),
        "文案应含字段名，实际 {error}"
    );
    cleanup(&path);
}

/// 缺少模型标识时拒绝。
#[test]
fn rejects_missing_model() {
    let path = write_config(
        "no-model",
        r#"
[memory]
kind = "local"
embedding_path = "C:/models/qwen3-embedding"
"#,
    );
    let error = MemoryConfig::load(&path).expect_err("缺模型标识应报错");
    assert!(
        error.to_string().contains("model"),
        "文案应含字段名，实际 {error}"
    );
    cleanup(&path);
}

/// 缺少类别时拒绝。
#[test]
fn rejects_missing_kind() {
    let path = write_config(
        "no-kind",
        r#"
[memory]
model = "m"
embedding_path = "p"
"#,
    );
    let error = MemoryConfig::load(&path).expect_err("缺类别应报错");
    assert!(
        error.to_string().contains("kind"),
        "文案应含字段名，实际 {error}"
    );
    cleanup(&path);
}

/// 类别取值非法时拒绝并列出可选值。
#[test]
fn rejects_invalid_kind() {
    let path = write_config(
        "bad-kind",
        r#"
[memory]
kind = "remote"
model = "m"
embedding_path = "p"
"#,
    );
    let error = MemoryConfig::load(&path).expect_err("非法类别应报错");
    let text = error.to_string();
    assert!(
        text.contains("local / cloud"),
        "文案应列可选值，实际 {text}"
    );
    cleanup(&path);
}

/// 向量模型目录为空时拒绝。
#[test]
fn rejects_empty_embedding_path() {
    let path = write_config(
        "no-embedding",
        r#"
[memory]
kind = "local"
model = "m"
embedding_path = "   "
"#,
    );
    let error = MemoryConfig::load(&path).expect_err("空向量目录应报错");
    assert!(
        error.to_string().contains("embedding_path"),
        "文案应含字段名，实际 {error}"
    );
    cleanup(&path);
}

/// 缺少 [memory] 组时拒绝。
#[test]
fn rejects_missing_section() {
    let path = write_config("no-section", "other = 1\n");
    let error = MemoryConfig::load(&path).expect_err("缺配置组应报错");
    assert!(
        error.to_string().contains("[memory]"),
        "文案应含配置组名，实际 {error}"
    );
    cleanup(&path);
}

/// 文件缺失时提示路径与必填字段。
#[test]
fn reports_missing_file() {
    let path = std::env::temp_dir().join("ramaria-eval-config-missing/eval.toml");
    let error = MemoryConfig::load(&path).expect_err("缺文件应报错");
    let text = error.to_string();
    assert!(text.contains("评测配置缺失"), "文案应说明缺失，实际 {text}");
    assert!(text.contains("eval.toml"), "文案应含路径，实际 {text}");
    assert!(text.contains("embedding_path"), "文案应列必填字段");
}
