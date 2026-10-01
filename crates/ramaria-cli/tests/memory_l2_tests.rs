//! tests/memory_l2_tests.rs - CLI `memory l2` 分页与时间列口径测试
//!
//! 覆盖:
//! - 分页口径：服务层视图按 `start` 倒序切片，与"全量读回后 skip / take"逐项一致
//! - 时间口径：视图透出 `start` / `end`（Unix 毫秒）；`--json` 条目转为 ISO-8601 UTC
//! - `total` 为分页前全量计数；offset 超出总数时条目为空而计数不变
//!
//! 安全约束:
//! - 使用临时文件库（init_pool 自动迁移）+ MockLlm，不调用真实 LLM、不连网、不触碰 keychain

mod common;

use ramaria_cli::commands::memory::l2_json_items;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StorageBackend, StoreCrud};
use ramaria_core::types::{MemoryEvent, Persona, PersonaKind};
use ramaria_service::{Engine, L2BrowseRequest};
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// 临时库序号（并行测试线程安全：纳秒可能撞车，追加原子计数保证唯一）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建唯一临时测试库路径（库文件位于独立目录内）。
fn temp_db_path(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-l2-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir.join("l2.db")
}

/// 构造真实 SQLite 引擎（空库，自动执行 migration）。
///
/// 返回 (引擎, 造数句柄, 连接池, 临时目录)：连接池供测试结束前关闭文件句柄。
async fn setup_engine(tag: &str) -> (Arc<Engine>, Arc<SqliteStorage>, SqlitePool, PathBuf) {
    let db = temp_db_path(tag);
    let dir = db.parent().expect("库文件应有父目录").to_path_buf();
    let pool = ramaria_storage::database::init_pool(Some(db))
        .await
        .expect("初始化测试数据库失败");
    let storage = Arc::new(SqliteStorage::new(pool.clone()));
    let storage_dyn: Arc<dyn StorageBackend> = storage.clone();
    let engine = Engine::from_parts(
        storage_dyn,
        Arc::new(common::MockLlm::new("memory-l2-test")),
        None,
        RamariaConfig::default(),
    );
    (Arc::new(engine), storage, pool, dir)
}

/// 关闭连接池并删除临时目录（Windows 下需先释放文件句柄）。
async fn cleanup(pool: SqlitePool, dir: PathBuf) {
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 造一个 persona 行（事件表的 persona_uid 外键依赖）。
async fn seed_persona(storage: &SqliteStorage, uid: &str) {
    let persona = Persona::new(
        uid.to_string(),
        "测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("写入 persona 应成功");
}

/// 造一条事件（`start` 显式给定，`end` 取 start 后 1 秒）。
async fn seed_event(storage: &SqliteStorage, persona: &str, title: &str, start: i64) {
    let event = MemoryEvent::new(
        persona.to_string(),
        title.to_string(),
        format!("{title}的摘要"),
        start,
        start + 1_000,
    );
    storage.save_event(&event).await.expect("写入事件应成功");
}

/// 分页口径：按 `start` 倒序切片，条目字段与底层事件一致。
#[tokio::test]
async fn memory_l2_page_slices_events_by_start_desc() {
    let (engine, storage, pool, dir) = setup_engine("slice").await;
    seed_persona(&storage, "char-0001").await;
    for i in 0..5_i64 {
        seed_event(&storage, "char-0001", &format!("事件{i}"), 1_000 + i * 10).await;
    }

    let page = engine
        .memory_l2(L2BrowseRequest {
            persona: Some("char-0001".to_string()),
            limit: Some(2),
            offset: Some(1),
        })
        .await
        .expect("L2 浏览应成功");

    assert_eq!(page.total, 5, "total 为分页前全量计数");
    let titles: Vec<&str> = page.items.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["事件3", "事件2"], "按 start 倒序跳过最新一条");
    assert_eq!(page.items[0].start, 1_030);
    assert_eq!(page.items[0].end, 2_030);
    assert_eq!(page.items[1].start, 1_020);
    assert_eq!(page.items[1].end, 2_020);
    assert_eq!(page.items[0].persona_uid, "char-0001");

    cleanup(pool, dir).await;
}

/// CLI `--json` 条目：字段集为对外契约，时间戳由视图毫秒转为 ISO-8601 UTC。
#[tokio::test]
async fn memory_l2_json_items_use_iso_times() {
    let (engine, storage, pool, dir) = setup_engine("json-items").await;
    seed_persona(&storage, "char-0001").await;
    seed_event(&storage, "char-0001", "明确时间", 1_700_000_000_000).await;
    seed_event(&storage, "char-0001", "缺失时间", 0).await;

    let page = engine
        .memory_l2(L2BrowseRequest {
            persona: Some("char-0001".to_string()),
            limit: Some(10),
            offset: Some(0),
        })
        .await
        .expect("L2 浏览应成功");

    let json_items = l2_json_items(&page.items);
    assert_eq!(json_items.len(), page.items.len(), "逐条转换不增不减");

    // 按 start 倒序：明确时间的事件在前，非正时间的事件在后
    let first = json_items[0].as_object().expect("条目应为对象");
    let mut keys: Vec<&str> = first.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "id",
        "title",
        "summary",
        "keywords",
        "valence",
        "confidence",
        "salience",
        "start",
        "end",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected, "条目字段集不多不少");
    assert_eq!(
        first["start"], "2023-11-14T22:13:20Z",
        "start 为 ISO-8601 UTC"
    );
    assert_eq!(first["end"], "2023-11-14T22:13:21Z", "end 为 ISO-8601 UTC");

    assert_eq!(
        json_items[1]["start"],
        serde_json::Value::Null,
        "start 非正时输出 null"
    );

    cleanup(pool, dir).await;
}

/// 偏移越界：条目为空、`total` 不受影响（CLI 展示走"暂无事件"分支）。
#[tokio::test]
async fn memory_l2_offset_beyond_total_returns_empty_page() {
    let (engine, storage, pool, dir) = setup_engine("empty").await;
    seed_persona(&storage, "char-0001").await;
    seed_event(&storage, "char-0001", "唯一事件", 1_000).await;

    let page = engine
        .memory_l2(L2BrowseRequest {
            persona: Some("char-0001".to_string()),
            limit: Some(10),
            offset: Some(99),
        })
        .await
        .expect("L2 浏览应成功");

    assert!(page.items.is_empty(), "offset 超出总数时无条目");
    assert_eq!(page.total, 1, "total 仍为分页前全量计数");

    cleanup(pool, dir).await;
}
