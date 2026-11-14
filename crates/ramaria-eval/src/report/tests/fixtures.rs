//! crates/ramaria-eval/src/report/tests/fixtures.rs - Ramaria 评测报告测试夹具
//!
//! 设计特点:
//! - 夹具直接写 run.json / score.json / scored.jsonl / items.jsonl / usage.json
//! - 比例由调用方按题给出，统计断言可手算；判分失败题用 `None` 表示
//! - 条件轴、条件计数与逐题行全部由同一份条件清单派生，不写重复常量
//! - 只读写系统临时目录，不触真实评测产物与用户库
//! - 产物内容固定，不含时间戳，便于逐字节比较

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::score::{
    RULESET_VERSION, SCORE_ARTIFACT_KIND, SCORE_DIR, SCORE_FILE, SCORE_SCHEMA_VERSION, SCORED_FILE,
    USAGE_ARTIFACT_KIND, USAGE_FILE, USAGE_SCHEMA_VERSION,
};

/// 逐题查询时钟（固定值，产物不含时间戳）。
const AS_OF_MS: i64 = 1_765_000_000_000;

/// 一个条件的夹具定义。
pub(super) struct ConditionSpec {
    pub tier: &'static str,
    pub pipeline: &'static str,
    pub budget: u32,
    /// 逐题得分率；`None` 表示判分失败（不进配对）。
    pub ratios: Vec<Option<f64>>,
}

impl ConditionSpec {
    /// 条件标识（与运行器口径一致）。
    pub(super) fn id(&self) -> String {
        format!("{}/{}/{}/0", self.tier, self.pipeline, self.budget)
    }
}

/// 一次运行的夹具定义。
pub(super) struct RunSpec {
    pub run_id: &'static str,
    pub judge: bool,
    pub question_sample: Option<usize>,
    pub conditions: Vec<ConditionSpec>,
    pub with_score: bool,
    pub usage: bool,
}

impl RunSpec {
    /// 单条件运行的缺省夹具（layered，判分开启）。
    pub(super) fn layered(run_id: &'static str, ratios: Vec<Option<f64>>) -> Self {
        Self {
            run_id,
            judge: true,
            question_sample: Some(ratios.len()),
            conditions: vec![ConditionSpec {
                tier: "u1-y1",
                pipeline: "layered",
                budget: 800,
                ratios,
            }],
            with_score: true,
            usage: false,
        }
    }
}

// =========================================================
// 构造
// =========================================================

/// 临时根目录（每个用例独立）。
pub(super) fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("ramaria-eval-report-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    dir
}

/// 交替取两个值（构造有方差的配对差）。
pub(super) fn alternating(low: f64, high: f64, count: usize) -> Vec<Option<f64>> {
    (0..count)
        .map(|index| Some(if index % 2 == 0 { high } else { low }))
        .collect()
}

/// 常数序列。
pub(super) fn constant(value: f64, count: usize) -> Vec<Option<f64>> {
    (0..count).map(|_| Some(value)).collect()
}

/// 写入一次运行的完整夹具。
pub(super) fn write_run(root: &Path, spec: &RunSpec) {
    std::fs::create_dir_all(root).expect("运行根应可创建");
    write_state(root, spec);
    write_items(root, spec);
    if spec.with_score {
        let score_dir = root.join(SCORE_DIR);
        std::fs::create_dir_all(&score_dir).expect("判分目录应可创建");
        write_score(&score_dir, spec);
        write_scored(&score_dir, spec);
        if spec.usage {
            write_usage(&score_dir, spec);
        }
    }
}

/// 逐题产物摘要（夹具内固定）。
pub(super) fn items_digest(run_id: &str) -> String {
    format!("{:x}", Sha256::digest(run_id.as_bytes()))
}

// =========================================================
// run.json
// =========================================================

fn write_state(root: &Path, spec: &RunSpec) {
    let mut tiers: Vec<&str> = spec
        .conditions
        .iter()
        .map(|condition| condition.tier)
        .collect();
    tiers.sort_unstable();
    tiers.dedup();
    let mut pipelines: Vec<&str> = spec
        .conditions
        .iter()
        .map(|condition| condition.pipeline)
        .collect();
    pipelines.sort_unstable();
    pipelines.dedup();
    let mut budgets: Vec<u32> = spec
        .conditions
        .iter()
        .map(|condition| condition.budget)
        .collect();
    budgets.sort_unstable();
    budgets.dedup();

    let conditions: Vec<Value> = spec
        .conditions
        .iter()
        .map(|condition| {
            json!({
                "id": condition.id(),
                "total": condition.ratios.len(),
                "completed": condition.ratios.len(),
                "failed": 0
            })
        })
        .collect();
    let mut run = json!({
        "corpus": "lifebench",
        "language": "zh",
        "tiers": tiers,
        "pipelines": pipelines,
        "budgets": budgets
    });
    if let Some(sample) = spec.question_sample {
        run["question_sample"] = json!(sample);
    }
    let mut matrix = json!({
        "run": run,
        "answer_models": [{ "kind": "local", "base_url": "", "model": "answer-model" }]
    });
    if spec.judge {
        matrix["judge"] = json!({ "kind": "local", "base_url": "", "model": "judge-model" });
    }
    let state = json!({
        "artifact_kind": "ramaria_eval_run",
        "schema_version": 2,
        "run_id": spec.run_id,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "status": "completed",
        "matrix": matrix,
        "language": "zh",
        "tiers": [],
        "conditions": conditions,
        "elapsed_ms": 0,
        "warnings": []
    });
    let text = serde_json::to_string_pretty(&state).expect("运行状态应可序列化");
    std::fs::write(root.join("run.json"), text).expect("运行状态应可写");
}

