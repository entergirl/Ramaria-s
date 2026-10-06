//! crates/ramaria-cli/src/tests.rs - CLI 参数解析与入口装配单元测试
//!
//! 设计特点:
//! - 覆盖 clap 参数解析（--db 优先级 / probe 档位 / rule clusters / 帮助分组）
//! - 覆盖 `init_app` 装配路径（临时 SQLite 库，config.toml 读取与后端对齐）
//! - 覆盖退出码契约与入口统一错误文案的映射
//! - 经 `use super::*` 复用入口模块的私有项与装配函数

use super::*;
use clap::Parser;
use ramaria_core::traits::StoreInfrastructure;

use crate::cli::{Cli, Commands, McpCmd, ProbeArgs, RuleCmd, StyleCmd};

/// 串行化 env 变量测试（多个 #[test] 并行时会互相干扰环境变量）。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 解析 `ramaria <args>` 并返回 db 路径（仅解析，不执行命令）。
fn parse_db(args: &[&str]) -> PathBuf {
    Cli::try_parse_from(args).unwrap().db
}

/// RAMARIA_DB_PATH 生效：无 `--db` 时使用环境变量。
#[test]
fn db_path_uses_env_when_no_flag() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // safety: edition 2024 下 set_var 为 unsafe；测试内串行使用，无并发读
    unsafe { std::env::set_var("RAMARIA_DB_PATH", "env-data/assistant.db") };
    let db = parse_db(&["ramaria", "status"]);
    unsafe { std::env::remove_var("RAMARIA_DB_PATH") };
    assert_eq!(db, PathBuf::from("env-data/assistant.db"));
}

/// `--db` 优先于环境变量（优先级：--db > env > 默认）。
#[test]
fn db_path_flag_overrides_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // safety: 同 db_path_uses_env_when_no_flag
    unsafe { std::env::set_var("RAMARIA_DB_PATH", "env-data/assistant.db") };
    let db = parse_db(&["ramaria", "--db", "flag-data/custom.db", "status"]);
    unsafe { std::env::remove_var("RAMARIA_DB_PATH") };
    assert_eq!(db, PathBuf::from("flag-data/custom.db"));
}

/// 无 env 且无 `--db` 时使用默认路径。
#[test]
fn db_path_defaults_when_nothing_set() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::remove_var("RAMARIA_DB_PATH") };
    let db = parse_db(&["ramaria", "status"]);
    assert_eq!(db, PathBuf::from("data/ramaria_assistant.db"));
}

/// `probe run --no-rebuild-utt` 可解析，且映射到内部 rebuild_utt=false。
#[test]
fn probe_run_no_rebuild_utt_flag_parses() {
    let cli = Cli::try_parse_from([
        "ramaria",
        "probe",
        "run",
        "--dataset",
        "d.json",
        "--no-rebuild-utt",
    ])
    .expect("--no-rebuild-utt 应可解析");
    match cli.command {
        Commands::Probe(ProbeArgs::Run {
            dataset,
            no_rebuild_utt,
            repeat,
            ..
        }) => {
            assert_eq!(dataset, PathBuf::from("d.json"));
            assert!(no_rebuild_utt, "--no-rebuild-utt 应置位");
            assert_eq!(repeat, 1, "默认 repeat=1（不聚合）");
        }
        _ => panic!("应解析为 Probe::Run，实际解析为其他命令"),
    }
}

/// `probe report --ablation` 可解析（消融对比报告模式）。
#[test]
fn probe_report_ablation_flag_parses() {
    let cli = Cli::try_parse_from([
        "ramaria",
        "probe",
        "report",
        "--results",
        "r.json",
        "--evaluation",
        "e.json",
        "--ablation",
    ])
    .expect("--ablation 应可解析");
    match cli.command {
        Commands::Probe(ProbeArgs::Report { ablation, .. }) => {
            assert!(ablation, "--ablation 应置位");
        }
        _ => panic!("应解析为 Probe::Report"),
    }
    // 不带 --ablation → 默认 false（普通报告行为不变）
    let cli2 = Cli::try_parse_from(["ramaria", "probe", "report", "--results", "r.json"])
        .expect("普通 report 应可解析");
    match cli2.command {
        Commands::Probe(ProbeArgs::Report { ablation, .. }) => {
            assert!(!ablation, "默认 ablation=false");
        }
        _ => panic!("应解析为 Probe::Report"),
    }
}

