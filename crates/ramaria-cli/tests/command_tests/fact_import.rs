//! tests/command_tests/fact_import.rs - CLI 知识事实 / 导入 / setup 命令集成测试
//!
//! 设计特点:
//! - fact list / show（缺失为校验失败、无 delete 子命令）
//! - import 报告掩码 / --no-report / --quiet
//! - setup / chat 交互式与 --json 冲突显式不支持；blocks / index / diagnostics 信封
//! - 共享 Mock 基建与装配辅助经 `use super::*` 复用（不调用真实 LLM）

use super::common::build_test_engine;
use super::*;
use ramaria_core::traits::StoreCrud;

/// fact list：空数据 → 空数组（命令级）。
#[tokio::test]
async fn fact_list_empty_returns_empty() {
    let (engine, _storage) = build_test_engine();
    let page = engine
        .memory_facts(ramaria_service::FactBrowseRequest {
            persona: "rama-0001".to_string(),
            field: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    assert!(page.items.is_empty(), "无数据时应返回空数组");
}

/// fact list：有数据返回 active 事实，按 field 过滤生效（命令级）。
#[tokio::test]
async fn fact_list_filters_by_field() {
    let (engine, storage) = build_test_engine();
    use ramaria_core::types::{FactStatus, ProfileField};
    // 两条不同 field 的 active 事实 + 一条 superseded（不应出现在 active list）
    let interest = make_test_fact("rama-0001", ProfileField::Interests, "喜欢科幻电影");
    storage.add_fact(interest.clone());
    let social = make_test_fact("rama-0001", ProfileField::Social, "有一个同学叫小李");
    storage.add_fact(social.clone());
    let mut old = make_test_fact("rama-0001", ProfileField::Interests, "旧兴趣（已覆盖）");
    old.status = FactStatus::Superseded;
    storage.add_fact(old);

    // 不按 field：只返回 active 两条
    let page = engine
        .memory_facts(ramaria_service::FactBrowseRequest {
            persona: "rama-0001".to_string(),
            field: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    let all = page.items;
    assert_eq!(all.len(), 2, "superseded 不应出现在 active list");
    assert!(all.iter().all(|f| f.status == FactStatus::Active));

    // 按 field=interests：仅兴趣
    let page = engine
        .memory_facts(ramaria_service::FactBrowseRequest {
            persona: "rama-0001".to_string(),
            field: Some(ProfileField::Interests),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    let interests = page.items;
    assert_eq!(interests.len(), 1);
    assert_eq!(interests[0].content, "喜欢科幻电影");
}

/// fact show：单条详情 + 完整版本链（命令级，链头最早在前）。
#[tokio::test]
async fn fact_show_versions_chain() {
    let (engine, storage) = build_test_engine();
    use ramaria_core::types::ProfileField;

    // 版本链：旧事实 → 新事实（新 version_of 指向旧）
    let old = make_test_fact("rama-0001", ProfileField::RecentContext, "当前情绪：平静");
    let old_id = storage.add_fact(old.clone());
    let old_now = storage.get_fact_by_id(old_id).await.unwrap().unwrap();
    let fresh = make_test_fact("rama-0001", ProfileField::RecentContext, "当前情绪：焦虑");
    let fresh_id = storage.add_fact_with_version(&old_now, fresh);

    // 服务层用例读取版本链：链头最早在前（旧 → 新）
    let detail = engine
        .memory_fact_detail(fresh_id)
        .await
        .unwrap()
        .expect("事实应存在");
    let chain = detail.versions;
    assert_eq!(chain.len(), 2, "版本链应含旧新两版");
    assert_eq!(chain[0].id, old_id);
    assert_eq!(chain[1].id, fresh_id);
    assert_eq!(
        chain[1].version_of,
        Some(old_id),
        "新事实 version_of 应指向旧 id"
    );

    // show 单条（新事实 active）
    let f = detail.fact;
    assert_eq!(f.status, ramaria_core::types::FactStatus::Active);
}

/// 进程级：`fact list --json` 输出信封契约（空库返回空数组，stdout 仅一行 JSON）。
#[test]
fn fact_list_json_envelope_purity() {
    let out = run_cli(&["fact", "list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "fact list --json 应成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    assert_eq!(parsed["data"]["persona_uid"], "rama-0001");
    assert_eq!(parsed["data"]["total"], 0);
    assert!(parsed["data"]["facts"].is_array(), "facts 应为数组");
}

/// 进程级：`fact show <不存在>` → 业务校验失败，exit code 4 + 错误信封。
#[test]
fn fact_show_missing_is_validation_error() {
    let out = run_cli(&["fact", "show", "99999", "--json"]);
    assert_eq!(out.status.code(), Some(4), "不存在的事实应退出 code 4");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], 4);
    assert!(
        parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("99999"),
        "错误信息应包含事实 id"
    );
}

/// 进程级：**无 delete 子命令断言**（双端不做事实删除）。
#[test]
fn fact_no_delete_subcommand() {
    let out = run_cli(&["fact", "delete"]);
    // clap 参数错 → exit code 2；错误信息应提示 unknown subcommand
    assert_eq!(out.status.code(), Some(2), "fact delete 应为参数错误");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unrecognized subcommand") || stderr.contains("无"),
        "应提示 delete 不存在，实际: {stderr}"
    );
    // fact --help 只含 list/show/help，不含 delete
    let help = run_cli(&["fact", "--help"]);
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("list"), "help 应含 list");
    assert!(help_text.contains("show"), "help 应含 show");
    assert!(
        !help_text.contains("delete"),
        "help 不应含 delete 子命令（双端不做事实删除）"
    );
}

// =========================================================
// 导入解析报告隐私契约（进程级）
// =========================================================

/// 导入解析报告：默认掩码、--no-report 关闭、--quiet 抑制（CLI-01 契约）。
#[test]
fn import_report_masked_no_report_and_quiet() {
    // 最小 qq-chat-exporter v6.x JSON：chatInfo（含可识别昵称/QQ 号）+ 1 条 text 消息
    let json = r#"{
        "chatInfo": {
            "name": "对方昵称B",
            "type": "private",
            "selfUid": "u_self_001",
            "selfName": "导出者昵称A",
            "selfUin": "123456789",
            "peerUid": "u_peer_001",
            "peerUin": "987654321"
        },
        "messages": [
            {
                "id": "m1",
                "timestamp": 1700000000000,
                "type": "text",
                "content": { "text": "你好" }
            }
        ]
    }"#;
    let file_path = std::env::temp_dir().join(format!(
        "ramaria_cli_import_report_{}.json",
        std::process::id()
    ));
    std::fs::write(&file_path, json).expect("写入临时 QQ 导出文件失败");
    let file_arg = file_path.to_string_lossy().to_string();

    // 1) 默认：报告走掩码版（stderr 不含原值，含 mask_id 掩码形态）
    let out = run_cli(&["import", "qq", "--file", file_arg.as_str(), "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "dry-run 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("123456789"),
        "stderr 不应含导出者 QQ 号原值: {stderr}"
    );
    assert!(
        !stderr.contains("导出者昵称A"),
        "stderr 不应含导出者昵称原值: {stderr}"
    );
    assert!(stderr.contains("12…89"), "stderr 应含掩码 QQ 号: {stderr}");

    // 2) --no-report：完全关闭解析报告输出
    let out = run_cli(&[
        "import",
        "qq",
        "--file",
        file_arg.as_str(),
        "--dry-run",
        "--no-report",
    ]);
    assert_eq!(out.status.code(), Some(0), "--no-report 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("解析报告"),
        "--no-report 不应输出解析报告: {stderr}"
    );

    // 3) --quiet：全局抑制 stderr 提示（报告属提示类输出）
    let out = run_cli(&[
        "import",
        "qq",
        "--file",
        file_arg.as_str(),
        "--dry-run",
        "--quiet",
    ]);
    assert_eq!(out.status.code(), Some(0), "--quiet 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("解析报告"),
        "--quiet 应抑制解析报告: {stderr}"
    );

    let _ = std::fs::remove_file(&file_path);
}

// =========================================================
// JSON 信封契约（进程级）
// =========================================================
// 覆盖:
// - setup / chat 为交互式命令：--json 显式 unsupported（错误信封 + exit 4）
// - blocks rebuild / index rebuild / diagnostics：--json 成功信封 + stdout 纯净性

/// 交互式命令 setup 与 --json 不兼容：显式 unsupported（错误信封 + exit 4）。
#[test]
fn setup_json_is_explicitly_unsupported() {
    let out = run_cli(&["setup", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "setup --json 应显式失败（exit 4）"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], false, "信封 ok 应为 false");
    assert_eq!(parsed["error"]["code"], 4);
    assert!(
        parsed["error"]["message"].is_string(),
        "错误信封应含 message"
    );
}

/// 交互式命令 chat 与 --json 不兼容：显式 unsupported（错误信封 + exit 4）。
#[test]
fn chat_json_is_explicitly_unsupported() {
    let out = run_cli(&["chat", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "chat --json 应显式失败（exit 4）"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], false, "信封 ok 应为 false");
    assert_eq!(parsed["error"]["code"], 4);
}

/// blocks rebuild --json：成功信封（含 rebuilt 键），stdout 仅一行 JSON。
#[test]
fn blocks_rebuild_json_envelope() {
    let out = run_cli(&["blocks", "rebuild", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "blocks rebuild --json 应成功退出"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    assert!(parsed["data"].is_object(), "data 应为对象");
    assert!(
        parsed["data"]["rebuilt"].is_boolean(),
        "data.rebuilt 应存在且为布尔"
    );
}

/// index rebuild --json：成功信封（doc_count 为数字），stdout 仅一行 JSON。
#[test]
fn index_rebuild_json_envelope() {
    let out = run_cli(&["index", "rebuild", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "index rebuild --json 应成功退出"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    assert!(
        parsed["data"]["doc_count"].is_number(),
        "data.doc_count 应为数字"
    );
}

/// diagnostics --json：成功信封（file 指向真实产物），结束后清理临时文件。
#[test]
fn diagnostics_json_envelope() {
    let zip_path = std::env::temp_dir().join(format!("ramaria-diag-{}.zip", std::process::id()));
    let zip_arg = zip_path.to_string_lossy().to_string();
    let out = run_cli(&["diagnostics", "--json", "--output", zip_arg.as_str()]);
    assert_eq!(out.status.code(), Some(0), "diagnostics --json 应成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    let file = parsed["data"]["file"]
        .as_str()
        .expect("data.file 应为字符串");
    assert!(
        std::path::Path::new(file).exists(),
        "导出文件应存在: {file}"
    );
    // 清理临时产物（以报告中 canonical 化后的路径为准）
    let _ = std::fs::remove_file(file);
}
