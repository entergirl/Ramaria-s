//! crates/ramaria-eval/src/run/matrix/tests.rs - Ramaria 评测矩阵单元测试
//!
//! 设计特点:
//! - TOML 夹具落到临时文件，覆盖解析、缺省值与路径化报错
//! - 条件展开按四轴顺序逐一比对标识，并锁定唯一性
//! - 八种管线的稳定标识、序列化与掩码逐项断言
//! - 校验失败覆盖空轴、零预算、零题限与云端缺项

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_core::error::RamariaError;

use super::*;

// =========================================================
// 夹具
// =========================================================

const MINIMAL: &str = r#"
[run]
corpus = "lifebench"
language = "zh"
tiers = ["u1-m1"]
pipelines = ["layered"]
budgets = [400]

[[answer_models]]
kind = "local"
base_url = "http://127.0.0.1:1234/v1"
model = "google/gemma-4-e4b"
"#;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-matrix-{tag}-{nanos}"))
}

fn write_matrix(dir: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("临时目录应可创建");
    let path = dir.join("matrix.toml");
    std::fs::write(&path, text).expect("矩阵夹具应可写");
    path
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn load_text(dir: &Path, text: &str) -> RamariaResult<RunMatrix> {
    let path = write_matrix(dir, text);
    RunMatrix::load(&path)
}

// =========================================================
// 解析与缺省
// =========================================================

/// 必填轴齐全即可加载，可选字段取缺省值。
#[test]
fn load_reads_axes_and_defaults() {
    let dir = temp_dir("defaults");
    let matrix = load_text(&dir, MINIMAL).expect("矩阵应可加载");

    assert_eq!(matrix.run.corpus, "lifebench");
    assert_eq!(matrix.run.language, "zh");
    assert_eq!(matrix.run.tiers, vec!["u1-m1".to_string()]);
    assert_eq!(matrix.run.pipelines, vec![Pipeline::Layered]);
    assert_eq!(matrix.run.budgets, vec![400]);
    assert_eq!(matrix.run.question_limit, None, "缺省不限制题量");
    assert_eq!(matrix.run.question_sample, None, "缺省不抽样");
    assert!(!matrix.run.strict_isolation, "缺省非严格隔离");
    assert_eq!(matrix.run.retry_limit, DEFAULT_RETRY_LIMIT);
    assert_eq!(matrix.answer_models.len(), 1);
    assert_eq!(matrix.answer_models[0].kind, AnswerModelKind::Local);
    cleanup(&dir);
}

/// 显式选项覆盖缺省值，重试次数允许 0。
#[test]
fn load_applies_explicit_options() {
    let dir = temp_dir("options");
    let text = MINIMAL.replace(
        "budgets = [400]",
        "budgets = [400]\nquestion_limit = 20\nstrict_isolation = true\nretry_limit = 0",
    );
    let matrix = load_text(&dir, &text).expect("矩阵应可加载");

    assert_eq!(matrix.run.question_limit, Some(20));
    assert!(matrix.run.strict_isolation);
    assert_eq!(matrix.run.retry_limit, 0);
    cleanup(&dir);
}

/// 抽样预算可单独声明；与题量上限互斥，零预算被拒绝。
#[test]
fn question_sample_parses_and_conflicts_with_limit() {
    let dir = temp_dir("sample-ok");
    let text = MINIMAL.replace("budgets = [400]", "budgets = [400]\nquestion_sample = 300");
    let matrix = load_text(&dir, &text).expect("抽样预算应可加载");
    assert_eq!(matrix.run.question_sample, Some(300));
    assert_eq!(matrix.run.question_limit, None);
    cleanup(&dir);

    let dir = temp_dir("sample-zero");
    let path = write_matrix(
        &dir,
        &toml_with(
            "[\"u1-m1\"]",
            "[\"layered\"]",
            "[400]",
            "question_sample = 0",
        ),
    );
    let error = RunMatrix::load(&path).expect_err("零预算应报错");
    assert!(
        error.to_string().contains("run.question_sample 必须大于 0"),
        "文案应指明字段，实际 {error}"
    );
    cleanup(&dir);

    let dir = temp_dir("sample-conflict");
    let path = write_matrix(
        &dir,
        &toml_with(
            "[\"u1-m1\"]",
            "[\"layered\"]",
            "[400]",
            "question_limit = 20\nquestion_sample = 300",
        ),
    );
    let error = RunMatrix::load(&path).expect_err("两个选项同时出现应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("run.question_sample 与 run.question_limit 不能同时设置"),
        "文案应指明互斥字段，实际 {error}"
    );
    cleanup(&dir);
}

/// 未知管线取值被拒绝，错误文案带文件路径。
#[test]
fn load_rejects_unknown_pipeline_with_path() {
    let dir = temp_dir("unknown-pipeline");
    let text = MINIMAL.replace("pipelines = [\"layered\"]", "pipelines = [\"bogus\"]");
    let path = write_matrix(&dir, &text);
    let error = RunMatrix::load(&path).expect_err("未知管线应报错");

    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains(&path.display().to_string()),
        "文案应含文件路径，实际 {error}"
    );
    cleanup(&dir);
}

