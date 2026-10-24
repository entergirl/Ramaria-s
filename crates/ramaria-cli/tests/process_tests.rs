//! tests/process_tests.rs - CLI 进程级输出路径与状态回执测试
//!
//! 覆盖:
//! - `export` / `diagnostics` 的裸文件名输出（无目录前缀）写入当前目录
//! - `config set` 的状态回执按变更后的配置判定（嵌入目录失效 → degraded）
//!
//! 安全约束:
//! - 真实 CLI 二进制（`CARGO_BIN_EXE_ramaria`）+ 临时目录，不调用真实 LLM、不连网
//! - 嵌入模型目录用占位文件构造（可用性只校验文件存在，不加载权重）

use ramaria_core::traits::StoreInfrastructure;
use ramaria_core::types::{BackendConfig, LlmProvider};
use ramaria_storage::SqliteStorage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// 临时目录序号（同进程内多次建目录时避免撞名）。
static TEMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建本次用例独占的临时目录。
fn temp_dir(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "ramaria_process_{tag}_{}_{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 以真实 CLI 二进制运行（工作目录限定在临时目录，便于断言裸文件名输出）。
fn run_cli(cwd: &Path, db: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .current_dir(cwd)
        .args(args)
        .arg("--db")
        .arg(db)
        .output()
        .expect("运行 ramaria 二进制失败")
}

/// 断言进程成功退出，失败时带上 stderr。
fn assert_success(out: &std::process::Output, context: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{context} 应成功退出；stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 播种后端配置（可选嵌入模型路径）。
async fn seed_backend_config(db: &Path, embedding_path: Option<&Path>) {
    let pool = ramaria_storage::database::init_pool(Some(db.to_path_buf()))
        .await
        .expect("初始化测试数据库失败");
    let storage = SqliteStorage::new(pool);
    let mut config = BackendConfig::new_with_defaults(
        LlmProvider::LmStudio,
        "http://127.0.0.1:1234/v1".to_string(),
        String::new(),
    );
    config.embedding_model_path = embedding_path.map(|path| path.to_string_lossy().to_string());
    storage
        .save_backend_config(&config)
        .await
        .expect("写入后端配置应成功");
    storage
        .set_setting(ramaria_service::SETUP_COMPLETED_KEY, "1")
        .await
        .expect("写入完成标记应成功");
}

/// 构造只含必需文件的占位嵌入模型目录（就绪判定只查文件存在）。
fn fake_model_dir(root: &Path) -> PathBuf {
    let dir = root.join("embedding-model");
    std::fs::create_dir_all(&dir).expect("创建模型目录应成功");
    for file in ["config.json", "model.safetensors", "tokenizer.json"] {
        std::fs::write(dir.join(file), b"{}").expect("写入占位模型文件应成功");
    }
    dir
}

// =========================================================
// 输出路径：裸文件名
// =========================================================

/// `export -o out.json`（无目录前缀）应写入当前目录。
#[test]
fn export_accepts_bare_file_name() {
    let dir = temp_dir("export-bare");
    let db = dir.join("ramaria.db");

    let out = run_cli(&dir, &db, &["export", "json", "-o", "out.json"]);
    assert_success(&out, "裸文件名导出");

    let written = dir.join("out.json");
    assert!(written.is_file(), "应在当前目录生成 out.json");
    let text = std::fs::read_to_string(&written).expect("读取导出文件失败");
    serde_json::from_str::<serde_json::Value>(&text).expect("导出文件应为合法 JSON");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `export -o ./out.json`（显式当前目录）同样成功，两种写法结果一致。
#[test]
fn export_accepts_explicit_current_dir_prefix() {
    let dir = temp_dir("export-dot");
    let db = dir.join("ramaria.db");

    let out = run_cli(&dir, &db, &["export", "json", "-o", "./out.json"]);
    assert_success(&out, "带 ./ 前缀导出");

    assert!(dir.join("out.json").is_file(), "应在当前目录生成 out.json");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `diagnostics -o diag.zip`（无目录前缀）应生成诊断包。
#[test]
fn diagnostics_accepts_bare_file_name() {
    let dir = temp_dir("diag-bare");
    let db = dir.join("ramaria.db");

    let out = run_cli(&dir, &db, &["diagnostics", "-o", "diag.zip"]);
    assert_success(&out, "裸文件名诊断导出");

    let written = dir.join("diag.zip");
    let size = std::fs::metadata(&written).expect("应生成诊断包").len();
    assert!(size > 0, "诊断包不应为空");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 状态回执：按变更后的配置判定
// =========================================================

/// 嵌入目录从可用改为不存在后，`config set` 回执应为 degraded
/// （配置路径失效即视为嵌入不可用，不回显变更前状态）。
#[tokio::test]
async fn config_set_reports_degraded_after_embedding_dir_removed() {
    let dir = temp_dir("config-state");
    let db = dir.join("ramaria.db");
    let model_dir = fake_model_dir(&dir);
    seed_backend_config(&db, Some(&model_dir)).await;

    // 初始态：模型目录齐全 → ready
    let out = run_cli(&dir, &db, &["config", "get", "state"]);
    assert_success(&out, "读取初始状态");
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(state, "ready", "占位模型目录齐全时应为 ready");

    // 改为不存在的目录 → degraded
    let missing = dir.join("missing-model");
    let out = run_cli(
        &dir,
        &db,
        &[
            "config",
            "set",
            "embedding_model_path",
            missing.to_str().expect("临时路径应为合法 UTF-8"),
        ],
    );
    assert_success(&out, "设置失效嵌入路径");
    // 状态回执经 ui::info 输出（stderr，不污染 stdout 数据流）
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("degraded"),
        "回执应为变更后的 degraded，实际 stderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
