//! tests/probe_tests.rs - probe build / probe run 命令集成测试
//!
//! 设计特点:
//! - 命令级测试：直接调用 `commands::probe` 的公共 API（`build_dataset` / `build_experiment`），
//!   mock LLM + mock storage，确定性断言（v1.5 mock 约定：不跑真实 LLM）。
//! - 进程级测试：`probe dataset` alias 兼容 + `--json` 信封 + 空库 fixture 兜底（端到端）。
//! - 覆盖 M2 验收要点：
//!   ① 测试集 JSON 结构断言（维度/题数/档位组合/seed 可复跑）；
//!   ② 无真实数据时 fixture 兜底路径；
//!   ③ 档位批量输出 JSON 结构断言 + mock LLM 确定性测试；
//!   ④ 单题失败不中断批量（FailingLlm）；
//!   ⑤ `probe dataset` alias 可用。

mod common;

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use ramaria_cli::commands::probe::{ProbeCmd, build_dataset, build_experiment};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, StorageBackend, StreamDelta};
use ramaria_core::types::{BackendConfig, LlmProvider as LlmProviderKind, ModelCapability};
use uuid::Uuid;

use common::{
    MockStorage, build_test_app, make_assistant_message, make_test_event, make_user_message,
};

/// 构造带 persona 消息 + 事件的测试 App（probe build 真实数据路径）。
fn build_app_with_probe_data() -> (Arc<ramaria_app::App>, Arc<MockStorage>) {
    let (app, storage) = build_test_app();
    storage.add_persona(common::make_test_persona(
        "char-0001",
        "角色一",
        ramaria_core::types::PersonaKind::Char,
        None,
    ));
    let sid = Uuid::new_v4();
    storage.create_session_with_messages(
        sid,
        vec![
            make_user_message(sid, "今天上班好累"),
            persona_reply(sid, "辛苦了，早点休息"),
            make_user_message(sid, "周末去哪玩"),
            persona_reply(sid, "去公园散步吧"),
        ],
    );
    for i in 1..=4 {
        storage.add_event("char-0001", make_test_event(i, &format!("测试事件{i}")));
    }
    (app, storage)
}

/// 构造带指定 persona_uid 的 assistant 消息（语气模仿配对的 persona 发言）。
fn persona_reply(session_id: Uuid, content: &str) -> ramaria_core::types::Message {
    let mut m = make_assistant_message(session_id, content);
    m.persona_uid = Some("char-0001".to_string());
    m
}

/// 用指定 LLM provider 构造 ready 状态的 App。
fn build_app_with_llm(llm: Arc<dyn LlmProvider>) -> (Arc<ramaria_app::App>, Arc<MockStorage>) {
    let storage = Arc::new(MockStorage::new());
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
    let config = ramaria_core::config::RamariaConfig::default();
    let app = ramaria_app::App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );
    app.set_state(ramaria_core::types::AppState::Ready);
    (Arc::new(app), storage)
}

/// 恒失败的 Mock LLM（验证单题失败不中断批量）。
struct FailingLlm {
    model_capability: ModelCapability,
    config: BackendConfig,
}

impl FailingLlm {
    fn new() -> Self {
        let config = BackendConfig::lm_studio_default();
        Self {
            model_capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "failing-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config,
        }
    }
}

#[async_trait]
impl LlmProvider for FailingLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Err(RamariaError::llm("mock: LLM 恒失败"))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Err(RamariaError::llm("mock: LLM 恒失败"))
    }

    fn capability(&self) -> &ModelCapability {
        &self.model_capability
    }

    fn config(&self) -> &BackendConfig {
        &self.config
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "FailingLlm"
    }
}

// =========================================================
// probe build：测试集构建
// =========================================================

/// 空数据库 → 全部使用内置夹具兜底（M2 验收：fixture 兜底路径）。
#[tokio::test]
async fn probe_build_empty_db_falls_back_to_fixture() {
    let (app, _storage) = build_test_app();
    let ds = build_dataset(&app, None, 10, 2026_0810, None).await;

    assert_eq!(ds.source, "fixture", "空库应降级为夹具数据");
    assert_eq!(
        ds.persona_uid, "char-0001",
        "无白名单 persona 时默认 char-0001"
    );
    assert_eq!(ds.items.len(), 20, "2 维 × 每维 10 题");
    assert_eq!(ds.dimensions, vec!["tone", "fact"]);
    assert_eq!(ds.variants.len(), 4, "代表配对 4 档位");
    assert!(
        ds.items.iter().all(|i| i.source == "fixture"),
        "空库时每题都应来自夹具"
    );
    assert_eq!(
        ds.items.iter().filter(|i| i.dimension == "tone").count(),
        10,
        "tone 维度恰好 10 题"
    );
    assert_eq!(
        ds.items.iter().filter(|i| i.dimension == "fact").count(),
        10,
        "fact 维度恰好 10 题"
    );
}

