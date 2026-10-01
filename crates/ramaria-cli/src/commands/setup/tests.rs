//! crates/ramaria-cli/src/commands/setup/tests.rs - setup 旧人格文件回退单元测试
//!
//! 设计特点:
//! - 覆盖旧单文件路径（`config/persona.toml` → `rama-0001`）的导入结果：新建 / 跳过 / 失败
//! - 断言落库字段与文件等价：`config` 为文件全文、名称取 `assistant_name`、缺失回退 Ramaria
//! - 已存在记录不覆盖（名称 / 配置 / updated_at 均不变）
//! - 真实 SQLite 临时库落库后按 uid 读回逐项断言，测试间以唯一目录隔离
//!
//! 安全约束:
//! - 真实 SQLite 临时库（自动 migration）+ 默认装配，不调用真实 LLM、不连网、不触碰 keychain

use super::*;
use ramaria_core::types::{Persona, PersonaKind};
use ramaria_service::EngineOptions;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// 临时目录序号（并行测试线程安全：纳秒可能撞车，追加原子计数保证唯一）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建唯一临时测试目录（库文件位于其中，独立于仓库目录）。
fn temp_test_dir(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-setup-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 构造真实 SQLite 临时库的引擎（默认装配，不连网）。
async fn setup_engine(tag: &str) -> (Arc<Engine>, PathBuf) {
    let dir = temp_test_dir(tag);
    let engine = Engine::open_with(EngineOptions::new(dir.join("persona.db")))
        .await
        .expect("装配测试引擎应成功");
    (Arc::new(engine), dir)
}

/// 释放连接池并清理临时目录（Windows 下需先释放文件句柄）。
async fn cleanup(engine: &Arc<Engine>, dir: PathBuf) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 旧文件存在且库无 rama-0001：新建人格，字段与文件等价。
#[tokio::test]
async fn legacy_file_creates_persona_from_file_content() {
    let (engine, dir) = setup_engine("legacy-create").await;
    let legacy_path = dir.join("persona.toml");
    let content = "assistant_name = \"黎杋枫\"\n[blocks]\nA_persona = \"\"\"\n测试正文\n\"\"\"\n";
    std::fs::write(&legacy_path, content).expect("写入旧人格文件应成功");

    let handled = load_legacy_persona_file(&engine, &legacy_path).await;
    assert!(handled, "导入成功应视为已处理（不再提示未找到人格文件）");

    let persona = engine
        .storage()
        .get_persona_by_uid("rama-0001")
        .await
        .expect("查询人格应成功")
        .expect("库内应出现 rama-0001");
    assert_eq!(persona.name, "黎杋枫", "名称应取文件 assistant_name");
    assert_eq!(
        persona.config.as_deref(),
        Some(content),
        "config 应为文件全文"
    );
    assert_eq!(persona.kind, PersonaKind::Rama, "uid 决定人格类型");
    assert_eq!(persona.source, "file");

    cleanup(&engine, dir).await;
}

/// 已存在 rama-0001：仅创建缺失模式跳过，不覆盖名称 / 配置 / 更新时间。
#[tokio::test]
async fn legacy_file_skips_existing_persona_without_overwrite() {
    let (engine, dir) = setup_engine("legacy-skip").await;
    let mut persona = Persona::new(
        "rama-0001".to_string(),
        "旧名称".to_string(),
        PersonaKind::Rama,
        1,
        "file".to_string(),
    );
    persona.config = Some("assistant_name = \"旧名称\"\n".to_string());
    engine
        .storage()
        .create_persona(&persona)
        .await
        .expect("预置人格应成功");
    let before = engine
        .storage()
        .get_persona_by_uid("rama-0001")
        .await
        .expect("查询人格应成功")
        .expect("预置记录应存在");

    let legacy_path = dir.join("persona.toml");
    std::fs::write(&legacy_path, "assistant_name = \"新名称\"\n").expect("写入旧人格文件应成功");
    let handled = load_legacy_persona_file(&engine, &legacy_path).await;
    assert!(handled, "已存在跳过应视为已处理");

    let after = engine
        .storage()
        .get_persona_by_uid("rama-0001")
        .await
        .expect("查询人格应成功")
        .expect("记录应存在");
    assert_eq!(after.name, "旧名称", "已存在记录名称不应被改写");
    assert_eq!(after.config, before.config, "已存在记录配置不应被改写");
    assert_eq!(after.updated_at, before.updated_at, "updated_at 不应刷新");

    cleanup(&engine, dir).await;
}

/// 缺失 assistant_name：回退 Ramaria；文件不可读：失败条目不阻塞（返回 false）。
#[tokio::test]
async fn legacy_file_name_fallback_and_failure_tolerated() {
    let (engine, dir) = setup_engine("legacy-fallback").await;
    let legacy_path = dir.join("persona.toml");
    let content = "[blocks]\nA_persona = \"\"\"\n无名称\n\"\"\"\n";
    std::fs::write(&legacy_path, content).expect("写入旧人格文件应成功");

    let handled = load_legacy_persona_file(&engine, &legacy_path).await;
    assert!(handled);
    let persona = engine
        .storage()
        .get_persona_by_uid("rama-0001")
        .await
        .expect("查询人格应成功")
        .expect("应已创建");
    assert_eq!(
        persona.name, "Ramaria",
        "缺失 assistant_name 应回退 Ramaria"
    );
    assert_eq!(persona.config.as_deref(), Some(content));

    // 文件不可读：失败条目，返回 false（向导继续，不阻塞）
    let handled = load_legacy_persona_file(&engine, &dir.join("missing.toml")).await;
    assert!(!handled, "读取失败应按未命中处理");

    cleanup(&engine, dir).await;
}