/// `probe run --repeat N` 可解析，且默认不置 `--no-rebuild-utt`。
#[test]
fn probe_run_repeat_flag_parses() {
    let cli = Cli::try_parse_from([
        "ramaria",
        "probe",
        "run",
        "--dataset",
        "d.json",
        "--repeat",
        "5",
    ])
    .expect("--repeat 应可解析");
    match cli.command {
        Commands::Probe(ProbeArgs::Run {
            repeat,
            no_rebuild_utt,
            ..
        }) => {
            assert_eq!(repeat, 5);
            assert!(
                !no_rebuild_utt,
                "不带 --no-rebuild-utt 时默认应重建（rebuild_utt=true）"
            );
        }
        _ => panic!("应解析为 Probe::Run，实际解析为其他命令"),
    }
}

/// `probe baseline --window-hours` 可解析，缺省为 24。
#[test]
fn probe_baseline_window_hours_parses() {
    let cli = Cli::try_parse_from(["ramaria", "probe", "baseline", "--window-hours", "6"])
        .expect("--window-hours 应可解析");
    match cli.command {
        Commands::Probe(ProbeArgs::Baseline { window_hours }) => {
            assert_eq!(window_hours, 6);
        }
        _ => panic!("应解析为 Probe::Baseline，实际解析为其他命令"),
    }

    let cli2 = Cli::try_parse_from(["ramaria", "probe", "baseline"]).expect("缺省参数应可解析");
    match cli2.command {
        Commands::Probe(ProbeArgs::Baseline { window_hours }) => {
            assert_eq!(window_hours, 24, "回应窗口缺省 24 小时");
        }
        _ => panic!("应解析为 Probe::Baseline"),
    }
}

/// `ramaria mcp serve` 可解析（MCP 服务端入口；--db 沿用全局参数）。
#[test]
fn mcp_serve_parses() {
    let cli = Cli::try_parse_from(["ramaria", "mcp", "serve"]).expect("mcp serve 应可解析");
    assert!(
        matches!(cli.command, Commands::Mcp(McpCmd::Serve)),
        "应解析为 Mcp::Serve"
    );
    // --db 全局参数生效（MCP 宿主按此路径装配服务层引擎）
    let cli = Cli::try_parse_from(["ramaria", "--db", "d/x.db", "mcp", "serve"])
        .expect("带 --db 的 mcp serve 应可解析");
    assert_eq!(cli.db, PathBuf::from("d/x.db"));
}

/// `ramaria style update` 可解析（无 --persona → 命令层回退默认 rama-0001）。
#[test]
fn style_update_parses_default_persona() {
    let cli = Cli::try_parse_from(["ramaria", "style", "update"]).expect("style update 应可解析");
    match cli.command {
        Commands::Style(StyleCmd::Update { persona }) => {
            assert!(persona.is_none(), "缺省 --persona 时在命令层用默认值");
        }
        _ => panic!("应解析为 Style::Update，实际解析为其他命令"),
    }
}

/// `ramaria style update --persona <uid>` 可解析并透传 persona_uid。
#[test]
fn style_update_persona_flag_parses() {
    let cli = Cli::try_parse_from(["ramaria", "style", "update", "--persona", "char-2766366159"])
        .expect("--persona 应可解析");
    match cli.command {
        Commands::Style(StyleCmd::Update { persona }) => {
            assert_eq!(persona.as_deref(), Some("char-2766366159"));
        }
        _ => panic!("应解析为 Style::Update，实际解析为其他命令"),
    }
}

/// `ramaria rule clusters` 无覆盖参数可解析（全部 None，命令层回退配置值）。
#[test]
fn rule_clusters_parses_without_overrides() {
    let cli = Cli::try_parse_from(["ramaria", "rule", "clusters"]).expect("rule clusters 应可解析");
    match cli.command {
        Commands::Rule(RuleCmd::Clusters {
            persona,
            theta_nb,
            min_cluster_size,
            beta1,
            beta2,
            theta_join,
            split_ratio,
        }) => {
            assert!(persona.is_none(), "缺省 --persona 时在命令层用默认值");
            assert!(theta_nb.is_none() && min_cluster_size.is_none());
            assert!(beta1.is_none() && beta2.is_none());
            assert!(theta_join.is_empty(), "缺省不启用 θ_join 模拟");
            assert!((split_ratio - 0.8).abs() < 1e-12, "缺省 split_ratio=0.8");
        }
        _ => panic!("应解析为 Rule::Clusters，实际解析为其他命令"),
    }
}