/// 有真实数据 → 真实数据优先，夹具仅补齐不足部分。
#[tokio::test]
async fn probe_build_with_data_prefers_db_items() {
    let (app, _storage) = build_app_with_probe_data();
    let ds = build_dataset(&app, None, 10, 2026_0810, None).await;

    assert_eq!(ds.source, "db", "有真实数据时主来源为 db");
    // 2 组 tone 配对 + 4 条事件 → 真实 6 题，夹具补齐 14 题
    let real = ds.items.iter().filter(|i| i.source == "db").count();
    let fixture = ds.items.iter().filter(|i| i.source == "fixture").count();
    assert_eq!(real, 6, "真实数据 6 题（2 tone + 4 fact）");
    assert_eq!(fixture, 14, "夹具补齐 14 题");
    assert_eq!(ds.items.len(), 20);
    // 真实数据应排在每维前面
    let first_real_tone = ds.items.iter().find(|i| i.dimension == "tone").unwrap();
    assert_eq!(first_real_tone.source, "db");
    // 题目内容来自数据库
    assert!(ds.items.iter().any(|i| i.question.contains("今天上班好累")));
    assert!(ds.items.iter().any(|i| i.question.contains("测试事件1")));
}

/// 同 seed 复跑产生完全相同的测试集（验收：seed 可复跑）。
#[tokio::test]
async fn probe_build_same_seed_reproducible() {
    let (app, _storage) = build_app_with_probe_data();
    let a = build_dataset(&app, None, 10, 42, None).await;
    let b = build_dataset(&app, None, 10, 42, None).await;

    let ids_a: Vec<String> = a.items.iter().map(|i| i.id.clone()).collect();
    let ids_b: Vec<String> = b.items.iter().map(|i| i.id.clone()).collect();
    assert_eq!(ids_a, ids_b, "同 seed 必须产生相同测试集");
    let qa: Vec<&str> = a.items.iter().map(|i| i.question.as_str()).collect();
    let qb: Vec<&str> = b.items.iter().map(|i| i.question.as_str()).collect();
    assert_eq!(qa, qb);
    assert_eq!(a.seed, b.seed);
}

/// 数据集可序列化为合法 JSON 且结构完整（probe build --json 的数据面）。
#[tokio::test]
async fn probe_build_dataset_serializes_to_valid_json() {
    let (app, _storage) = build_test_app();
    let ds = build_dataset(&app, None, 3, 7, None).await;
    let json = serde_json::to_value(&ds).expect("数据集必须可序列化");
    assert!(json["items"].is_array());
    assert_eq!(json["items"].as_array().unwrap().len(), 6);
    assert!(json["variants"][0]["id"].is_string());
    assert!(json["items"][0]["question"].is_string());
    assert!(
        json["items"][0]["reference"].is_string(),
        "每题应带参考回答"
    );
}

/// 命令级入口：`probe build` 返回 Ok（输出路径由进程级测试覆盖）。
#[tokio::test]
async fn probe_build_command_runs_ok() {
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::probe::run(
        &app,
        ProbeCmd::Build {
            persona: None,
            questions_per_dim: 5,
            seed: 1,
            source: None,
            output: None,
            json: false,
        },
        false,
    )
    .await;
    assert!(
        result.is_ok(),
        "probe build 命令应成功（空库 fixture 兜底）"
    );
}

// =========================================================
// probe run：档位批量实验
// =========================================================

