//! tests/memory_l1_tests.rs - CLI `memory l1` 时间口径测试（进程级契约）
//!
//! 覆盖:
//! - `--json` 条目含 `occurred_at`（发生时间，读取已回退）与 `created_at`（入库时间）双键
//! - 文本"时间"行取发生时间（用户看到的浏览时间 = 事情发生时刻）
//!
//! 安全约束:
//! - 使用临时文件库（预写合成数据 + 真实二进制打开），不调用真实 LLM、不连网、不触碰 keychain

mod common;

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{MemoryL1, Persona, PersonaKind};
use ramaria_storage::SqliteStorage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// 临时库序号（并行测试线程安全）。
static DB_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建唯一临时库路径（库文件位于独立目录内）。
fn temp_db(tag: &str) -> PathBuf {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("ramaria_cli_l1_{tag}_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir.join("l1.db")
}

/// 预写一条 L1：入库时间与发生时间背离（发生 22:13:20、入库 22:15:00 UTC）。
async fn seed_l1(db: &Path) {
    let pool = ramaria_storage::database::init_pool(Some(db.to_path_buf()))
        .await
        .expect("初始化测试数据库失败");
    let storage = SqliteStorage::new(pool.clone());
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
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let mut l1 = MemoryL1::new(
        session.id,
        "导入的历史摘要".to_string(),
        Some("下午".to_string()),
    );
    l1.persona_uid = Some("char-0001".to_string());
    l1.created_at = 1_700_000_100_000; // 2023-11-14T22:15:00Z（入库）
    l1.occurred_at = Some(1_700_000_000_000); // 2023-11-14T22:13:20Z（发生）
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    pool.close().await;
}

/// 以真实二进制运行 CLI（库由调用方预置）。
fn run_cli(args: &[&str], db: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(db)
        .output()
        .expect("运行 ramaria 二进制失败")
}

/// `--json` 条目：occurred_at 为发生时间 ISO、created_at 保留原键原值。
#[tokio::test]
async fn memory_l1_json_contains_occurred_at_key() {
    let db = temp_db("json");
    seed_l1(&db).await;

    let out = run_cli(&["memory", "--json", "--persona", "char-0001"], &db);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"occurred_at\":\"2023-11-14T22:13:20Z\""),
        "JSON 应含发生时间键（ISO-8601 UTC）: {stdout}"
    );
    assert!(
        stdout.contains("\"created_at\":\"2023-11-14T22:15:00Z\""),
        "created_at 原键原值应保留（入库时间）: {stdout}"
    );

    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}

/// 文本"时间"行取发生时间（不显示入库时间）。
#[tokio::test]
async fn memory_l1_text_time_line_uses_occurred_at() {
    let db = temp_db("text");
    seed_l1(&db).await;

    let out = run_cli(&["memory", "--persona", "char-0001"], &db);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("2023-11-14 22:13"),
        "文本时间行应取发生时间: {stdout}"
    );
    assert!(
        !stdout.contains("2023-11-14 22:15"),
        "文本不应显示入库时间: {stdout}"
    );

    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}