// =========================================================
// items.jsonl
// =========================================================

fn write_items(root: &Path, spec: &RunSpec) {
    let mut body = String::new();
    for condition in &spec.conditions {
        for index in 0..condition.ratios.len() {
            let line = json!({
                "question_id": format!("q{index}"),
                "condition_id": condition.id(),
                "tier": condition.tier,
                "pipeline": condition.pipeline,
                "budget_chars": condition.budget,
                "answer_model": "answer-model",
                "answer_model_effective": "answer-model",
                "sample_key": "s1",
                "persona_uid": "lifebench-s1",
                "ask_time": "2025-12-02",
                "as_of_ms": AS_OF_MS,
                "question": "问题",
                "gold_answer": "答案",
                "gold_message_ids": ["m1"],
                "category": [0],
                "reply": "回复",
                "reply_chars": 2,
                "recall": null,
                "elapsed_ms": 10,
                "error": null,
                "attempts": 1,
                "conditions": {
                    "corpus_name": "lifebench",
                    "corpus_version": "2.0",
                    "language": "zh",
                    "corpus_sha256": "corpus-sha",
                    "memory_model": "memory-model",
                    "memory_model_kind": "local",
                    "memory_temperature": 0.2,
                    "memory_max_tokens": 512,
                    "embedding_fingerprint": "embed-fp",
                    "embedding_dimension": 512,
                    "prompt_template_version": "prompt-v1",
                    "schema_version": 4,
                    "index_status": "ready",
                    "device_os": "windows",
                    "device_arch": "x86_64",
                    "device_cores": 4
                }
            });
            body.push_str(&serde_json::to_string(&line).expect("逐题产物应可序列化"));
            body.push('\n');
        }
    }
    std::fs::write(root.join("items.jsonl"), body).expect("逐题产物应可写");
}

// =========================================================
// score.json
// =========================================================

fn write_score(score_dir: &Path, spec: &RunSpec) {
    let conditions: Vec<Value> = spec
        .conditions
        .iter()
        .map(|condition| {
            let judged_values: Vec<f64> = condition.ratios.iter().flatten().copied().collect();
            let judged = judged_values.len();
            let failed = condition.ratios.len() - judged;
            let micro = (judged > 0).then(|| judged_values.iter().sum::<f64>() / judged as f64);
            let full_hits = judged_values
                .iter()
                .filter(|ratio| (**ratio - 1.0).abs() < 1e-12)
                .count();
            let rubric = if spec.judge {
                json!({
                    "questions": condition.ratios.len(),
                    "judged": judged,
                    "micro_mean_ratio": micro,
                    "micro_full_hit_rate": (judged > 0).then(|| full_hits as f64 / judged as f64),
                    "macro_by_category": macro_by_category(judged, micro),
                    "disagreement_rate": 0.0
                })
            } else {
                Value::Null
            };
            let tone = if condition.pipeline == "layered" {
                2.5
            } else {
                2.1
            };
            json!({
                "condition_id": condition.id(),
                "questions": condition.ratios.len(),
                "scored": judged,
                "skipped_no_gold": 0,
                "failed_items": 0,
                "judge_failed": failed,
                "retrieval": {
                    "message": null,
                    "artifact": {
                        "questions": judged,
                        "coverage_pre": 0.5,
                        "coverage_post": 0.6,
                        "precision_pre": 0.10,
                        "precision_post": 0.12,
                        "mrr_pre": 0.30,
                        "mrr_post": 0.35,
                        "delta_coverage": 0.10,
                        "units_pre_avg": 10.0,
                        "units_post_avg": 8.0,
                        "missing_gold_total": 0
                    }
                },
                "credibility": {
                    "ua_answered": 30,
                    "ua_refusal_hits": 0,
                    "ua_refusal_rate": null,
                    "cd_questions": 30,
                    "cd_false_premise_accept_rate": null,
                    "ua_judge": if spec.judge {
                        json!({ "judged": 10, "hits": 7, "rate": 0.7, "disagreement_rate": 0.0 })
                    } else {
                        Value::Null
                    },
                    "cd_judge": if spec.judge {
                        json!({ "judged": 10, "hits": 4, "rate": 0.4, "disagreement_rate": 0.0 })
                    } else {
                        Value::Null
                    }
                },
                "rubric": rubric,
                "persona": if spec.judge {
                    json!({
                        "questions": judged,
                        "tone": tone,
                        "fact": 0.40,
                        "emotion": 0.60,
                        "fact_variant": "cosine+keyword",
                        "tone_failed": 0
                    })
                } else {
                    Value::Null
                },
                "missing": []
            })
        })
        .collect();
    let score = json!({
        "artifact_kind": SCORE_ARTIFACT_KIND,
        "schema_version": SCORE_SCHEMA_VERSION,
        "run_id": spec.run_id,
        "items_sha256": items_digest(spec.run_id),
        "ruleset_version": RULESET_VERSION,
        "judge": judge_json(spec.judge),
        "conditions": conditions,
        "missing": []
    });
    let text = serde_json::to_string_pretty(&score).expect("判分汇总应可序列化");
    std::fs::write(score_dir.join(SCORE_FILE), text).expect("判分汇总应可写");
}

