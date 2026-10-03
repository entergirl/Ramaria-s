//! crates/ramaria-service/src/engine/tests.rs - Ramaria 引擎装配与用例入口单元测试
//!
//! 设计特点:
//! - 装配用例：无配置 / 有配置 / DB 侧后端配置三条打开路径，锁定默认回退与只读纪律
//! - 注入构造用例：`from_parts` 不触碰数据库文件与配置文件；连接池附着门面往返
//! - 用例入口可达性：空库上的读用例返回结构完整结果、写用例按边界显式报错
//! - 配置用例：双写落盘 / 重载一致性 / 空 config_path 显式错误
//! - 冷却窗口判定：间隔 0 / 从未构建 / 窗口内 / 超窗四种口径
//!
//! 安全约束:
//! - 使用临时目录合成库与配置，不使用真实 API key / LLM 网络调用 / 用户数据。

use super::*;
use crate::types::{HistoryRequest, IngestRequest, RecallRequest};
use ramaria_core::config::RamariaConfig as TestConfig;
use ramaria_core::lock::read_recover;
use ramaria_core::traits::StoreInfrastructure;
use ramaria_core::types::{BackendConfig, now_ms};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

/// 创建唯一临时目录（测试结束前由调用方清理）。
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-service-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    dir
}

/// 无 config.toml 时：使用默认配置、LLM 回退 LM Studio、嵌入降级不报错。
#[tokio::test]
async fn open_without_config_uses_defaults_and_degrades_embedding() {
    let dir = temp_dir("defaults");
    let db_path = dir.join("assistant.db");

    let engine = Engine::open(db_path.clone()).await.expect("引擎装配应成功");

    // 存储可用（空库也可正常查询）
    assert!(
        engine.storage().list_personas().await.is_ok(),
        "装配后存储后端应可查询"
    );
    // 无后端配置记录 → 回退 LM Studio 默认
    assert_eq!(engine.llm().name(), "LM Studio");
    // 无嵌入模型 → 降级但装配成功
    assert!(
        !engine.is_embedding_available(),
        "无嵌入模型时向量通道应降级"
    );
    // 检索器懒加载占位：装配后未加载
    assert!(!engine.is_retriever_loaded(), "装配阶段不应加载检索索引");
    // 默认配置生效
    assert_eq!(engine.config().session.l1_idle_minutes, 10);
    assert_eq!(engine.db_path(), db_path.as_path());
    // 文件缺失属正常默认路径，不算回退告警
    assert!(engine.config_warning().is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 探针只读访问器：装配后检索器槽未加载（None）、关键词镜像为空。
#[tokio::test]
async fn probe_handles_expose_empty_state_after_assembly() {
    let dir = temp_dir("probe-handles");
    let db_path = dir.join("assistant.db");
    let engine = Engine::open(db_path).await.expect("引擎装配应成功");

    let retriever = engine.retriever_slot();
    assert!(
        read_recover(&retriever, "engine.probe.retriever").is_none(),
        "装配阶段检索器槽应为空（懒加载占位）"
    );

    let mirror = engine.keyword_mirror();
    let guard = read_recover(&mirror, "engine.probe.keyword_mirror");
    assert_eq!(guard.doc_count(), 0, "装配阶段关键词镜像应为空");
    assert_eq!(guard.pool_len(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// config.toml 存在时：按文件生效，且本层不写回（只读装配）。
#[tokio::test]
async fn open_reads_config_toml_readonly() {
    let dir = temp_dir("config");
    let db_path = dir.join("assistant.db");
    std::fs::write(
        dir.join("config.toml"),
        "[session]\nl1_idle_minutes = 25\n\n[utt]\ntheta_gap_minutes = 45\n",
    )
    .expect("写入 config.toml 应成功");
    let before = std::fs::read_to_string(dir.join("config.toml")).expect("读取配置应成功");

    let engine = Engine::open(db_path).await.expect("引擎装配应成功");
    assert_eq!(
        engine.config().session.l1_idle_minutes,
        25,
        "config.toml 的 [session] 必须被服务层读取"
    );
    assert_eq!(
        engine.config().utt.theta_gap_minutes,
        45,
        "config.toml 的 [utt] 必须被服务层读取"
    );
    assert!(
        engine.config_warning().is_none(),
        "合法配置装配不应产生回退告警"
    );

    // 只读纪律：装配过程不得改写配置文件
    let after = std::fs::read_to_string(dir.join("config.toml")).expect("读取配置应成功");
    assert_eq!(before, after, "服务层装配不得写回 config.toml");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 非法 config.toml：装配成功（回退默认）且携带解析失败告警；写回成功后清除。
#[tokio::test]
async fn open_with_broken_config_reports_warning_then_clears_after_save() {
    let dir = temp_dir("config-broken");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "[session\nl1_idle_minutes = ").expect("写入非法配置应成功");

    let engine =
        Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
            .await
            .expect("配置非法时引擎应按默认配置完成装配");
    let warning = engine.config_warning().expect("非法配置应携带回退告警");
    assert!(
        warning.contains("解析失败"),
        "告警应说明解析失败: {warning}"
    );
    assert_eq!(
        engine.config().session.l1_idle_minutes,
        10,
        "非法配置应回退默认值"
    );

    // 双写写回（文件被合法配置覆盖）成功后：告警清除
    let mut cfg = engine.config().as_ref().clone();
    cfg.session.l1_idle_minutes = 33;
    let result = engine.save_config(&cfg).await.expect("保存配置应成功");
    assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);
    assert!(
        engine.config_warning().is_none(),
        "配置成功写回后应清除回退告警"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 配置双写用例：带 config_path 装配后，save_config 双侧落盘并热重载内存快照。
#[tokio::test]
async fn save_config_updates_in_memory_snapshot() {
    let dir = temp_dir("config-save");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    let engine =
        Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
            .await
            .expect("引擎装配应成功");

    // 装配期只读：未生成配置文件，快照为默认值
    assert!(!config_path.exists(), "装配不得写回 config.toml");
    assert_eq!(engine.config().session.l1_idle_minutes, 10);

    let mut cfg = engine.config().as_ref().clone();
    cfg.session.l1_idle_minutes = 42;
    let result = engine.save_config(&cfg).await.expect("保存配置应成功");
    assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

    // 内存快照已热重载（后续用例读取生效）
    assert_eq!(
        engine.config().session.l1_idle_minutes,
        42,
        "save_config 后快照应更新"
    );

    // 文件侧与 DB 侧同步落盘
    let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
    let file_cfg: TestConfig = toml::from_str(&text).expect("文件应为合法 TOML");
    assert_eq!(file_cfg.session.l1_idle_minutes, 42);
    let stored = engine
        .storage()
        .get_setting("config.session.l1_idle_minutes")
        .await
        .expect("读取 settings 应成功");
    assert_eq!(stored.as_deref(), Some("42"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 配置重载用例：文件改值后 reload → 快照与 DB 对齐（以文件为准回写）。
#[tokio::test]
async fn reload_config_reads_file_and_writes_db() {
    let dir = temp_dir("config-reload");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "[utt]\ntheta_gap_minutes = 30\n").expect("写入配置应成功");

    let engine =
        Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
            .await
            .expect("引擎装配应成功");
    assert_eq!(engine.config().utt.theta_gap_minutes, 30);

    // 外部直写 DB 制造不一致残值
    engine
        .storage()
        .set_setting("config.utt.theta_gap_minutes", "60")
        .await
        .expect("写入 settings 应成功");
    // 文件改值 → reload：一致性校验以文件为准回写 DB，并热重载快照
    std::fs::write(&config_path, "[utt]\ntheta_gap_minutes = 25\n").expect("写入配置应成功");
    let outcome = engine.reload_config().await.expect("重载应成功");

    assert_eq!(outcome.config.utt.theta_gap_minutes, 25);
    assert!(
        outcome
            .mismatches
            .iter()
            .any(|m| m.key == "config.utt.theta_gap_minutes"),
        "不一致应记入 mismatch: {:?}",
        outcome.mismatches
    );
    assert_eq!(
        engine.config().utt.theta_gap_minutes,
        25,
        "reload 后快照应与文件一致"
    );
    let stored = engine
        .storage()
        .get_setting("config.utt.theta_gap_minutes")
        .await
        .expect("读取 settings 应成功");
    assert_eq!(stored.as_deref(), Some("25"), "DB 应以文件为准回写");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 配置用例边界：注入构造（空 config_path）调用配置用例返回显式错误，不 panic。
#[tokio::test]
async fn config_writer_requires_config_path() {
    let (engine, _storage, dir) = crate::test_support::engine_with_db("config-no-path").await;
    assert!(
        engine.config_path().as_os_str().is_empty(),
        "注入构造不携带配置路径"
    );

    let err = engine
        .save_config(&TestConfig::default())
        .await
        .expect_err("空 config_path 应报错");
    assert_eq!(err.category(), "config");

    let err = engine
        .reload_config()
        .await
        .expect_err("空 config_path 应报错");
    assert_eq!(err.category(), "config");

    let err = engine
        .load_full_config()
        .await
        .expect_err("空 config_path 应报错");
    assert_eq!(err.category(), "config");

    let err = engine
        .sync_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect_err("空 config_path 应报错");
    assert_eq!(err.category(), "config");

    let _ = std::fs::remove_dir_all(&dir);
}

/// DB 侧 backend_config 记录被采用（验证后端配置来源为数据库）。
#[tokio::test]
async fn open_uses_saved_backend_config() {
    let dir = temp_dir("backend");
    let db_path = dir.join("assistant.db");

    // 预写 DB：LM Studio 自定义 base_url（与默认不同，用于断言读取生效）
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .expect("初始化测试库应成功");
    let storage = SqliteStorage::new(pool.clone());
    let mut backend = BackendConfig::lm_studio_default();
    backend.base_url = "http://localhost:9999/v1".to_string();
    backend.capability.base_url = "http://localhost:9999/v1".to_string();
    storage
        .save_backend_config(&backend)
        .await
        .expect("保存后端配置应成功");
    pool.close().await;

    let engine = Engine::open(db_path).await.expect("引擎装配应成功");
    assert_eq!(engine.llm().name(), "LM Studio");
    assert_eq!(
        engine.llm().config().base_url,
        "http://localhost:9999/v1",
        "LLM provider 应使用 DB 侧 backend_config"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 用例入口可达：空库上的读用例返回结构完整的结果，写用例按边界显式报错。
#[tokio::test]
async fn use_cases_are_reachable_on_empty_db() {
    let dir = temp_dir("usecases");
    let db_path = dir.join("assistant.db");
    let engine = Engine::open(db_path).await.expect("引擎装配应成功");

    // 召回：空库 → 空结果（不报错），且进入概览模式（无 query / messages）
    let result = engine
        .recall(RecallRequest::default())
        .await
        .expect("空库召回应成功");
    assert!(result.items.is_empty());
    assert_eq!(result.stats.mode, crate::types::RecallMode::Overview);

    // 写入：空 messages 是边界错误（显式 Validation，不静默成功）
    let err = engine
        .ingest(IngestRequest {
            messages: Vec::new(),
            persona: None,
            conversation_id: None,
            channel: crate::types::CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect_err("空 messages 应报错");
    assert_eq!(err.category(), "validation");

    // 封存：不存在的会话 → 未抢到（幂等语义，不报错）
    let outcome = engine.seal(Uuid::nil()).await.expect("封存应成功返回");
    assert!(!outcome.sealed);
    assert_eq!(outcome.l1_count, 0);

    // 空闲检查：无活跃会话 → 0
    assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);

    // 历史：无 session_id / persona → 空结构
    let history = engine
        .history(HistoryRequest::default())
        .await
        .expect("历史读取应成功");
    assert!(history.session_id.is_none());
    assert!(history.messages.is_empty());

    // 人格列表：空库 → 空列表
    assert!(
        engine
            .persona_list()
            .await
            .expect("人格列表应成功")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重建冷却窗口：间隔为 0（默认）恒允许；配置间隔后按"最近构建完成时间"判定。
#[tokio::test]
async fn index_rebuild_cooldown_follows_config_interval() {
    let dir = temp_dir("cooldown");
    let db_path = dir.join("assistant.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("初始化测试库应成功");
    let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));
    let keychain = Arc::new(Keychain::new());
    let llm = build_llm_provider(&BackendConfig::lm_studio_default(), &keychain, None)
        .expect("构建本地 provider 应成功");

    // 间隔 0（默认）: 不节流 —— 跨进程写入即时可见
    let engine = Engine::from_parts(
        Arc::clone(&storage),
        Arc::clone(&llm),
        None,
        TestConfig::default(),
    );
    assert_eq!(engine.config().index.refresh_interval_seconds, 0);
    engine.record_index_build_time(now_ms());
    assert!(
        engine.index_rebuild_cooldown_elapsed(),
        "间隔为 0 时应恒允许重建"
    );

    // 间隔 60 秒: 从未构建 / 窗口内 / 超过窗口 三种判定
    let mut config = TestConfig::default();
    config.index.refresh_interval_seconds = 60;
    let engine = Engine::from_parts(storage, llm, None, config);
    assert!(
        engine.index_rebuild_cooldown_elapsed(),
        "从未构建过索引 → 允许（首次加载不受节流约束）"
    );
    engine.record_index_build_time(now_ms());
    assert!(
        !engine.index_rebuild_cooldown_elapsed(),
        "刚构建完成 → 冷却窗口内不允许重建"
    );
    engine.record_index_build_time(now_ms() - 61_000);
    assert!(
        engine.index_rebuild_cooldown_elapsed(),
        "距上次构建超过间隔 → 允许重建"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `from_parts` 注入构造：不触碰数据库文件与配置文件。
#[tokio::test]
async fn from_parts_constructs_without_io() {
    let dir = temp_dir("parts");
    let db_path = dir.join("assistant.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("初始化测试库应成功");
    let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));

    // mock LLM：仅实现 trait 必需方法的最小子集成本较高，此处用真实本地 provider
    // （不发起网络调用，仅构造）验证注入路径；embedding 显式注入 None 走降级。
    let keychain = Arc::new(Keychain::new());
    let llm = build_llm_provider(&BackendConfig::lm_studio_default(), &keychain, None)
        .expect("构建本地 provider 应成功");
    let engine = Engine::from_parts(storage, llm, None, TestConfig::default());

    assert!(!engine.is_embedding_available());
    assert!(!engine.is_retriever_loaded());
    assert!(
        engine.db_path().as_os_str().is_empty(),
        "from_parts 不携带库路径"
    );
    engine
        .storage()
        .list_personas()
        .await
        .expect("注入的存储应可查询");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 连接池门面：注入构造默认未附着；附着后读取返回共享句柄；装配路径自动携带。
#[tokio::test]
async fn attach_sqlite_pool_roundtrip() {
    let dir = temp_dir("pool-attach");
    let db_path = dir.join("assistant.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .expect("初始化测试库应成功");
    let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool.clone()));
    let keychain = Arc::new(Keychain::new());
    let llm = build_llm_provider(&BackendConfig::lm_studio_default(), &keychain, None)
        .expect("构建本地 provider 应成功");

    let engine = Engine::from_parts(storage, llm, None, TestConfig::default());
    assert!(engine.sqlite_pool().is_none(), "注入构造默认不携带连接池");
    engine.attach_sqlite_pool(pool);
    assert!(engine.sqlite_pool().is_some(), "附着后应可读取连接池句柄");

    // 装配路径（open_with）自动携带连接池句柄
    let engine = Engine::open(db_path).await.expect("引擎装配应成功");
    assert!(
        engine.sqlite_pool().is_some(),
        "装配路径应自动携带连接池句柄"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