/// `ramaria rule clusters` 聚类覆盖参数可解析并透传（不传 θ_join 时保持关闭）。
#[test]
fn rule_clusters_parses_all_overrides() {
    let cli = Cli::try_parse_from([
        "ramaria",
        "rule",
        "clusters",
        "--persona",
        "char-0001",
        "--theta-nb",
        "0.6",
        "--min-cluster-size",
        "2",
        "--beta1",
        "0.7",
        "--beta2",
        "0.2",
    ])
    .expect("rule clusters 覆盖参数应可解析");
    match cli.command {
        Commands::Rule(RuleCmd::Clusters {
            persona,
            theta_nb,
            min_cluster_size,
            beta1,
            beta2,
            theta_join,
            split_ratio,
        }) => {
            assert_eq!(persona.as_deref(), Some("char-0001"));
            assert_eq!(theta_nb, Some(0.6));
            assert_eq!(min_cluster_size, Some(2));
            assert_eq!(beta1, Some(0.7));
            assert_eq!(beta2, Some(0.2));
            assert!(theta_join.is_empty(), "未传 --theta-join 时不启用模拟");
            assert!((split_ratio - 0.8).abs() < 1e-12);
        }
        _ => panic!("应解析为 Rule::Clusters，实际解析为其他命令"),
    }
}

/// `ramaria rule clusters --theta-join` 多值/重复解析（启用 θ_join 时序增量模拟）。
#[test]
fn rule_clusters_parses_theta_join_multi_values() {
    let cli = Cli::try_parse_from([
        "ramaria",
        "rule",
        "clusters",
        "--theta-join",
        "0.6",
        "0.7",
        "--theta-join",
        "0.8",
        "--split-ratio",
        "0.7",
    ])
    .expect("--theta-join/--split-ratio 应可解析");
    match cli.command {
        Commands::Rule(RuleCmd::Clusters {
            theta_join,
            split_ratio,
            ..
        }) => {
            assert_eq!(theta_join, vec![0.6, 0.7, 0.8], "多值与重复出现按顺序累积");
            assert!((split_ratio - 0.7).abs() < 1e-12);
        }
        _ => panic!("应解析为 Rule::Clusters，实际解析为其他命令"),
    }
}

/// 帮助分组表必须覆盖全部顶层子命令，rule/fact 归"管理"分组。
#[test]
fn help_groups_cover_all_subcommands_and_rule_fact() {
    let groups = help_groups();
    // 全部顶层命令都有分组（防新增命令漏登记）
    for sub in Cli::command().get_subcommands() {
        let name = sub.get_name();
        if name == "help" {
            continue; // clap 自动生成的 help 子命令无需分组
        }
        assert!(
            groups.iter().any(|(n, _)| *n == name),
            "子命令 {name} 未在 help_groups 登记分组"
        );
    }
    assert!(groups.contains(&("rule", "管理")), "rule 应归管理分组");
    assert!(groups.contains(&("fact", "管理")), "fact 应归管理分组");
}

// =========================================================
// init_app 集成测试（真实 SQLite 临时库，不连网）
// =========================================================