/// 九类 Macro 明细：只有第 0 类有题。
fn macro_by_category(judged: usize, micro: Option<f64>) -> Vec<Value> {
    (0..=8)
        .map(|category| {
            if category == 0 {
                json!({ "category": category, "questions": judged, "mean_ratio": micro })
            } else {
                json!({ "category": category, "questions": 0, "mean_ratio": null })
            }
        })
        .collect()
}

/// 判分模型状态。
fn judge_json(enabled: bool) -> Value {
    if enabled {
        json!({
            "status": "configured",
            "model": "judge-model",
            "kind": "local",
            "source": "matrix",
            "votes": 3,
            "temperature": 0.0,
            "templates": [{ "task": "rubric_points", "sha256": "template-sha" }]
        })
    } else {
        json!({ "status": "disabled" })
    }
}

// =========================================================
// scored.jsonl
// =========================================================

fn write_scored(score_dir: &Path, spec: &RunSpec) {
    let mut body = String::new();
    for condition in &spec.conditions {
        for (index, ratio) in condition.ratios.iter().enumerate() {
            let rubric = if !spec.judge {
                Value::Null
            } else {
                match ratio {
                    Some(ratio) => json!({
                        "points": [{
                            "index": 0,
                            "description": "要点",
                            "weight": 10.0,
                            "hit": *ratio > 0.0,
                            "votes": [true, true, true],
                            "reason": null
                        }],
                        "score": ratio * 10.0,
                        "max": 10.0,
                        "ratio": ratio,
                        "full_hit": (ratio - 1.0).abs() < 1e-12,
                        "judge_failed": false
                    }),
                    None => json!({
                        "points": [],
                        "score": 0.0,
                        "max": 10.0,
                        "ratio": null,
                        "full_hit": false,
                        "judge_failed": true
                    }),
                }
            };
            let line = json!({
                "question_id": format!("q{index}"),
                "condition_id": condition.id(),
                "tier": condition.tier,
                "pipeline": condition.pipeline,
                "budget_chars": condition.budget,
                "answer_model": "answer-model",
                "sample_key": "s1",
                "category": [0],
                "gold_count": 1,
                "retrieval": null,
                "credibility": {
                    "ua_refusal_hint": null,
                    "cd_score_points": 0,
                    "ua_judge": null,
                    "cd_judge": null,
                    "judge_failed": false
                },
                "rubric": rubric,
                "persona": null,
                "notes": []
            });
            body.push_str(&serde_json::to_string(&line).expect("逐题判分应可序列化"));
            body.push('\n');
        }
    }
    std::fs::write(score_dir.join(SCORED_FILE), body).expect("逐题判分应可写");
}

// =========================================================
// usage.json
// =========================================================

fn write_usage(score_dir: &Path, spec: &RunSpec) {
    let conditions: Vec<Value> = spec
        .conditions
        .iter()
        .map(|condition| {
            json!({
                "condition_id": condition.id(),
                "answer": usage_json(30, 1_000, 100),
                "judge": usage_json(90, 3_000, 300)
            })
        })
        .collect();
    let total = json!({
        "answer": usage_json(30 * spec.conditions.len(), 1_000, 100),
        "judge": usage_json(90 * spec.conditions.len(), 3_000, 300)
    });
    let usage = json!({
        "artifact_kind": USAGE_ARTIFACT_KIND,
        "schema_version": USAGE_SCHEMA_VERSION,
        "run_id": spec.run_id,
        "items_sha256": items_digest(spec.run_id),
        "conditions": conditions,
        "total": total
    });
    let text = serde_json::to_string_pretty(&usage).expect("用量产物应可序列化");
    std::fs::write(score_dir.join(USAGE_FILE), format!("{text}\n")).expect("用量产物应可写");
}

/// 一路用量。
fn usage_json(calls: usize, prompt: u32, completion: u32) -> Value {
    json!({
        "calls": calls,
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "cache_hit_tokens": prompt / 2,
        "cache_miss_tokens": prompt - prompt / 2,
        "cache_hit_rate": 0.5
    })
}
