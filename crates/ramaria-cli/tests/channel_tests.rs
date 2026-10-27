//! tests/channel_tests.rs - CLI `channel` 子命令契约测试
//!
//! 覆盖:
//! - `channel status --json`：信封字段完整（配置摘要 + 凭据状态 + 绑定数）
//! - `channel bindings --json`：空库输出空数组；写入后条目字段与开关如实回显
//! - `channel bind`：创建 / 幂等重放 / 开关更新；persona 不存在为业务校验错误（exit code 4）
//! - 命令级真库路径：绑定到已存在人格时生效人格取绑定覆盖
//!
//! 安全约束:
//! - 进程级用例使用临时库（--db 指向临时目录），不触碰用户数据
//! - 不访问真实平台凭据（读取失败按未配置处理）；不调用真实 LLM

mod common;

use ramaria_cli::commands::channel::ChannelCmd;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StorageBackend, StoreCrud};
use ramaria_core::types::{Persona, PersonaKind};
use ramaria_service::Engine;
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// 进程级临时库序号（并行测试线程安全）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建唯一临时测试目录（库文件位于其中）。
fn temp_test_dir(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-channel-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 以临时库运行 CLI 进程（返回输出）；库文件固定为目录内 `channel.db`。
fn run_cli(db_dir: &Path, args: &[&str]) -> std::process::Output {
    let db = db_dir.join("channel.db");
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

// =========================================================
// 进程级用例（真库 + 真实 CLI 进程）
// =========================================================

/// status 信封字段完整；空库 bindings 输出空数组。
#[test]
fn status_json_envelope_and_empty_bindings() {
    let dir = temp_test_dir("status");

    let out = run_cli(&dir, &["channel", "status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "status --json 应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    for key in [
        "enabled",
        "qq_enabled",
        "telegram_enabled",
        "qq_ws_url",
        "default_persona_uid",
        "proactive_delivery_enabled",
        "qq_token_configured",
        "telegram_token_configured",
        "bindings_count",
    ] {
        assert!(data.get(key).is_some(), "data 应含字段 {key}: {data}");
    }
    assert_eq!(data["enabled"], false, "通道默认关闭");
    assert_eq!(data["bindings_count"], 0, "空库绑定数应为 0");

    let out = run_cli(&dir, &["channel", "bindings", "--json"]);
    assert_eq!(out.status.code(), Some(0), "bindings --json 应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["data"],
        serde_json::json!([]),
        "空库绑定列表应为空数组"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 绑定创建 / 幂等重放 / 开关更新：同一库连续命令口径一致。
#[test]
fn bind_creates_updates_and_is_idempotent() {
    let dir = temp_test_dir("bind");

    // 创建（无 persona 覆盖：生效人格回退通道默认）
    let out = run_cli(&dir, &["channel", "bind", "qq", "c2c:10001", "--json"]);
    assert_eq!(out.status.code(), Some(0), "创建绑定应成功");
    let envelope = parse_envelope(&out);
    let data = &envelope["data"];
    assert_eq!(data["channel"], "qq");
    assert_eq!(data["external_ref"], "c2c:10001");
    assert!(
        data["persona_uid"].is_null(),
        "未覆盖时 persona_uid 应为 null"
    );
    assert_eq!(data["effective_persona_uid"], "rama-0001");
    assert_eq!(
        data["enabled"], false,
        "自动建档默认禁用（白名单载体，须显式放行）"
    );
    assert_eq!(data["allow_proactive"], false, "主动投递默认关");

    // 幂等重放：再次执行同命令仍成功且不新增
    let out = run_cli(&dir, &["channel", "bind", "qq", "c2c:10001", "--json"]);
    assert_eq!(out.status.code(), Some(0), "幂等重放应成功");

    // 开关更新：--disable / --deny-proactive
    let out = run_cli(
        &dir,
        &[
            "channel",
            "bind",
            "qq",
            "c2c:10001",
            "--disable",
            "--deny-proactive",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["data"]["enabled"], false);
    assert_eq!(envelope["data"]["allow_proactive"], false);

    // 列表读回：同一绑定仅一行
    let out = run_cli(&dir, &["channel", "bindings", "--json"]);
    let envelope = parse_envelope(&out);
    let rows = envelope["data"].as_array().expect("data 应为数组").clone();
    assert_eq!(rows.len(), 1, "应恰有一条绑定：{rows:?}");
    assert_eq!(rows[0]["external_ref"], "c2c:10001");

    let _ = std::fs::remove_dir_all(&dir);
}

/// persona 不存在：业务校验错误文案 + exit code 4。
#[test]
fn bind_missing_persona_reports_validation_and_exit_code() {
    let dir = temp_test_dir("missing-persona");

    let out = run_cli(
        &dir,
        &[
            "channel",
            "bind",
            "qq",
            "c2c:10001",
            "--persona",
            "char-9999",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(4), "业务校验失败应退出 code 4");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], 4);
    let message = envelope["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("绑定人格不存在: char-9999"),
        "错误文案应含目标人格: {message}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 命令级用例（真库引擎 + 预置人格）
// =========================================================

/// 装配真实 SQLite 引擎（空库，自动执行 migration）。
async fn setup_engine(tag: &str) -> (Arc<Engine>, Arc<SqliteStorage>, SqlitePool, PathBuf) {
    let dir = temp_test_dir(tag);
    let db = dir.join("channel.db");
    let pool = ramaria_storage::database::init_pool(Some(db))
        .await
        .expect("初始化测试数据库失败");
    let storage = Arc::new(SqliteStorage::new(pool.clone()));
    let storage_dyn: Arc<dyn StorageBackend> = storage.clone();
    let engine = Engine::from_parts(
        storage_dyn,
        Arc::new(common::MockLlm::new("channel-test")),
        None,
        RamariaConfig::default(),
    );
    (Arc::new(engine), storage, pool, dir)
}

/// 绑定到已存在人格：生效人格取绑定覆盖（不是通道默认）。
#[tokio::test]
async fn bind_with_existing_persona_sets_override() {
    let (engine, storage, pool, dir) = setup_engine("override").await;
    let persona = Persona::new(
        "char-0001".to_string(),
        "测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("写入 persona 应成功");

    let binding = engine
        .channel_set_binding("qq", "group:123456", Some("char-0001"), None, Some(true))
        .await
        .expect("绑定写入应成功");
    assert_eq!(binding.persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(
        engine.channel_effective_persona_uid(&binding).await,
        "char-0001"
    );
    assert!(binding.allow_proactive, "主动投递开关应如实回显");

    // 命令层可读回（bindings 空输出路径之外的读写一致性）
    let result = ramaria_cli::commands::channel::run(&engine, ChannelCmd::Bindings, false).await;
    assert!(result.is_ok(), "bindings 命令应成功: {result:?}");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