// =========================================================
// 校验
// =========================================================

/// 空轴、零预算与零题限均在加载阶段被拒绝。
#[test]
fn load_rejects_invalid_axes() {
    let cases = [
        (
            "run.tiers 不能为空",
            toml_with("[]", "[\"layered\"]", "[400]", ""),
        ),
        (
            "run.pipelines 不能为空",
            toml_with("[\"u1-m1\"]", "[]", "[400]", ""),
        ),
        (
            "run.budgets 不能为空",
            toml_with("[\"u1-m1\"]", "[\"layered\"]", "[]", ""),
        ),
        (
            "run.budgets 每项必须大于 0",
            toml_with("[\"u1-m1\"]", "[\"layered\"]", "[0, 400]", ""),
        ),
        (
            "run.question_limit 必须大于 0",
            toml_with(
                "[\"u1-m1\"]",
                "[\"layered\"]",
                "[400]",
                "question_limit = 0",
            ),
        ),
    ];

    for (index, (expected, text)) in cases.iter().enumerate() {
        let dir = temp_dir(&format!("axes-{index}"));
        let path = write_matrix(&dir, text);
        let error = RunMatrix::load(&path).expect_err("非法轴应报错");

        assert!(
            matches!(&error, RamariaError::Validation { .. }),
            "应为 Validation，实际 {error:?}"
        );
        assert!(
            error.to_string().contains(expected),
            "文案应含 {expected}，实际 {error}"
        );
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "文案应含文件路径，实际 {error}"
        );
        cleanup(&dir);
    }
}

/// 拼一个只含必填轴的矩阵 TOML。
fn toml_with(tiers: &str, pipelines: &str, budgets: &str, extra: &str) -> String {
    format!(
        "[run]\ncorpus = \"lifebench\"\nlanguage = \"zh\"\ntiers = {tiers}\npipelines = {pipelines}\nbudgets = {budgets}\n{extra}\n\n[[answer_models]]\nkind = \"local\"\nbase_url = \"http://127.0.0.1:1234/v1\"\nmodel = \"m\"\n"
    )
}

/// 云端模型缺地址或模型标识被拒绝，本地模型允许留空。
#[test]
fn cloud_model_requires_address_and_model() {
    let cloud = r#"
[run]
corpus = "lifebench"
language = "zh"
tiers = ["u1-m1"]
pipelines = ["layered"]
budgets = [400]

[[answer_models]]
kind = "cloud"
base_url = ""
model = "cloud-model"
"#;
    let dir = temp_dir("cloud-url");
    let error = load_text(&dir, cloud).expect_err("云端缺地址应报错");
    assert!(error.to_string().contains("answer_models[0].base_url"));
    cleanup(&dir);

    let dir = temp_dir("cloud-model");
    let text = cloud
        .replace(
            "base_url = \"\"",
            "base_url = \"https://api.example.com/v1\"",
        )
        .replace("model = \"cloud-model\"", "model = \"\"");
    let error = load_text(&dir, &text).expect_err("云端缺模型应报错");
    assert!(error.to_string().contains("answer_models[0].model"));
    cleanup(&dir);

    let dir = temp_dir("local-empty");
    let text = MINIMAL.replace("model = \"google/gemma-4-e4b\"", "model = \"\"");
    assert!(
        load_text(&dir, &text).is_ok(),
        "本地模型允许留空，由服务端自动选择"
    );
    cleanup(&dir);
}