/// 创建唯一临时测试目录（自动清理）。
fn temp_test_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-cli-init-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup_temp_dir(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

/// 释放引擎持有的连接池句柄（Windows 下删除临时目录前需关闭文件句柄）。
async fn close_engine_pool(engine: &Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
}

/// 断言 config.toml 参数被 CLI 链路读取。
#[tokio::test]
async fn init_app_loads_config_toml() {
    let dir = temp_test_dir("toml");
    let db_path = dir.join("assistant.db");
    // 预写 config.toml：`[utt] theta_gap_minutes = 45`（与默认 30 不同，用于断言读取生效）
    std::fs::write(dir.join("config.toml"), "[utt]\ntheta_gap_minutes = 45\n").unwrap();

    let engine = init_app(db_path).await.expect("init_app 应成功");
    assert_eq!(
        engine.config().utt.theta_gap_minutes,
        45,
        "config.toml 的 [utt] 参数必须被 CLI 链路读取"
    );
    close_engine_pool(&engine).await;
    cleanup_temp_dir(&dir);
}

/// config.toml 缺失 → 生成含默认值的模板，CLI 以默认配置启动。
#[tokio::test]
async fn init_app_generates_template_when_missing() {
    let dir = temp_test_dir("missing");
    let db_path = dir.join("assistant.db");

    let engine = init_app(db_path).await.expect("init_app 应成功");
    assert_eq!(engine.config().utt.theta_gap_minutes, 10, "缺失时用默认值");
    assert!(
        dir.join("config.toml").exists(),
        "config.toml 缺失时应生成模板"
    );
    close_engine_pool(&engine).await;
    cleanup_temp_dir(&dir);
}

/// config.toml 损坏 → 回退默认值记 warn，启动不失败。
#[tokio::test]
async fn init_app_falls_back_on_corrupt_config() {
    let dir = temp_test_dir("corrupt");
    let db_path = dir.join("assistant.db");
    std::fs::write(dir.join("config.toml"), "这不是合法的 TOML [[[").unwrap();

    let engine = init_app(db_path).await.expect("损坏 config 不应阻塞启动");
    assert_eq!(
        engine.config().utt.theta_gap_minutes,
        10,
        "损坏时回退默认值"
    );
    close_engine_pool(&engine).await;
    cleanup_temp_dir(&dir);
}

/// embedding_model_path 指向不存在目录 → 降级为 None 不阻塞。
#[tokio::test]
async fn init_app_degrades_when_embedding_missing() {
    let dir = temp_test_dir("embed");
    let db_path = dir.join("assistant.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .unwrap();
    let storage = ramaria_storage::SqliteStorage::new(pool.clone());
    let mut backend = ramaria_core::types::BackendConfig::lm_studio_default();
    backend.embedding_model_path = Some(dir.join("no-such-model").to_string_lossy().to_string());
    storage.save_backend_config(&backend).await.unwrap();
    pool.close().await;

    let engine = init_app(db_path).await.expect("embedding 缺失不应阻塞启动");
    assert!(
        !engine.is_embedding_available(),
        "模型目录不存在时 embedding 不可用（BM25 降级）"
    );
    close_engine_pool(&engine).await;
    cleanup_temp_dir(&dir);
}

/// 新库（backend_config 表无记录）首次启动：config.toml 的 `[backend]`（deepseek）
/// 必须经配置同步后生效，而不是回退 lm_studio_default() 指向 localhost:1234。
///
/// 回归背景：装配时库内无后端配置会回退 LM Studio，导致 L1/L2 全失败
/// （首次导入因此失败）；同步完成后的库内记录必须成为 provider 的真相源。
#[tokio::test]
async fn init_app_uses_config_toml_backend_on_fresh_db() {
    let dir = temp_test_dir("backend");
    let db_path = dir.join("assistant.db");
    // 预写 config.toml：deepseek 后端（新库 backend_config 表无任何记录）
    std::fs::write(
        dir.join("config.toml"),
        "[backend]\nprovider = \"deepseek\"\nbase_url = \"https://api.deepseek.com\"\nmodel_id = \"deepseek-chat\"\napi_key = \"test-only-key\"\n",
    )
    .unwrap();

    let engine = init_app(db_path).await.expect("init_app 应成功");
    assert_eq!(
        engine.llm().name(),
        "DeepSeek",
        "新库首次启动必须采用 config.toml 的 [backend]（BUG-M5b-01）"
    );

    // 同步后 DB backend_config 也应记录 deepseek（文件为准回写）
    let bc = engine
        .storage()
        .get_backend_config()
        .await
        .expect("读取 backend_config 应成功")
        .expect("新库同步后 backend_config 应有记录");
    assert_eq!(
        bc.provider.as_str(),
        "deepseek",
        "DB backend_config 应为 deepseek"
    );
    assert_eq!(
        bc.base_url, "https://api.deepseek.com",
        "base_url 应以文件为准"
    );

    close_engine_pool(&engine).await;
    cleanup_temp_dir(&dir);
}

/// 库内已保存 LM Studio 但 config.toml 指定 deepseek → 以文件为准。
///
/// 覆盖"文件与 DB 不一致"场景：同步写回 DB 后，LLM 应使用文件侧的 deepseek。
#[tokio::test]
async fn init_app_prefers_config_toml_over_stale_db_backend() {
    let dir = temp_test_dir("backend-stale");
    let db_path = dir.join("assistant.db");
    // 预写 config.toml：deepseek
    std::fs::write(
        dir.join("config.toml"),
        "[backend]\nprovider = \"deepseek\"\nbase_url = \"https://api.deepseek.com\"\nmodel_id = \"deepseek-chat\"\napi_key = \"test-only-key\"\n",
    )
    .unwrap();
    // 预写 DB：LM Studio（旧状态，模拟升级前的遗留配置）
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .unwrap();
    let storage = ramaria_storage::SqliteStorage::new(pool.clone());
    storage
        .save_backend_config(&ramaria_core::types::BackendConfig::lm_studio_default())
        .await
        .unwrap();
    pool.close().await;

    let engine = init_app(db_path).await.expect("init_app 应成功");
    assert_eq!(
        engine.llm().name(),
        "DeepSeek",
        "文件与 DB 不一致时应以 config.toml 为准"
    );

    close_engine_pool(&engine).await;
    cleanup_temp_dir(&dir);
}

// =========================================================
// 退出码契约
// =========================================================

/// 退出码契约：anyhow 上下文包裹后仍按错误链中的 RamariaError 分类。
#[test]
fn exit_code_maps_ramaria_error_through_context_chain() {
    let llm = anyhow::Error::from(RamariaError::llm("LLM 不可用")).context("L1 摘要生成失败");
    assert_eq!(exit_code_for_error(&llm), 3);
    let storage = anyhow::Error::from(RamariaError::storage("x")).context("索引重建失败");
    assert_eq!(exit_code_for_error(&storage), 3);
    let validation = anyhow::Error::from(RamariaError::validation("x")).context("y");
    assert_eq!(exit_code_for_error(&validation), 4);
    // 对照：降级为纯字符串的错误无法分类 → 退化为 1（修复前的缺陷形态）
    let degraded = anyhow::anyhow!("L1 摘要生成失败: llm 不可用");
    assert_eq!(exit_code_for_error(&degraded), 1);
}

/// 入口统一文案：业务类原文直出；技术类场景 + 中文类别标题 + 原因；无上下文用兜底场景。
#[test]
fn entry_message_maps_through_context_chain() {
    // validation：被 anyhow 上下文包裹后，用户可见消息仍是业务原文（exit 4 路径）
    let validation =
        anyhow::Error::from(RamariaError::validation("会话不存在: abc")).context("查询会话失败");
    let (_, message) = mapped_error_message(&validation).expect("应识别服务层错误");
    assert_eq!(message, "会话不存在: abc");

    // privacy：同业务类，原文直出
    let privacy =
        anyhow::Error::from(RamariaError::privacy("请先完成隐私确认")).context("生成回复失败");
    let (_, message) = mapped_error_message(&privacy).expect("应识别服务层错误");
    assert_eq!(message, "请先完成隐私确认");

    // storage + 上下文：场景 + 中文类别标题 + 原因（不复现英文类别串）
    let storage = anyhow::Error::from(RamariaError::storage("磁盘只读")).context("索引重建失败");
    let (_, message) = mapped_error_message(&storage).expect("应识别服务层错误");
    assert_eq!(message, "索引重建失败: 数据库错误: 磁盘只读");
    assert!(
        !message.contains("storage error"),
        "不应复现英文类别串: {message}"
    );

    // 多层上下文：场景取最外层上下文文本
    let nested = anyhow::Error::from(RamariaError::index("索引损坏"))
        .context("调用方标记")
        .context("命令层");
    let (_, message) = mapped_error_message(&nested).expect("应识别服务层错误");
    assert_eq!(message, "命令层: 索引错误: 索引损坏");

    // 无 anyhow 上下文：退化为固定兜底场景
    let bare = anyhow::Error::from(RamariaError::llm("连接超时"));
    let (_, message) = mapped_error_message(&bare).expect("应识别服务层错误");
    assert_eq!(
        message,
        format!("{FALLBACK_ERROR_SCENE}: LLM 服务错误: 连接超时")
    );

    // 纯 anyhow 错误：不进入统一映射（维持现状渲染路径）
    assert!(mapped_error_message(&anyhow::anyhow!("读取配置失败: boom")).is_none());
}
