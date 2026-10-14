//! tests/persona_tests.rs - CLI `persona import` 子命令契约测试
//!
//! 覆盖:
//! - dry-run：预览信封（注册 / 将写入计数 / 拆解状态）与不写库保证
//! - 导入执行：写入统计 / 列表读回 / 直连库断言 trait 来源为 Manual
//! - 合并：二次导入 mode=merge、四类零写入、merge_mode 提示
//! - `--into` 目标不存在：按新建处理 + merge_target_missing 提示
//! - 文本形态与错误路径（文件不存在 / 非法 TOML 的退出码）
//!
//! 安全约束:
//! - 进程级用例使用临时库（--db 指向临时目录），不触碰用户数据
//! - 不调用真实 LLM（--no-ai）与嵌入模型

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::TraitSource;
use ramaria_storage::SqliteStorage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// 进程级临时库序号（并行测试线程安全）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

// =========================================================
// 共享夹具与工具
// =========================================================

/// 合法预设（超集 TOML）：注册段 + identity + 两层 traits + 三类条目 + 自由文本块。
const PRESET_TOML: &str = r#"[persona]
uid = "char-0001"
kind = "char"
name = "导入人格"

[identity]
assistant_name = "导入人格"

[blocks]
A_persona = "人格设定文本。"

[traits]
base = [{ label = "温和", meaning = "待人温和" }]
primary = [{ label = "慢热", meaning = "熟悉后放松" }]

[[facts]]
field = "BasicInfo"
content = "示例事实"

[[examples]]
user = "在吗"
assistant = "在的"

[[behavior_rules]]
keywords = ["加班"]
reaction = "提醒休息"
"#;