// =========================================================
// 判分模型
// =========================================================

/// `[judge]` 段按答题模型口径解析，缺省时不产出判分模型。
#[test]
fn judge_section_parses_with_answer_model_contract() {
    let dir = temp_dir("judge-ok");
    let text = MINIMAL.replace(
        "model = \"google/gemma-4-e4b\"",
        "model = \"google/gemma-4-e4b\"\n\n[judge]\nkind = \"cloud\"\nbase_url = \"https://api.example.com/v1\"\nmodel = \"judge-model\"",
    );
    let matrix = load_text(&dir, &text).expect("带判分模型的矩阵应可加载");
    let judge = matrix.judge.expect("judge 段应被解析");
    assert_eq!(judge.kind, AnswerModelKind::Cloud);
    assert_eq!(judge.base_url, "https://api.example.com/v1");
    assert_eq!(judge.model, "judge-model");
    cleanup(&dir);

    let dir = temp_dir("judge-none");
    let matrix = load_text(&dir, MINIMAL).expect("矩阵应可加载");
    assert!(matrix.judge.is_none(), "缺省不声明判分模型");
    cleanup(&dir);
}

/// 云端判分模型缺地址或模型标识被拒绝，文案带字段名。
#[test]
fn cloud_judge_requires_address_and_model() {
    let cloud = MINIMAL.replace(
        "model = \"google/gemma-4-e4b\"",
        "model = \"google/gemma-4-e4b\"\n\n[judge]\nkind = \"cloud\"\nbase_url = \"\"\nmodel = \"judge-model\"",
    );
    let dir = temp_dir("judge-url");
    let error = load_text(&dir, &cloud).expect_err("云端判分模型缺地址应报错");
    assert!(
        error.to_string().contains("judge.base_url"),
        "文案应含字段名，实际 {error}"
    );
    cleanup(&dir);

    let dir = temp_dir("judge-model");
    let text = cloud
        .replace(
            "base_url = \"\"",
            "base_url = \"https://api.example.com/v1\"",
        )
        .replace("model = \"judge-model\"", "model = \"\"");
    let error = load_text(&dir, &text).expect_err("云端判分模型缺模型应报错");
    assert!(
        error.to_string().contains("judge.model"),
        "文案应含字段名，实际 {error}"
    );
    cleanup(&dir);
}

// =========================================================
// 条件展开
// =========================================================

