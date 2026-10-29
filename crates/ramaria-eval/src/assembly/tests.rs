//! crates/ramaria-eval/src/assembly/tests.rs - 评测装配单元测试
//!
//! 设计特点:
//! - 快照断言覆盖开关、路径，并逐键比对"除覆盖项外等于产品默认"
//! - 装配断言库与快照落在评测根，生效配置取自快照
//! - 后台任务以运行时存活任务数前后对比锁定，而不是只看装配能否跑通
//! - 每个用例独立临时目录，结束前关闭连接池再删除（Windows 句柄未释放会拒绝删除）

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ramaria_core::config::RamariaConfig;

use super::*;

/// 独立临时评测根：同一进程内并行用例互不干扰。
fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-assembly-{tag}-{nanos}"))
}

/// 删除临时目录；清理失败不掩盖断言结果。
fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// 关闭引擎持有的连接池，释放 SQLite 文件句柄。
async fn close(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
}

/// 读取快照文件并解析为配置。
fn parse_snapshot(root: &Path) -> RamariaConfig {
    let path = root.join(EVAL_CONFIG_FILE);
    let text = std::fs::read_to_string(&path).expect("快照文件应可读");
    toml::from_str(&text).expect("快照应可解析为完整配置")
}

/// 配置转为 TOML 值（逐键比对用）。
fn as_value(config: &RamariaConfig) -> toml::Value {
    toml::Value::try_from(config).expect("配置应可转为 TOML 值")
}

/// 快照关闭通道与主动对话，路径指向评测根。
#[test]
fn snapshot_disables_channel_and_proactive() {
    let root = temp_root("snapshot");
    let path = write_config_snapshot(&root).expect("写快照应成功");
    assert_eq!(path, root.join(EVAL_CONFIG_FILE), "返回路径应为快照文件");

    let config = parse_snapshot(&root);
    assert!(!config.channel.enabled, "社交通道总开关应关闭");
    assert!(!config.channel.qq_enabled, "QQ 通道应关闭");
    assert!(!config.channel.telegram_enabled, "Telegram 通道应关闭");
    assert!(
        !config.channel.proactive_delivery_enabled,
        "社交主动投递应关闭"
    );
    assert!(!config.proactive.enabled, "主动对话总开关应关闭");

    assert_eq!(config.paths.data_dir, root.display().to_string());
    assert_eq!(config.paths.config_dir, root.display().to_string());
    assert_eq!(
        config.paths.log_dir,
        root.join("logs").display().to_string()
    );
    assert_eq!(
        config.paths.vector_index_dir,
        root.join("vectors").display().to_string()
    );
    cleanup(&root);
}

/// 快照是全量配置：除覆盖项外与产品默认逐键一致。
#[test]
fn snapshot_is_full() {
    let root = temp_root("full");
    write_config_snapshot(&root).expect("写快照应成功");
    let parsed = parse_snapshot(&root);

    let mut snapshot_value = as_value(&parsed);
    let mut default_value = as_value(&RamariaConfig::default());
    for key in ["paths", "channel", "proactive"] {
        snapshot_value
            .as_table_mut()
            .expect("配置根应为表")
            .remove(key);
        default_value
            .as_table_mut()
            .expect("配置根应为表")
            .remove(key);
    }
    assert_eq!(
        snapshot_value, default_value,
        "除覆盖项外，快照应与产品默认逐键一致"
    );

    let table = snapshot_value.as_table().expect("配置根应为表");
    for key in [
        "retrieval",
        "session",
        "thresholds",
        "index",
        "l1",
        "utt",
        "cache",
        "behavior",
    ] {
        assert!(table.contains_key(key), "全量快照应含 [{key}] 分组");
    }
    cleanup(&root);
}

/// 装配后库与快照都落在评测根，生效配置来自快照。
#[tokio::test]
async fn open_engine_uses_eval_root() {
    let root = temp_root("root");
    let engine = open_engine(&root).await.expect("装配应成功");

    assert!(root.join(EVAL_DB_FILE).exists(), "评测库应建在评测根");
    assert!(root.join(EVAL_CONFIG_FILE).exists(), "配置快照应落在评测根");
    assert_eq!(engine.db_path(), root.join(EVAL_DB_FILE).as_path());
    assert_eq!(engine.config_path(), root.join(EVAL_CONFIG_FILE).as_path());

    let config = engine.config();
    assert!(!config.channel.enabled);
    assert!(!config.channel.qq_enabled);
    assert!(!config.channel.telegram_enabled);
    assert!(!config.channel.proactive_delivery_enabled);
    assert!(!config.proactive.enabled);
    assert_eq!(config.paths.data_dir, root.display().to_string());
    assert!(engine.sqlite_pool().is_some(), "装配路径应携带连接池句柄");
    assert!(
        engine.config_warning().is_none(),
        "快照应正常解析，不应产生回退告警"
    );

    drop(config);
    close(engine).await;
    cleanup(&root);
}

/// 装配不拉起后台调度任务：与普通装配的任务增量一致，关闭后回到基线。
#[tokio::test]
async fn open_engine_starts_no_background_task() {
    let root = temp_root("no-task");
    let control_root = temp_root("control");
    let handle = tokio::runtime::Handle::current();
    let baseline = handle.metrics().num_alive_tasks();

    // 对照：普通装配同样会拉起 sqlx 连接池维护任务，先量出这条基线增量。
    let control = Engine::open_with(EngineOptions::new(control_root.join("ramaria.db")))
        .await
        .expect("对照片段装配应成功");
    let control_tasks = handle.metrics().num_alive_tasks();

    let engine = open_engine(&root).await.expect("装配应成功");
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let eval_tasks = handle.metrics().num_alive_tasks();
    assert_eq!(
        eval_tasks, control_tasks,
        "评测装配不应比普通装配多出后台任务（对照 {control_tasks}，评测 {eval_tasks}）"
    );

    close(engine).await;
    close(control).await;
    for _ in 0..50 {
        if handle.metrics().num_alive_tasks() == baseline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        handle.metrics().num_alive_tasks(),
        baseline,
        "关闭装配后不应残留后台任务"
    );

    cleanup(&root);
    cleanup(&control_root);
}

/// 同一评测根可重复装配，两次生效配置一致。
#[tokio::test]
async fn open_engine_is_idempotent() {
    let root = temp_root("reopen");

    let first = open_engine(&root).await.expect("首次装配应成功");
    let first_config = first.config();
    close(first).await;

    let second = open_engine(&root).await.expect("二次装配应成功");
    let second_config = second.config();

    assert_eq!(
        as_value(&second_config),
        as_value(&first_config),
        "两次装配的生效配置应一致"
    );
    assert!(!second_config.proactive.enabled);
    assert_eq!(second_config.paths.data_dir, root.display().to_string());

    drop(second_config);
    drop(first_config);
    close(second).await;
    cleanup(&root);
}
