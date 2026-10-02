//! tests/command_tests/cli_envelope.rs - CLI CLI 进程级信封与确认契约
//!
//! 设计特点:
//! - JSON 信封 stdout 纯净性、错误信封 code、非 TTY 不挂起、--yes 跳过确认
//! - 帮助分组与 blocks/utt 别名
//! - 共享 Mock 基建与装配辅助经 `use super::*` 复用（不调用真实 LLM）

use super::common::{build_test_engine, make_test_persona};
use super::*;
use ramaria_core::types::PersonaKind;

/// `--json` 信封结构 + stdout 纯净性：stdout 仅含一行合法 JSON 信封。
#[test]
fn json_envelope_stdout_purity() {
    let out = run_cli(&["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "status --json 应成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    assert!(parsed["data"]["state"].is_string(), "data.state 应存在");
    assert!(parsed["data"]["db_path"].is_string(), "data.db_path 应存在");
    // stderr 应包含状态提示（信息/日志走 stderr，不污染 stdout）
    assert!(!out.stderr.is_empty(), "stderr 应含日志/提示");
}

/// `--json` 错误信封：业务校验失败 → ok=false + error.code=4。
#[test]
fn json_error_envelope_validation_code() {
    let out = run_cli(&["memory", "l4", "--json"]);
    assert_eq!(out.status.code(), Some(4), "业务校验失败应退出 code 4");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], 4);
    assert!(parsed["error"]["message"].is_string());
    // 文本错误同时走 stderr
    assert!(!out.stderr.is_empty());
}

/// 非 TTY 且无 --yes 不挂起：session delete 直接失败并提示 --yes（M1 B 项）。
#[test]
fn non_tty_without_yes_does_not_hang() {
    let out = run_cli(&["session", "delete", "11111111-1111-1111-1111-111111111111"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "非 TTY 无 --yes 应失败退出（不挂起）"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "提示应包含 --yes，实际: {stderr}");
}

/// `--yes` 自动确认：非 TTY + --yes 跳过确认（M1 B 项）。
#[test]
fn yes_flag_skips_confirmation() {
    let out = run_cli(&[
        "session",
        "delete",
        "11111111-1111-1111-1111-111111111111",
        "--yes",
    ]);
    assert_eq!(out.status.code(), Some(0), "有 --yes 应通过确认并成功退出");
}

/// help 分组：--help 显示 对话/记忆/数据/管理/高级 分组（§2.9）。
#[test]
fn help_grouped_sections() {
    let out = run_cli(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for section in ["对话", "记忆", "数据", "管理", "高级"] {
        assert!(stdout.contains(section), "--help 应包含分组 {section}");
    }
}

/// blocks canonical + utt alias 双支持。
#[test]
fn blocks_and_utt_alias() {
    let out_blocks = run_cli(&["blocks", "rebuild", "--help"]);
    assert_eq!(out_blocks.status.code(), Some(0), "blocks 命令应可用");
    let out_utt = run_cli(&["utt", "rebuild", "--help"]);
    assert_eq!(out_utt.status.code(), Some(0), "utt alias 应可用");
}

// =========================================================
// M1 命令级契约测试
// =========================================================

/// memory 层级别名双支持：summary/events/profile 与 l1/l2/l3 等价。
#[tokio::test]
async fn memory_layer_aliases_ok() {
    let (engine, _storage) = build_test_engine();
    for layer in ["summary", "events", "profile"] {
        let result = ramaria_cli::commands::memory::run(
            &engine,
            ramaria_cli::commands::memory::MemoryArgs {
                layer: layer.to_string(),
                persona: None,
                limit: 10,
                offset: 0,
                json: false,
            },
        )
        .await;
        assert!(result.is_ok(), "层级别名 {layer} 应可用");
    }
}

/// memory 未知层级纠错提示：可用值 summary/events/profile（或 l1/l2/l3）。
#[tokio::test]
async fn memory_unknown_layer_suggestion() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l4".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    let err = result.expect_err("未知层级应报错");
    let msg = format!("{err}");
    for hint in ["summary", "events", "profile"] {
        assert!(msg.contains(hint), "纠错提示应含 {hint}: {msg}");
    }
}

/// persona list：空数据不报错。
#[tokio::test]
async fn persona_list_empty() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::List {
            limit: None,
            offset: 0,
        },
        false,
    )
    .await;
    assert!(result.is_ok());
}

/// persona list：结构化字段（uid/name/kind）不报错。
#[tokio::test]
async fn persona_list_with_data() {
    let (engine, storage) = build_test_engine();
    storage.add_persona(make_test_persona(
        "rama-0001",
        "黎杋枫",
        PersonaKind::Rama,
        None,
    ));
    storage.add_persona(make_test_persona(
        "user-0001",
        "用户",
        PersonaKind::User,
        None,
    ));
    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::List {
            limit: None,
            offset: 0,
        },
        false,
    )
    .await;
    assert!(result.is_ok());
}

/// status 命令（agent 探活）：mock 引擎可执行。
#[tokio::test]
async fn status_command_ok() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::status::run(
        &engine,
        ramaria_cli::commands::status::StatusArgs {
            db_path: std::path::PathBuf::from("data/test.db"),
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

/// import --dry-run 不产生任何数据库写入（命令级：无文件时不报 panic）。
/// 注：真实 dry-run 路径依赖 qq-chat-exporter 文件，进程级验证由 M1 手动验收覆盖；
/// 此处验证 dry_run 参数不影响现有命令行为（文件缺失仍为业务校验错误）。
#[tokio::test]
async fn config_set_model_id_roundtrip() {
    let dir = temp_config_dir("model");
    let engine = build_config_test_engine(&dir).await;

    // 设置 model_id（写入 capability.model_id，与 get_config 对称）
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "model_id".to_string(),
            value: "qwen3-8b".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.capability.model_id, "qwen3-8b");

    // 空值拒绝
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "model_id".to_string(),
            value: "".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_err());

    close_engine_pool(&engine).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn import_dry_run_missing_file_is_validation_error() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::import_cmd::run(
        &engine,
        ramaria_cli::commands::import_cmd::ImportArgs {
            file: std::path::PathBuf::from("nonexistent_file.json"),
            deep: false,
            dry_run: true,
            persona_self_name: None,
            persona_self_uid: None,
            persona_other_name: None,
            persona_other_uid: None,
            gap: 10,
            side: ramaria_importer::qq::ImportSide::Both,
            no_report: false,
            yes: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_err(), "文件不存在应报错");
}

// =========================================================
// 知识层 fact 命令契约（只读，无 delete）
// =========================================================
// 覆盖:
// - list: 空数据 / 有数据按 field 过滤（命令级 + 进程级 --json 信封）
// - show: 单条详情 + 版本链（命令级）；不存在 → exit code 4（进程级）
// - **无 delete 子命令断言**（clap 子命令列表不含 delete，进程级）
// - 版本链只读展示：superseded 版本沿 version_of 回溯可见