/// 条件按档位 → 管线 → 预算 → 模型顺序展开，标识唯一。
#[test]
fn conditions_expand_in_axis_order() {
    let dir = temp_dir("conditions");
    let text = r#"
[run]
corpus = "lifebench"
language = "zh"
tiers = ["u1-m1", "u1-q1"]
pipelines = ["layered", "flat-rag"]
budgets = [400, 800]
question_limit = 5
strict_isolation = false
retry_limit = 2

[[answer_models]]
kind = "local"
base_url = "http://127.0.0.1:1234/v1"
model = "local-a"

[[answer_models]]
kind = "cloud"
base_url = "https://api.example.com/v1"
model = "cloud-b"
"#;
    let matrix = load_text(&dir, text).expect("矩阵应可加载");
    let conditions = matrix.conditions();

    let ids: Vec<&str> = conditions
        .iter()
        .map(|condition| condition.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "u1-m1/layered/400/0",
            "u1-m1/layered/400/1",
            "u1-m1/layered/800/0",
            "u1-m1/layered/800/1",
            "u1-m1/flat-rag/400/0",
            "u1-m1/flat-rag/400/1",
            "u1-m1/flat-rag/800/0",
            "u1-m1/flat-rag/800/1",
            "u1-q1/layered/400/0",
            "u1-q1/layered/400/1",
            "u1-q1/layered/800/0",
            "u1-q1/layered/800/1",
            "u1-q1/flat-rag/400/0",
            "u1-q1/flat-rag/400/1",
            "u1-q1/flat-rag/800/0",
            "u1-q1/flat-rag/800/1",
        ]
    );
    assert_eq!(
        conditions
            .iter()
            .map(|condition| condition.id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        ids.len(),
        "条件标识应唯一"
    );

    let first = &conditions[0];
    assert_eq!(first.tier, "u1-m1");
    assert_eq!(first.pipeline, Pipeline::Layered);
    assert_eq!(first.budget_chars, 400);
    assert_eq!(first.answer_model.kind, AnswerModelKind::Local);
    let second = &conditions[1];
    assert_eq!(second.answer_model.kind, AnswerModelKind::Cloud);
    cleanup(&dir);
}

// =========================================================
// 管线标识与掩码
// =========================================================

/// 矩阵文件不允许声明 oracle 管线：它只能由 eval oracle 生成。
#[test]
fn load_rejects_oracle_pipeline() {
    let dir = temp_dir("oracle");
    let error = load_text(
        &dir,
        &toml_with("[\"u1-m1\"]", "[\"oracle-l1\"]", "[400]", ""),
    )
    .expect_err("oracle 管线应被拒绝");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("oracle-l1"),
        "文案应指明管线，实际 {error}"
    );
    cleanup(&dir);
}

/// 八种管线的稳定标识、序列化与反序列化一致。
#[test]
fn pipeline_ids_roundtrip() {
    for (pipeline, id, _) in pipeline_cases() {
        assert_eq!(pipeline.as_str(), id, "稳定标识不符");
        let json = serde_json::to_string(&pipeline).expect("管线应可序列化");
        assert_eq!(json, format!("\"{id}\""));
        let back: Pipeline = serde_json::from_str(&json).expect("管线应可反序列化");
        assert_eq!(back, pipeline);
    }
}

/// 八种管线的掩码逐字段匹配变体语义。
#[test]
fn pipeline_masks_match_variants() {
    for (pipeline, id, expected) in pipeline_cases() {
        assert_eq!(
            mask_flags(pipeline.mask()),
            expected,
            "管线 {id} 的掩码不符"
        );
    }
}

/// 掩码字段顺序：l1 / l2 / l3 / utt / knowledge / narrative / behavior / style / examples。
fn mask_flags(mask: EvalInjectionMask) -> [bool; 9] {
    [
        mask.l1,
        mask.l2,
        mask.l3,
        mask.utt,
        mask.knowledge,
        mask.narrative,
        mask.behavior,
        mask.style,
        mask.examples,
    ]
}

fn pipeline_cases() -> [(Pipeline, &'static str, [bool; 9]); 8] {
    [
        (Pipeline::Layered, "layered", [true; 9]),
        (
            Pipeline::L1,
            "l1",
            [true, false, false, false, false, false, false, false, false],
        ),
        (
            Pipeline::L1L2,
            "l1-l2",
            [true, true, false, false, false, false, false, false, false],
        ),
        (
            Pipeline::L1L2L3,
            "l1-l2-l3",
            [true, true, true, false, false, false, false, false, false],
        ),
        (
            Pipeline::L1L2L3Utt,
            "l1-l2-l3-utt",
            [true, true, true, true, false, false, false, false, false],
        ),
        (
            Pipeline::FlatRag,
            "flat-rag",
            [false, false, false, true, false, false, false, false, false],
        ),
        (Pipeline::FullContext, "full-context", [false; 9]),
        (Pipeline::OracleL1, "oracle-l1", [false; 9]),
    ]
}
