//! tests/sticker_tests.rs - CLI `sticker` 子命令契约测试
//!
//! 覆盖:
//! - add：写入资产（信封含 id / 文件名 / 标签 / 描述），资产文件落盘到 stickers/ 目录
//! - list：读回资产数组（新增后含该资产、删除后为空）
//! - update：描述空串清空、标签出现即整体覆盖；未知 id 零改写
//! - remove：删除成功（含资产文件删除）与未知 id 零删除
//! - --json 与文本两形态；非图片文件校验错误退出码
//!
//! 安全约束:
//! - 进程级用例使用临时库（--db 指向临时目录），不触碰用户数据
//! - 不调用真实 LLM 与嵌入模型

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// 进程级临时库序号（并行测试线程安全）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 最小合法 PNG（1×1，含正确 CRC）。
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0x7A, 0x5E, 0xAB, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
    0xAE, 0x42, 0x60, 0x82,
];

// =========================================================
// 共享夹具与工具
// =========================================================

/// 创建唯一临时测试目录（库与图片文件位于其中）。
fn temp_test_dir(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-sticker-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 在临时目录写入 1×1 PNG（返回文件路径）。
fn write_png(dir: &Path, file_name: &str) -> PathBuf {
    let path = dir.join(file_name);
    std::fs::write(&path, PNG_1X1).expect("写入 PNG 文件应成功");
    path
}

/// 以临时库运行 CLI 进程（库文件固定为目录内 `sticker.db`）。
fn run_cli(db_dir: &Path, args: &[&str]) -> std::process::Output {
    let db = db_dir.join("sticker.db");
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

/// `sticker list --json` 的资产条目数组。
fn list_assets(dir: &Path) -> Vec<serde_json::Value> {
    let out = run_cli(dir, &["sticker", "list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "sticker list 应成功退出");
    let envelope = parse_envelope(&out);
    envelope["data"]
        .as_array()
        .expect("data 应为资产数组")
        .clone()
}

// =========================================================
// 增删改查全链路（--json）
// =========================================================

/// add → list → update（清空描述 / 覆盖标签）→ remove → list 为空。
#[test]
fn add_list_update_remove_json_cycle() {
    let dir = temp_test_dir("cycle");
    let png = write_png(&dir, "smile.png");
    let file = png.to_str().expect("图片路径应为合法 UTF-8");

    // add：信封返回资产（id / 文件名 / 标签 / 描述）
    let out = run_cli(
        &dir,
        &[
            "sticker",
            "add",
            file,
            "--description",
            "开心时使用",
            "--tag",
            "开心",
            "--tag",
            "摸鱼",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "add 应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    let id = data["id"].as_i64().expect("id 应为数字");
    assert!(id > 0, "新增资产 id 应为正数");
    assert_eq!(data["file_name"], "smile.png");
    assert_eq!(data["description"], "开心时使用");
    assert_eq!(data["tags"], serde_json::json!(["开心", "摸鱼"]));

    // 资产文件应落盘到 stickers/ 目录（file_path 为 md5 + 扩展名）
    let file_path = data["file_path"].as_str().expect("file_path 应为字符串");
    let asset_path = dir.join("stickers").join(file_path);
    assert!(
        asset_path.is_file(),
        "资产文件应存在于: {}",
        asset_path.display()
    );

    // list：应含该资产
    let assets = list_assets(&dir);
    assert_eq!(assets.len(), 1, "list 应含一条资产: {assets:?}");
    assert_eq!(assets[0]["id"], id);

    // update：描述空串清空 + 标签整体覆盖
    let out = run_cli(
        &dir,
        &[
            "sticker",
            "update",
            &id.to_string(),
            "--description",
            "",
            "--tag",
            "无奈",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "update 应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["data"]["id"], id);
    assert_eq!(envelope["data"]["updated"], true);

    let assets = list_assets(&dir);
    assert_eq!(assets.len(), 1);
    assert_eq!(
        assets[0]["description"],
        serde_json::Value::Null,
        "空串应清空描述"
    );
    assert_eq!(
        assets[0]["tags"],
        serde_json::json!(["无奈"]),
        "标签应整体覆盖"
    );

    // remove：删除成功且资产文件被清理
    let out = run_cli(&dir, &["sticker", "remove", &id.to_string(), "--json"]);
    assert_eq!(out.status.code(), Some(0), "remove 应成功退出");
    let envelope = parse_envelope(&out);
    assert_eq!(envelope["data"]["id"], id);
    assert_eq!(envelope["data"]["removed"], true);
    assert!(!asset_path.exists(), "删除后资产文件应被清理");

    // list：删除后为空
    let assets = list_assets(&dir);
    assert!(assets.is_empty(), "删除后 list 应为空: {assets:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 未给 --description 时保持现值（仅覆盖标签）。
#[test]
fn update_without_description_keeps_value() {
    let dir = temp_test_dir("keep-desc");
    let png = write_png(&dir, "ok.png");
    let file = png.to_str().expect("图片路径应为合法 UTF-8");

    let out = run_cli(
        &dir,
        &["sticker", "add", file, "--description", "原描述", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "add 应成功退出");
    let id = parse_envelope(&out)["data"]["id"]
        .as_i64()
        .expect("id 应为数字");

    let out = run_cli(
        &dir,
        &[
            "sticker",
            "update",
            &id.to_string(),
            "--tag",
            "新标签",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "update 应成功退出");
    assert_eq!(parse_envelope(&out)["data"]["updated"], true);

    let assets = list_assets(&dir);
    assert_eq!(assets[0]["description"], "原描述", "缺省描述应保持现值");
    assert_eq!(assets[0]["tags"], serde_json::json!(["新标签"]));

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 文本形态
// =========================================================

/// 文本形态：add 摘要走 stderr、list 逐行走 stdout；未知 id 零改写 / 零删除提示。
#[test]
fn text_forms_and_unknown_id_noop() {
    let dir = temp_test_dir("text");
    let png = write_png(&dir, "smile.png");
    let file = png.to_str().expect("图片路径应为合法 UTF-8");

    let out = run_cli(&dir, &["sticker", "add", file, "--tag", "开心"]);
    assert_eq!(out.status.code(), Some(0), "文本 add 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("已新增表情资产"),
        "add 摘要应输出到 stderr: {stderr}"
    );
    assert!(stderr.contains("smile.png"), "摘要应含文件名: {stderr}");

    let out = run_cli(&dir, &["sticker", "list"]);
    assert_eq!(out.status.code(), Some(0), "文本 list 应成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("smile.png"), "list 应含文件名: {stdout}");
    assert!(stdout.contains("开心"), "list 应含标签: {stdout}");

    // 未知 id：update 零改写
    let out = run_cli(&dir, &["sticker", "update", "9999", "--tag", "x"]);
    assert_eq!(out.status.code(), Some(0), "未知 id update 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("未找到匹配的表情资产"),
        "未知 id 应输出未找到提示: {stderr}"
    );

    // 未知 id：remove 零删除
    let out = run_cli(&dir, &["sticker", "remove", "9999"]);
    assert_eq!(out.status.code(), Some(0), "未知 id remove 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("未找到匹配的表情资产"),
        "未知 id 应输出未找到提示: {stderr}"
    );

    // 空库文本 list：提示暂无
    let empty_dir = temp_test_dir("empty");
    let out = run_cli(&empty_dir, &["sticker", "list"]);
    assert_eq!(out.status.code(), Some(0), "空库 list 应成功退出");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("暂无表情资产"), "空库应输出提示: {stderr}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty_dir);
}

// =========================================================
// 错误路径
// =========================================================

/// 非图片文件与缺失文件：非零退出且 stderr 非空（校验错误退出码 4）。
#[test]
fn add_rejects_invalid_inputs() {
    let dir = temp_test_dir("invalid");

    // 非图片字节：服务层魔数识别拒绝 → 校验错误退出码 4
    let text = dir.join("note.txt");
    std::fs::write(&text, b"not an image").expect("写入文本文件应成功");
    let text = text.to_str().expect("路径应为合法 UTF-8");
    let out = run_cli(&dir, &["sticker", "add", text]);
    assert_eq!(out.status.code(), Some(4), "非图片应退出 code 4");
    assert!(!out.stderr.is_empty(), "错误应输出到 stderr");

    // 文件不存在：读取失败，非零退出
    let missing = dir.join("missing.png");
    let missing = missing.to_str().expect("路径应为合法 UTF-8");
    let out = run_cli(&dir, &["sticker", "add", missing]);
    assert_eq!(out.status.code(), Some(1), "读取失败应退出 code 1");
    assert!(!out.stderr.is_empty(), "错误应输出到 stderr");

    let _ = std::fs::remove_dir_all(&dir);
}