/// 用小数据集（4 题）跑全部档位：输出结构断言（档位 → 输出 → 指标）。
#[tokio::test]
async fn probe_run_batch_structure_with_mock_llm() {
    let (app, _storage) = build_test_app();
    let ds = build_dataset(&app, None, 2, 7, None).await; // 2 维 × 2 题 = 4 题
    let experiment = build_experiment(
        &app,
        &ds,
        &PathBuf::from("dataset.json"),
        None,
        None,
        false,
        false,
    )
    .await
    .expect("档位实验应成功");

    assert_eq!(experiment.persona_uid, ds.persona_uid);
    assert_eq!(experiment.variants.len(), 4, "默认全部 4 档位");
    for variant in &experiment.variants {
        assert_eq!(variant.runs.len(), 4, "每档位应跑全部 4 题");
        assert_eq!(variant.failed_count, 0, "MockLlm 下不应有失败");
        for run in &variant.runs {
            assert!(run.error.is_none(), "{} 不应报错", run.item_id);
            assert!(run.metrics.reply_chars > 0, "{} 应有回复内容", run.item_id);
            assert!(run.reply.contains("Hello"), "回复应来自 mock LLM");
        }
    }
    // 档位参数与数据集一致
    assert_eq!(experiment.variants[0].params.theta_gap_minutes, 30);
    assert_eq!(experiment.variants[1].params.theta_gap_minutes, 60);
    assert_eq!(experiment.variants[3].params.retrieve_top_k, 1);
}

/// 单题失败不中断批量：FailingLlm 下全部题失败但返回 Ok 且逐题记录原因。
#[tokio::test]
async fn probe_run_single_failure_does_not_abort_batch() {
    let (app, _storage) = build_app_with_llm(Arc::new(FailingLlm::new()));
    let ds = build_dataset(&app, None, 2, 7, None).await;
    let experiment = build_experiment(
        &app,
        &ds,
        &PathBuf::from("dataset.json"),
        None,
        None,
        false,
        false,
    )
    .await
    .expect("单题失败不应中断批量（返回 Ok）");

    assert_eq!(experiment.variants.len(), 4, "全部档位仍执行");
    for variant in &experiment.variants {
        assert_eq!(
            variant.failed_count,
            variant.runs.len(),
            "所有题都应记录失败"
        );
        for run in &variant.runs {
            assert!(run.error.is_some(), "{} 应记录失败原因", run.item_id);
            assert!(run.error.as_deref().unwrap().contains("LLM 恒失败"));
            assert_eq!(run.reply, "", "失败时无回复");
        }
    }
}

/// --variants 过滤：只跑指定档位（无效 id 忽略）。
#[tokio::test]
async fn probe_run_variants_filter() {
    let (app, _storage) = build_test_app();
    let ds = build_dataset(&app, None, 1, 7, None).await;
    let experiment = build_experiment(
        &app,
        &ds,
        &PathBuf::from("dataset.json"),
        Some("baseline,top_k_1,nonexistent"),
        None,
        false,
        false,
    )
    .await
    .expect("档位过滤实验应成功");

    let ids: Vec<&str> = experiment
        .variants
        .iter()
        .map(|v| v.variant_id.as_str())
        .collect();
    assert_eq!(ids, vec!["baseline", "top_k_1"], "只应包含有效档位");
}

/// --limit：每档位最多跑指定题数。
#[tokio::test]
async fn probe_run_limit_truncates_items() {
    let (app, _storage) = build_test_app();
    let ds = build_dataset(&app, None, 10, 7, None).await; // 20 题
    let experiment = build_experiment(
        &app,
        &ds,
        &PathBuf::from("dataset.json"),
        None,
        Some(3),
        false,
        false,
    )
    .await
    .expect("limit 实验应成功");

    for variant in &experiment.variants {
        assert_eq!(variant.runs.len(), 3, "每档位只跑 limit=3 题");
    }
}

/// 实验结果序列化为合法 JSON（probe run --json 信封的数据面）。
#[tokio::test]
async fn probe_run_result_serializes_to_valid_json() {
    let (app, _storage) = build_test_app();
    let ds = build_dataset(&app, None, 1, 7, None).await;
    let experiment = build_experiment(
        &app,
        &ds,
        &PathBuf::from("dataset.json"),
        None,
        None,
        false,
        false,
    )
    .await
    .expect("实验应成功");
    let json = serde_json::to_value(&experiment).expect("实验结果必须可序列化");
    assert!(json["variants"].is_array());
    let first = &json["variants"][0];
    assert!(first["variant_id"].is_string());
    assert!(first["params"]["theta_gap_minutes"].is_number());
    assert!(first["runs"][0]["metrics"]["reply_chars"].is_number());
    assert!(first["runs"][0]["reply"].is_string());
    assert!(json["generated_at"].is_string());
}

// =========================================================
// 进程级测试：probe dataset alias + --json 信封（端到端）
// =========================================================