/// 创建唯一临时测试目录（库与预设文件位于其中）。
fn temp_test_dir(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-persona-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 写入预设文件到临时目录（返回文件路径）。
fn write_preset(dir: &Path, file_name: &str, content: &str) -> PathBuf {
    let path = dir.join(file_name);
    std::fs::write(&path, content).expect("写入预设文件应成功");
    path
}

/// 以临时库运行 CLI 进程（库文件固定为目录内 `persona.db`）。
fn run_cli(db_dir: &Path, args: &[&str]) -> std::process::Output {
    let db = db_dir.join("persona.db");
    std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(&db)
        .output()
        .expect("运行 ramaria 二进制失败")
}

/// 解析 stdout 的 JSON 信封（断言 stdout 仅一行）。
fn parse_envelope(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON")
}

/// `persona list --json` 的 persona 条目数组。
fn list_personas(dir: &Path) -> Vec<serde_json::Value> {
    let out = run_cli(dir, &["persona", "list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "persona list 应成功退出");
    let envelope = parse_envelope(&out);
    envelope["data"]["personas"]
        .as_array()
        .expect("personas 应为数组")
        .clone()
}

// =========================================================
// dry-run 预览
// =========================================================

/// dry-run：预览字段与预设一致，且不写库。
#[test]
fn dry_run_previews_without_writing() {
    let dir = temp_test_dir("dry-run");
    let preset = write_preset(&dir, "char-0001.toml", PRESET_TOML);
    let file = preset.to_str().expect("预设路径应为合法 UTF-8");

    let out = run_cli(
        &dir,
        &["persona", "import", file, "--dry-run", "--json", "--no-ai"],
    );
    assert_eq!(out.status.code(), Some(0), "dry-run 应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    assert_eq!(data["registration"]["uid"], "char-0001");
    assert_eq!(data["registration"]["mode"], "create");
    assert_eq!(data["will_write"]["traits"]["base"], 1);
    assert_eq!(data["will_write"]["traits"]["primary"], 1);
    assert_eq!(data["will_write"]["traits"]["accent"], 0);
    assert_eq!(data["will_write"]["facts"]["count"], 1);
    assert_eq!(data["will_write"]["examples"]["count"], 1);
    assert_eq!(data["will_write"]["behavior_rules"]["count"], 1);
    assert_eq!(data["will_write"]["blocks"]["segments"], 1);
    assert_eq!(data["decompose"]["enabled"], false, "--no-ai 时拆解未启用");

    // 库中不应存在 char-0001（dry-run 未写库）
    let rows = list_personas(&dir);
    assert!(
        rows.iter().all(|p| p["uid"] != "char-0001"),
        "dry-run 后库中不应存在 char-0001: {rows:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// dry-run 文本形态：stdout 含预览头与将写入段。
#[test]
fn dry_run_text_form_renders_preview() {
    let dir = temp_test_dir("dry-text");
    let preset = write_preset(&dir, "char-0001.toml", PRESET_TOML);
    let file = preset.to_str().expect("预设路径应为合法 UTF-8");

    let out = run_cli(&dir, &["persona", "import", file, "--dry-run", "--no-ai"]);
    assert_eq!(out.status.code(), Some(0), "文本 dry-run 应成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("预设预览："), "应含预览头: {stdout}");
    assert!(stdout.contains("将写入："), "应含将写入段: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `--into` 目标不存在：按新建处理并给出 merge_target_missing 提示。
#[test]
fn into_missing_target_reports_create_with_hint() {
    let dir = temp_test_dir("into-missing");
    let preset = write_preset(&dir, "char-0001.toml", PRESET_TOML);
    let file = preset.to_str().expect("预设路径应为合法 UTF-8");

    let out = run_cli(
        &dir,
        &[
            "persona",
            "import",
            file,
            "--into",
            "char-9999",
            "--dry-run",
            "--json",
            "--no-ai",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "dry-run 应成功退出");
    let envelope = parse_envelope(&out);
    let data = &envelope["data"];
    assert_eq!(
        data["registration"]["uid"], "char-9999",
        "--into 优先于预设 uid"
    );
    assert_eq!(
        data["registration"]["mode"], "create",
        "目标不存在按新建处理"
    );
    let hints = data["hints"].as_array().expect("hints 应为数组");
    assert!(
        hints.iter().any(|h| h["code"] == "merge_target_missing"),
        "应含 merge_target_missing 提示: {hints:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 导入执行与合并
// =========================================================

/// 导入执行：写入统计正确、列表读回、直连库来源为 Manual。
#[tokio::test]
async fn import_executes_and_persists_manual_entries() {
    let dir = temp_test_dir("execute");
    let preset = write_preset(&dir, "char-0001.toml", PRESET_TOML);
    let file = preset.to_str().expect("预设路径应为合法 UTF-8");

    let out = run_cli(&dir, &["persona", "import", file, "--json", "--no-ai"]);
    assert_eq!(out.status.code(), Some(0), "导入应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    assert_eq!(data["preview"]["registration"]["uid"], "char-0001");
    assert_eq!(data["preview"]["registration"]["mode"], "create");
    assert_eq!(data["written"]["traits"], 2);
    assert_eq!(data["written"]["facts"], 1);
    assert_eq!(data["written"]["examples"], 1);
    assert_eq!(data["written"]["behavior_rules"], 1);
    assert_eq!(data["written"]["failed"], 0);
    assert_eq!(data["decompose"]["executed"], false);

    // 列表读回
    let rows = list_personas(&dir);
    assert!(
        rows.iter().any(|p| p["uid"] == "char-0001"),
        "列表应含导入的人格: {rows:?}"
    );

    // 直连临时库：trait 来源为 Manual（预设直写通道）
    let pool = ramaria_storage::database::init_pool(Some(dir.join("persona.db")))
        .await
        .expect("打开测试库应成功");
    let storage = SqliteStorage::new(pool.clone());
    let traits = storage
        .list_traits_by_persona("char-0001")
        .await
        .expect("查询 traits 应成功");
    assert_eq!(traits.len(), 2, "应写入两条 trait");
    assert!(
        traits.iter().all(|t| t.source == TraitSource::Manual),
        "预设导入的 trait 来源应为 Manual: {traits:?}"
    );
    pool.close().await;

    let _ = std::fs::remove_dir_all(&dir);
}

/// 二次导入同一预设：按合并处理，四类条目零写入并给出 merge_mode 提示。
#[test]
fn reimport_merges_without_new_writes() {
    let dir = temp_test_dir("merge");
    let preset = write_preset(&dir, "char-0001.toml", PRESET_TOML);
    let file = preset.to_str().expect("预设路径应为合法 UTF-8");

    let out = run_cli(&dir, &["persona", "import", file, "--json", "--no-ai"]);
    assert_eq!(out.status.code(), Some(0), "首次导入应成功退出");

    let out = run_cli(&dir, &["persona", "import", file, "--json", "--no-ai"]);
    assert_eq!(out.status.code(), Some(0), "二次导入应成功退出");
    let envelope = parse_envelope(&out);
    let data = &envelope["data"];
    assert_eq!(data["preview"]["registration"]["mode"], "merge");
    assert_eq!(data["written"]["traits"], 0);
    assert_eq!(data["written"]["facts"], 0);
    assert_eq!(data["written"]["examples"], 0);
    assert_eq!(data["written"]["behavior_rules"], 0);
    let hints = data["preview"]["hints"].as_array().expect("hints 应为数组");
    assert!(
        hints.iter().any(|h| h["code"] == "merge_mode"),
        "合并应给出 merge_mode 提示: {hints:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 错误路径
// =========================================================

/// 文件不存在与非法 TOML：非零退出且 stderr 非空。
#[test]
fn import_errors_report_nonzero_exit() {
    let dir = temp_test_dir("errors");

    // 文件不存在：读取失败为 Io 错误，按 main.rs 映射归通用失败（exit code 1）
    let missing = dir.join("missing.toml");
    let missing = missing.to_str().expect("路径应为合法 UTF-8");
    let out = run_cli(&dir, &["persona", "import", missing, "--no-ai"]);
    assert_eq!(out.status.code(), Some(1), "Io 错误应退出 code 1");
    assert!(!out.stderr.is_empty(), "错误应输出到 stderr");

    // 非法 TOML（语法错）：解析失败，非零退出
    let bad = write_preset(&dir, "bad.toml", "[persona\nuid =");
    let bad = bad.to_str().expect("路径应为合法 UTF-8");
    let out = run_cli(&dir, &["persona", "import", bad, "--no-ai"]);
    assert_ne!(out.status.code(), Some(0), "非法 TOML 应非零退出");
    assert!(!out.stderr.is_empty(), "错误应输出到 stderr");

    let _ = std::fs::remove_dir_all(&dir);
}
