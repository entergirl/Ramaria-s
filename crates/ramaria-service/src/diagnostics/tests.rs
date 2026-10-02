//! crates/ramaria-service/src/diagnostics/tests.rs - 诊断导出用例单元测试
//!
//! 设计特点:
//! - 由 diagnostics 模块以 `#[cfg(test)] mod tests;` 收纳：覆盖 API key 脱敏 /
//!   导出前二次脱敏（消息字段与绝对路径）/ 收集失败占位 / zip 原子打包 / 端到端导出五组路径
//! - 端到端用例经真实 SQLite 临时库与 mock LLM 走完整导出（读取 zip 断言内容）
//!
//! 安全约束:
//! - 全部数据为合成样例；断言"脱敏后不含原文与绝对路径"，不写入真实密钥。

use super::collect::{SystemInfo, collect_config, collect_logs, collect_system_info};
use super::redact::{redact_api_keys, redact_for_export};
use super::render::{build_system_txt, build_zip};
use super::*;
use ramaria_core::config::RamariaConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── API key 脱敏 ──

#[test]
fn test_redact_api_key_cases() {
    // 单行脱敏、大小写不敏感、注释与无关键保持原样
    let cases = [
        ("api_key = \"sk-abc123def456\"", "api_key = \"[REDACTED]\""),
        ("API_KEY = \"secret\"", "API_KEY = \"[REDACTED]\""),
        (
            "# api_key = \"this is a comment\"",
            "# api_key = \"this is a comment\"",
        ),
        (
            "base_url = \"https://api.example.com\"",
            "base_url = \"https://api.example.com\"",
        ),
        ("// api_key = \"value\"", "// api_key = \"value\""),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_api_keys(input), expected, "input={input:?}");
    }
    // 多行: 保留 base_url/model_id、api_key 脱敏、不含原密文
    let input =
        "base_url = \"https://api.example.com\"\napi_key = \"my-secret\"\nmodel_id = \"gpt-4\"";
    let result = redact_api_keys(input);
    assert!(result.contains("base_url"));
    assert!(result.contains("[REDACTED]"));
    assert!(result.contains("model_id"));
    assert!(!result.contains("my-secret"));
}

// ── 导出前二次脱敏 ──

/// 消息类字段 → 字符数占位；长度类字段不受影响。
#[test]
fn redact_for_export_replaces_message_fields() {
    let cases = [
        (r#"preview="我想吃火锅""#, r#"preview="<5 chars>""#),
        ("msg=hello", "msg=<5 chars>"),
        ("user_input=\"hi\"", "user_input=\"<2 chars>\""),
        // 长度字段本身不含原文，保持可诊断性
        ("msg_len=12", "msg_len=12"),
        ("content_len=3", "content_len=3"),
        // 非敏感字段保持原样
        ("dropped=Some(2)", "dropped=Some(2)"),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_for_export(input), expected, "input={input}");
    }
}

/// 绝对路径 → 文件名；URL 与非路径文本不受影响；多语言内容不 panic。
#[test]
fn redact_for_export_replaces_absolute_paths() {
    let cases = [
        (
            r"path=C:\Users\someone\Documents\ramaria.log",
            "path=ramaria.log",
        ),
        (
            "无法读取 /home/someone/private/ramaria.log",
            "无法读取 ramaria.log",
        ),
        (r#""C:\Users\someone\AppData\Roaming""#, r#""Roaming""#),
        ("dir=/tmp/", "dir=tmp"),
        // URL 中的路径段不是本地路径，保持原样
        (
            "url=https://api.github.com/repos/entergirl/Ramaria-s",
            "url=https://api.github.com/repos/entergirl/Ramaria-s",
        ),
        // 中文与路径混排不 panic、不切坏多字节字符
        (
            "导入到 C:\\用户\\文档\\导出.zip 完成",
            "导入到 导出.zip 完成",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_for_export(input), expected, "input={input}");
    }
}

/// 行结构与空白保留（脱敏不破坏日志可读性）。
#[test]
fn redact_for_export_keeps_line_structure() {
    let input = "INFO x: a  preview=\"秘密内容\"\n\nWARN y: b\n";
    let out = redact_for_export(input);
    assert_eq!(out.lines().count(), 3, "行数应保持");
    assert!(out.contains("INFO x: a  preview=\"<4 chars>\""));
    assert!(out.ends_with('\n'), "末尾换行应保留");
}

// ── system.txt 构建 ──

#[test]
fn test_build_system_txt_contains_all_fields() {
    let info = SystemInfo {
        os: "windows".into(),
        arch: "x86_64".into(),
        family: "windows".into(),
        app_version: "2.0.0".into(),
        schema_version: "1".into(),
        collected_at: "2026-06-15T12:00:00Z".into(),
    };

    let content = build_system_txt(&info);

    assert!(content.contains("os = windows"));
    assert!(content.contains("arch = x86_64"));
    assert!(content.contains("app_version = 2.0.0"));
    assert!(content.contains("schema_version = 1"));
    assert!(content.contains("2026-06-15T12:00:00Z"));
}

// ── 系统信息收集 ──

#[test]
fn test_collect_system_info() {
    let info = collect_system_info("42");

    assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.schema_version, "42");
    assert!(!info.collected_at.is_empty());
    // 验证 OS 字段非空
    assert!(!info.os.is_empty());
    assert!(!info.arch.is_empty());
}