/// `probe dataset`（旧名 alias）可用，且空库时 `--json` 输出标准信封 + fixture 兜底。
#[test]
fn probe_dataset_alias_and_json_envelope() {
    let out = run_cli(&["probe", "dataset", "--json"]);
    assert_eq!(out.status.code(), Some(0), "probe dataset alias 应可用");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON");
    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON（M1 信封）");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    assert_eq!(parsed["data"]["source"], "fixture", "空库构建应降级夹具");
    assert_eq!(
        parsed["data"]["items"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        20,
        "默认 2 维 × 10 题"
    );
    assert_eq!(
        parsed["data"]["variants"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        4
    );
}

/// `--results` 文件缺失 → 业务校验失败 exit code=4（非通用失败 1），
/// 且错误信封携带 error.code=4（与结构非法 JSON 的 exit 4 一致）。
#[test]
fn probe_evaluate_missing_results_uses_validation_exit_code() {
    let out = run_cli(&[
        "probe",
        "evaluate",
        "--results",
        "/nonexistent/run.json",
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "实验结果文件缺失应归业务校验失败 exit 4"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("错误信封应为合法 JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], 4, "错误信封应携带 exit code 4");
}

/// `probe report --results` 文件缺失 → 同样 exit code=4。
#[test]
fn probe_report_missing_results_uses_validation_exit_code() {
    let out = run_cli(&[
        "probe",
        "report",
        "--results",
        "/nonexistent/run.json",
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "报告读取实验结果失败也应归业务校验失败 exit 4"
    );
}

/// `probe run --dataset` 文件缺失 → exit code=4（与注释声明的契约一致）。
#[test]
fn probe_run_missing_dataset_uses_validation_exit_code() {
    let out = run_cli(&[
        "probe",
        "run",
        "--dataset",
        "/nonexistent/probe.json",
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "数据集文件缺失应归业务校验失败 exit 4"
    );
}

/// `probe report --evaluation` 文件缺失 → exit code=4（需先提供有效 results）。
#[test]
fn probe_report_missing_evaluation_uses_validation_exit_code() {
    let dir = std::env::temp_dir().join(format!(
        "ramaria_probe_report_eval_missing_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let results = dir.join("results.json");
    std::fs::write(&results, minimal_experiment_json()).expect("写入结果文件失败");
    let out = run_cli(&[
        "probe",
        "report",
        "--results",
        results.to_str().unwrap(),
        "--evaluation",
        "/nonexistent/eval.json",
        "--json",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out.status.code(),
        Some(4),
        "评分数值文件缺失应归业务校验失败 exit 4"
    );
}

/// `probe report --calibration` 文件缺失 → exit code=4（需先提供有效 results）。
#[test]
fn probe_report_missing_calibration_uses_validation_exit_code() {
    let dir = std::env::temp_dir().join(format!(
        "ramaria_probe_report_calib_missing_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let results = dir.join("results.json");
    std::fs::write(&results, minimal_experiment_json()).expect("写入结果文件失败");
    let out = run_cli(&[
        "probe",
        "report",
        "--results",
        results.to_str().unwrap(),
        "--calibration",
        "/nonexistent/calib.json",
        "--json",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out.status.code(),
        Some(4),
        "校准文件缺失应归业务校验失败 exit 4"
    );
}

/// 最小可解析的 ProbeExperiment JSON（空档位，供 report 走到 evaluation/calibration 读取步骤）。
fn minimal_experiment_json() -> String {
    serde_json::json!({
        "dataset_file": "probe.json",
        "dataset_seed": 1,
        "persona_uid": "char-0001",
        "rebuild_utt": false,
        "variants": [],
        "generated_at": "2026-08-24T00:00:00Z"
    })
    .to_string()
}

/// 运行真实 ramaria 二进制（临时空 DB），返回进程输出。
fn run_cli(args: &[&str]) -> std::process::Output {
    use std::sync::atomic::{AtomicI64, Ordering};
    static DB_SEQ: AtomicI64 = AtomicI64::new(0);
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let db_dir =
        std::env::temp_dir().join(format!("ramaria_cli_probe_{}_{}", std::process::id(), seq));
    let _ = std::fs::create_dir_all(&db_dir);
    let db = db_dir.join("probe.db");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(&db)
        .output()
        .expect("运行 ramaria 二进制失败");
    let _ = std::fs::remove_dir_all(&db_dir);
    out
}
