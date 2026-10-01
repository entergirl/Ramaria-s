//! tests/export_tests.rs - CLI export 命令契约测试
//!
//! 覆盖:
//! - JSON / Markdown 写出内容与服务层渲染（入口共用实现）逐字段一致
//! - --redact 脱敏开关：消息正文替换为 <N chars>，其余结构不变
//! - 无可导出会话时 Markdown 提示且不写文件
//!
//! 安全约束:
//! - 使用 MockStorage + MockLlm，不访问真实数据 / OS keychain
//! - 文件写入仅限系统临时目录，测试结束后清理

mod common;

use common::{MockStorage, build_test_engine, make_assistant_message, make_user_message};
use ramaria_service::ExportDataRequest;
use std::sync::Arc;
use uuid::Uuid;

/// 构造含一个会话（2 条消息）的测试引擎。
fn engine_with_sessions() -> (Arc<ramaria_service::Engine>, Arc<MockStorage>) {
    let (engine, storage) = build_test_engine();
    let session_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("固定 UUID");
    storage.create_session_with_messages(
        session_id,
        vec![
            make_user_message(session_id, "你好"),
            make_assistant_message(session_id, "你好呀"),
        ],
    );
    (engine, storage)
}

/// 系统临时目录下的唯一测试文件路径。
fn temp_export_file(tag: &str, ext: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "ramaria_export_{tag}_{}_{stamp}.{ext}",
        std::process::id()
    ))
}

/// JSON 导出：写出文件与服务层渲染逐字段一致（仅 exported_at 为渲染时刻需归一）。
#[tokio::test]
async fn export_json_file_matches_service_render() {
    let (engine, _storage) = engine_with_sessions();
    let tmp_file = temp_export_file("parity", "json");

    ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
            redact: false,
            json: false,
        },
    )
    .await
    .expect("导出应成功");

    let mut written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tmp_file).expect("读取导出文件失败"))
            .expect("导出文件应为合法 JSON");
    let data = engine
        .export_sessions(ExportDataRequest::default())
        .await
        .expect("装配应成功");
    let mut expected: serde_json::Value =
        serde_json::from_str(&ramaria_service::render_sessions_json(&data, false))
            .expect("渲染结果应为合法 JSON");

    written["ramaria_export"]["exported_at"] = serde_json::json!("<normalized>");
    expected["ramaria_export"]["exported_at"] = serde_json::json!("<normalized>");
    assert_eq!(written, expected, "CLI 输出应与服务层渲染逐字段一致");
    assert_eq!(
        written["ramaria_export"]["version"],
        ramaria_service::EXPORT_FORMAT_VERSION
    );

    let _ = std::fs::remove_file(&tmp_file);
}

/// JSON 导出 --redact：正文替换为 `<N chars>`，结构与其余字段保持不变。
#[tokio::test]
async fn export_json_redact_hides_message_bodies() {
    let (engine, _storage) = engine_with_sessions();
    let tmp_file = temp_export_file("redacted", "json");

    ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
            redact: true,
            json: false,
        },
    )
    .await
    .expect("导出应成功");

    let content = std::fs::read_to_string(&tmp_file).expect("读取导出文件失败");
    assert!(
        content.contains("<2 chars>"),
        "正文应按字符数占位: {content}"
    );
    assert!(!content.contains("你好"), "脱敏后不得含原文: {content}");
    assert!(
        content.contains("\"role\": \"user\"") && content.contains("\"source\": \"local\""),
        "结构字段应保持: {content}"
    );

    let _ = std::fs::remove_file(&tmp_file);
}

/// Markdown 导出：写出文件与服务层渲染一致（导出时间行取渲染时刻，比对时剔除）。
#[tokio::test]
async fn export_markdown_file_matches_service_render() {
    let (engine, _storage) = engine_with_sessions();
    let tmp_file = temp_export_file("markdown", "md");

    ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
            redact: false,
            json: false,
        },
    )
    .await
    .expect("导出应成功");

    let written = std::fs::read_to_string(&tmp_file).expect("读取导出文件失败");
    let data = engine
        .export_sessions(ExportDataRequest::default())
        .await
        .expect("装配应成功");
    let expected = ramaria_service::render_sessions_markdown(&data, false).expect("应可导出");
    let strip_export_time = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("导出时间: "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        strip_export_time(&written),
        strip_export_time(&expected),
        "Markdown 输出应与服务层渲染一致"
    );
    assert!(
        written.contains("**👤 用户**") && written.contains("**🤖 AI**"),
        "角色标签应保持: {written}"
    );

    let _ = std::fs::remove_file(&tmp_file);
}

/// Markdown 导出：无可导出会话时正常返回且不写文件。
#[tokio::test]
async fn export_markdown_empty_writes_no_file() {
    let (engine, _storage) = build_test_engine();
    let tmp_file = temp_export_file("markdown-empty", "md");

    ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
            redact: false,
            json: false,
        },
    )
    .await
    .expect("空数据应为正常返回");
    assert!(!tmp_file.exists(), "无可导出会话时不应写文件");
}