// ── 日志收集: 空目录 ──

#[test]
fn test_collect_logs_empty_dir() {
    let mut config = RamariaConfig::default();
    config.paths.log_dir = String::new();
    let mut status = HashMap::new();

    let result = collect_logs(&config, &mut status);

    assert!(result.contains("未配置"));
    assert!(status.contains_key("logs"));
}

// ── 配置收集: 空目录 ──

#[test]
fn test_collect_config_empty_dir() {
    let mut config = RamariaConfig::default();
    config.paths.config_dir = String::new();
    let mut status = HashMap::new();

    let result = collect_config(&config, &mut status);

    assert!(result.contains("未配置"));
    assert!(status.contains_key("config"));
}

// ── 收集失败分支: 占位文本只写文件名 ──

/// 读取失败（目标同名目录占位）→ 占位文本只含文件名，不把本机绝对路径写进诊断包。
#[test]
fn collect_failure_placeholder_keeps_file_name_only() {
    let dir = unique_dir("collect-failure");
    let log_dir = dir.join("logs");
    let config_dir = dir.join("cfg");
    // 同名目录占位 → read_to_string 失败（非 NotFound 分支）
    std::fs::create_dir_all(log_dir.join("ramaria.log")).expect("创建同名目录应成功");
    std::fs::create_dir_all(config_dir.join("config.toml")).expect("创建同名目录应成功");

    let mut config = RamariaConfig::default();
    config.paths.log_dir = log_dir.to_string_lossy().into_owned();
    config.paths.config_dir = config_dir.to_string_lossy().into_owned();
    let mut status = HashMap::new();

    let logs = collect_logs(&config, &mut status);
    let config_text = collect_config(&config, &mut status);

    let dir_label = dir.to_string_lossy().into_owned();
    assert!(
        !logs.contains(&dir_label) && !config_text.contains(&dir_label),
        "占位文本不应含绝对路径: logs={logs} config={config_text}"
    );
    assert!(logs.contains("ramaria.log"), "应保留文件名便于定位: {logs}");
    assert!(
        config_text.contains("config.toml"),
        "应保留文件名便于定位: {config_text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── zip 打包: 原子写入 ──

fn sample_info() -> SystemInfo {
    SystemInfo {
        os: "windows".into(),
        arch: "x86_64".into(),
        family: "windows".into(),
        app_version: "test".into(),
        schema_version: "1".into(),
        collected_at: "2026-06-15T12:00:00Z".into(),
    }
}

/// 在系统临时目录下创建唯一的测试子目录，返回其绝对路径。
fn unique_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("ramaria_diag_{tag}_{}_{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建测试临时目录失败");
    dir
}

/// 装配诊断用例所需的引擎（临时库 + mock LLM；配置快照承载导出路径字段）。
async fn engine_for_diagnostics(dir: &Path, config: RamariaConfig) -> crate::engine::Engine {
    let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
        .await
        .expect("测试库初始化应成功");
    let storage: std::sync::Arc<dyn ramaria_core::traits::StorageBackend> =
        std::sync::Arc::new(ramaria_storage::SqliteStorage::new(pool));
    crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::test_support::MockLlm::local()),
        None,
        config,
    )
}

/// 读取 zip 中指定条目的文本内容（测试辅助）。
fn read_zip_entry(zip_path: &Path, name: &str) -> String {
    use std::io::Read;

    let file = std::fs::File::open(zip_path).expect("无法打开生成的 zip");
    let mut archive = zip::ZipArchive::new(file).expect("生成的文件不是合法 zip");
    let mut entry = archive.by_name(name).expect("归档缺少目标条目");
    let mut text = String::new();
    entry.read_to_string(&mut text).expect("读取归档条目失败");
    text
}

/// 端到端：导出包不含绝对路径与消息原文（二次脱敏后落盘）。
#[tokio::test]
async fn export_redacts_absolute_paths_and_message_previews() {
    let dir = unique_dir("redact");
    let log_dir = dir.join("logs");
    let config_dir = dir.join("cfg");
    std::fs::create_dir_all(&log_dir).expect("创建日志目录失败");
    std::fs::create_dir_all(&config_dir).expect("创建配置目录失败");

    // 日志：本机绝对路径（Windows + Unix）与结构化消息字段（含原文）
    std::fs::write(
        log_dir.join("ramaria.log"),
        "INFO ramaria_service::chat: 收到消息 preview=\"我想吃火锅\" path=C:\\Users\\someone\\Documents\\ramaria.log\n\
         WARN ramaria_service::io: 无法读取 /home/someone/private/ramaria.log\n",
    )
    .expect("写入日志失败");
    // 配置：本机路径 + API key（TOML 中以 \\ 转义反斜杠）
    std::fs::write(
        config_dir.join("config.toml"),
        "[paths]\ndata_dir = \"C:\\\\Users\\\\someone\\\\AppData\\\\Roaming\\\\Ramaria\"\napi_key = \"sk-secret\"\n",
    )
    .expect("写入配置失败");

    let mut config = RamariaConfig::default();
    config.paths.log_dir = log_dir.to_string_lossy().into_owned();
    config.paths.config_dir = config_dir.to_string_lossy().into_owned();

    let zip_path = dir.join("diag.zip");
    // 经服务层引擎调用导出用例（配置快照承载日志 / 配置目录字段）
    let engine = engine_for_diagnostics(&dir, config).await;
    let report = engine
        .export_diagnostics(DiagnosticsRequest {
            output_path: zip_path.clone(),
            schema_version: "1".to_string(),
        })
        .await
        .expect("导出诊断包应成功");
    assert!(report.file_size_bytes > 0);

    let logs = read_zip_entry(&zip_path, "ramaria.log");
    assert!(
        !logs.contains("C:\\Users"),
        "日志不得含 Windows 绝对路径: {logs}"
    );
    assert!(
        !logs.contains("/home/someone"),
        "日志不得含 Unix 绝对路径: {logs}"
    );
    assert!(logs.contains("ramaria.log"), "应保留文件名便于定位: {logs}");
    assert!(
        logs.contains("preview=\"<5 chars>\""),
        "消息预览应替换为字符数: {logs}"
    );
    assert!(!logs.contains("我想吃火锅"), "日志不得含消息原文");

    let cfg = read_zip_entry(&zip_path, "config.toml");
    assert!(!cfg.contains("Users"), "配置不得含绝对路径: {cfg}");
    assert!(
        cfg.contains("data_dir = \"Ramaria\""),
        "路径应只保留最后一段: {cfg}"
    );
    assert!(cfg.contains("[REDACTED]"), "API key 仍应脱敏: {cfg}");
    assert!(
        cfg.contains("# 配置文件: config.toml"),
        "包头只写文件名: {cfg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 检索索引构建状态进入诊断收集：未构建 / 失败（脱敏原因）/ 已构建三态。
#[tokio::test]
async fn export_reports_index_build_status() {
    let dir = unique_dir("index-status");
    let mut config = RamariaConfig::default();
    config.paths.log_dir = dir.join("logs").to_string_lossy().into_owned();
    config.paths.config_dir = dir.join("cfg").to_string_lossy().into_owned();

    // 组装带失败注入的引擎（真实 SQLite + 可失败存储包装）
    let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
        .await
        .expect("测试库初始化应成功");
    let inner = std::sync::Arc::new(ramaria_storage::SqliteStorage::new(pool));
    let failable = std::sync::Arc::new(crate::test_support::FailableStorage::new(inner));
    let engine = crate::engine::Engine::from_parts(
        std::sync::Arc::clone(&failable)
            as std::sync::Arc<dyn ramaria_core::traits::StorageBackend>,
        std::sync::Arc::new(crate::test_support::MockLlm::local()),
        None,
        config,
    );

    // 1) 未构建 → not_built
    let report = engine
        .export_diagnostics(DiagnosticsRequest {
            output_path: dir.join("diag-1.zip"),
            schema_version: "1".to_string(),
        })
        .await
        .expect("导出诊断包应成功");
    assert_eq!(
        report
            .collection_status
            .get("index_build")
            .map(String::as_str),
        Some("not_built"),
        "未构建时收集状态应为 not_built: {:?}",
        report.collection_status
    );

    // 2) 构建失败 → failed: <脱敏原因>（保留可诊断信息、折叠为单行）
    failable.set_fail_list_personas(true);
    engine
        .rebuild_index()
        .await
        .expect_err("失败注入下重建应报错");
    let report = engine
        .export_diagnostics(DiagnosticsRequest {
            output_path: dir.join("diag-2.zip"),
            schema_version: "1".to_string(),
        })
        .await
        .expect("导出诊断包应成功");
    let failed = report
        .collection_status
        .get("index_build")
        .cloned()
        .unwrap_or_default();
    assert!(failed.starts_with("failed: "), "失败态前缀: {failed}");
    assert!(
        failed.contains("list_personas"),
        "应保留可诊断信息: {failed}"
    );
    assert!(!failed.contains('\n'), "失败原因应折叠为单行: {failed}");

    // 3) 恢复后成功构建 → ok（失败记录清除）
    failable.set_fail_list_personas(false);
    engine.rebuild_index().await.expect("恢复后重建应成功");
    assert!(
        engine.index_build_failure().is_none(),
        "构建成功后失败记录应清除"
    );
    let report = engine
        .export_diagnostics(DiagnosticsRequest {
            output_path: dir.join("diag-3.zip"),
            schema_version: "1".to_string(),
        })
        .await
        .expect("导出诊断包应成功");
    assert_eq!(
        report
            .collection_status
            .get("index_build")
            .map(String::as_str),
        Some("ok"),
        "成功构建后收集状态应为 ok: {:?}",
        report.collection_status
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// 用 zip crate 读取并断言归档包含三份诊断文件。
fn assert_archive_has_three_files(zip_path: &Path) {
    let file = std::fs::File::open(zip_path).expect("无法打开生成的 zip");
    let mut archive = zip::ZipArchive::new(file).expect("生成的文件不是合法 zip");
    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .expect("读取归档条目失败")
                .name()
                .to_string()
        })
        .collect();
    for expected in ["system.txt", "ramaria.log", "config.toml"] {
        assert!(
            names.iter().any(|n| n == expected),
            "归档缺少条目 {expected}，实际: {names:?}"
        );
    }
}

// build_zip 生成有效归档：返回大小 > 0、目标存在、同目录无 .part 残留。
#[test]
fn build_zip_writes_valid_archive_and_cleans_temp() {
    let dir = unique_dir("valid");
    let target = dir.join("report.zip");
    let info = sample_info();

    let bytes =
        build_zip(&target, &info, "log line 1\n", "[config]\nkey=1").expect("build_zip 应成功");

    assert!(bytes > 0, "返回字节数应为正，得到 {bytes}");
    assert_eq!(
        std::fs::metadata(&target).expect("目标 zip 应存在").len(),
        bytes,
        "目标文件大小应与返回字节数一致"
    );
    assert!(
        !dir.join("report.zip.part").exists(),
        "成功后不应残留 .part 临时文件"
    );
    assert_archive_has_three_files(&target);

    let _ = std::fs::remove_dir_all(&dir);
}

// 目标已存在旧内容时，build_zip 用完整 zip 原子替换（不残留半成品或 temp）。
#[test]
fn build_zip_atomically_replaces_existing_target() {
    let dir = unique_dir("replace");
    let target = dir.join("report.zip");
    // 预写一个旧内容文件（非 zip）
    std::fs::write(&target, b"old stale content").expect("预写旧内容失败");
    let info = sample_info();

    let bytes =
        build_zip(&target, &info, "fresh log\n", "[config]\nfresh=1").expect("build_zip 应成功");

    assert!(bytes > 0);
    let size = std::fs::metadata(&target).expect("替换后目标应存在").len();
    assert!(
        size != b"old stale content".len() as u64,
        "目标应被新 zip 替换而非保留旧内容长度"
    );
    assert!(
        !dir.join("report.zip.part").exists(),
        "替换后不应残留 .part 临时文件"
    );
    // 目标是合法 zip 且含三文件，证明旧内容已被完整覆盖
    assert_archive_has_three_files(&target);

    let _ = std::fs::remove_dir_all(&dir);
}

// 失败路径（目标为已存在目录 → rename 失败）：返回 Err、无 .part 残留、
// 原目标（目录）保持不被半文件污染。锁定的是"失败不留半文件/temp"分支。
#[test]
fn build_zip_failure_leaves_no_partial_target() {
    let dir = unique_dir("failure");
    // 目标位置放一个目录：write 阶段写 .part 成功，rename 到该目录失败
    let target = dir.join("report.zip");
    std::fs::create_dir(&target).expect("创建目标目录失败");
    let info = sample_info();

    let err = build_zip(&target, &info, "log\n", "cfg\n").expect_err("目标为目录时应失败");

    assert!(!err.is_empty(), "错误信息不应为空");
    assert!(
        !dir.join("report.zip.part").exists(),
        "失败后不应残留 .part 临时文件"
    );
    assert!(
        target.is_dir(),
        "原目标目录应保持不变（未被半成品 zip 覆盖）"
    );
    // 目录内不应被写入任何文件
    let entries: Vec<_> = std::fs::read_dir(&target)
        .expect("读取目标目录失败")
        .collect();
    assert!(entries.is_empty(), "失败不应在目录内留下写入内容");

    let _ = std::fs::remove_dir_all(&dir);
}
