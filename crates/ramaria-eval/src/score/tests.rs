//! crates/ramaria-eval/src/score/tests.rs - Ramaria 评测判分单元测试
//!
//! 设计特点:
//! - 检索指标用闭包解析器锁定覆盖率与名次口径，不依赖库
//! - 判分入口用临时运行根锁定幂等、缺库降级与产物结构
//! - 用量观测（累加 / 命中率 / 摘要文案）单独放在 usage 子模块
//! - 夹具直接写 `run.json` / `items.jsonl`，同时覆盖旧版运行状态读取
//! - 不读写真实评测产物与用户库

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ramaria_core::traits::LlmProvider;
use ramaria_testkit::MockLlm;

use ramaria_service::{
    BudgetTrace, ChannelCandidate, EvalRecallItem, EvalRecallObservation, FusedEntryTrace,
    RetrievalTrace, UttTrace,
};
use serde_json::json;

use crate::baseline::{ArtifactMap, QrelsScorePoint};
use crate::run::{ConditionLine, RunItemRecord};

use super::artifact::{MISSING_PRESET_RECALL, MISSING_WORK_DB, SCORE_FILE, SCORED_FILE};
use super::judge::Judge;
use super::retrieval::{measure_item, measure_units};
use super::rubric::RubricSums;
use super::*;
use crate::run::AnswerModelKind;

mod concurrency;
mod order;
mod usage;

// =========================================================
// 夹具
// =========================================================

const CONDITION: &str = "u1-m1/layered/400/0";

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("ramaria-eval-score-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    dir
}

fn condition_line() -> ConditionLine {
    ConditionLine {
        corpus_name: "lifebench".to_string(),
        corpus_version: "2.0".to_string(),
        language: "zh".to_string(),
        corpus_sha256: "corpus-sha".to_string(),
        memory_model: "memory-model".to_string(),
        memory_model_kind: "local".to_string(),
        memory_temperature: 0.2,
        memory_max_tokens: 512,
        embedding_fingerprint: "embed-fp".to_string(),
        embedding_dimension: Some(512),
        prompt_template_version: "prompt-v1".to_string(),
        schema_version: 4,
        index_status: "ready".to_string(),
        device_os: std::env::consts::OS.to_string(),
        device_arch: std::env::consts::ARCH.to_string(),
        device_cores: 1,
    }
}

/// 观测侧分层标签：取不到落层时按 graph 记（与轨迹口径一致）。",
fn layer_of(label: &str) -> String {
    retrieval::parse_label(label)
        .map(|(layer, _)| layer.to_string())
        .unwrap_or_else(|| "graph".to_string())
}

/// 构造一条召回观测：预算前融合结果 + 预算后注入条目 + utt 候选。
fn observation(before: &[&str], after: &[&str], utt: &[&str]) -> EvalRecallObservation {
    EvalRecallObservation {
        items: after
            .iter()
            .map(|id| EvalRecallItem {
                layer: layer_of(id),
                id: (*id).to_string(),
                score: 1.0,
            })
            .collect(),
        trace: ramaria_service::RecallTrace {
            query_chars: 4,
            query_vector_dim: None,
            retrieval: RetrievalTrace {
                channels: Vec::new(),
                fused: before
                    .iter()
                    .map(|id| FusedEntryTrace {
                        id: (*id).to_string(),
                        layer: layer_of(id),
                        rrf_score: 0.5,
                        decay_factor: 1.0,
                        decayed_score: 0.5,
                        vector_raw: None,
                        bm25_raw: None,
                        graph_raw: None,
                        keyword_raw: None,
                    })
                    .collect(),
                dropped: Vec::new(),
            },
            dropped: Vec::new(),
            budget: BudgetTrace::default(),
            utt: UttTrace {
                active: !utt.is_empty(),
                candidates: utt
                    .iter()
                    .enumerate()
                    .map(|(index, id)| ChannelCandidate {
                        id: (*id).to_string(),
                        raw_score: 1.0,
                        rank: index + 1,
                    })
                    .collect(),
                rendered_chars: if utt.is_empty() { 0 } else { 120 },
                budget_chars: 1_000,
            },
        },
        context_chars: 20,
        injected_tokens: 8,
        utt_chars: 0,
    }
}

fn record(
    question: &str,
    gold: &[&str],
    reply: Option<&str>,
    recall: Option<EvalRecallObservation>,
) -> RunItemRecord {
    RunItemRecord {
        question_id: question.to_string(),
        condition_id: CONDITION.to_string(),
        tier: "u1-m1".to_string(),
        pipeline: "layered".to_string(),
        budget_chars: 400,
        answer_model: "answer-model".to_string(),
        answer_model_effective: "answer-model".to_string(),
        sample_key: "s1".to_string(),
        persona_uid: "lifebench-s1".to_string(),
        ask_time: "2025-12-02".to_string(),
        as_of_ms: 1_765_000_000_000,
        question: "问题".to_string(),
        gold_answer: "答案".to_string(),
        gold_message_ids: gold.iter().map(|id| (*id).to_string()).collect(),
        evidence: Vec::new(),
        question_type: Vec::new(),
        score_points: vec![QrelsScorePoint {
            description: "要点".to_string(),
            score: 10.0,
        }],
        category: vec![0],
        reply: reply.map(str::to_string),
        reply_chars: reply.map(|text| text.chars().count()).unwrap_or(0),
        recall,
        preset_chars: None,
        usage: None,
        elapsed_ms: 10,
        error: None,
        attempts: 1,
        conditions: condition_line(),
    }
}

fn failure_record(question: &str, gold: &[&str]) -> RunItemRecord {
    let mut record = record(question, gold, None, None);
    record.error = Some("模型不可达".to_string());
    record
}

/// 写一份旧版运行状态（schema 1，矩阵无 judge 段）与逐题产物。
fn write_run(dir: &Path, records: &[RunItemRecord]) {
    write_run_with_tiers(dir, records, json!([]));
}

/// 同上，但显式给出运行状态里的档位引用。
fn write_run_with_tiers(dir: &Path, records: &[RunItemRecord], tiers: serde_json::Value) {
    // 条件计数由逐题产物推导：同一条件多行只记一次，条目数取该条件的题目数。
    let mut conditions: Vec<serde_json::Value> = Vec::new();
    for record in records {
        match conditions
            .iter_mut()
            .find(|entry| entry["id"] == json!(record.condition_id))
        {
            Some(entry) => {
                let total = entry["total"].as_u64().unwrap_or(0) + 1;
                entry["total"] = json!(total);
            }
            None => conditions.push(json!({
                "id": record.condition_id,
                "total": 1,
                "completed": 0,
                "failed": 0
            })),
        }
    }
    let state = json!({
        "artifact_kind": "ramaria_eval_run",
        "schema_version": 1,
        "run_id": "run-test",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "status": "completed",
        "matrix": {
            "run": {
                "corpus": "lifebench",
                "language": "zh",
                "tiers": ["u1-m1"],
                "pipelines": ["layered"],
                "budgets": [400]
            },
            "answer_models": [{ "kind": "local", "base_url": "", "model": "m" }]
        },
        "language": "zh",
        "tiers": tiers,
        "conditions": conditions,
        "elapsed_ms": 0,
        "warnings": []
    });
    let text = serde_json::to_string_pretty(&state).expect("运行状态应可序列化");
    std::fs::write(dir.join("run.json"), text).expect("运行状态应可写");

    let mut body = String::new();
    for record in records {
        body.push_str(&serde_json::to_string(record).expect("逐题产物应可序列化"));
        body.push('\n');
    }
    std::fs::write(dir.join("items.jsonl"), body).expect("逐题产物应可写");
}

fn gold(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn labels(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

// =========================================================
// 检索指标
// =========================================================

/// 全部 gold 都被首个单元覆盖：覆盖率 1、首个命中名次 1。
#[test]
fn coverage_counts_full_hits() {
    let gold = gold(&["m1", "m2"]);
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m1", "m2"])),
        _ => None,
    };

    let metrics = measure_units(&labels(&["L1:a"]), &gold, &mut resolver);
    assert_eq!(metrics.coverage, 1.0);
    assert_eq!(metrics.first_hit_rank, Some(1));
    assert_eq!(metrics.units, 1);
    assert_eq!(metrics.unmappable, 0);
}

/// 部分命中：覆盖率按 gold 被覆盖比例算，名次取第一个相交单元。
#[test]
fn coverage_counts_partial_hits_with_rank() {
    let gold = gold(&["m1", "m2", "m3"]);
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m9"])),
        "b" => Some(labels(&["m2"])),
        "c" => Some(labels(&["m1"])),
        _ => None,
    };

    let metrics = measure_units(&labels(&["L1:a", "L2:b", "L1:c"]), &gold, &mut resolver);
    assert!((metrics.coverage - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(metrics.first_hit_rank, Some(2), "首个相交单元是第 2 个");
}

/// 全不命中：覆盖率为 0，名次为空且倒数为 0。
#[test]
fn coverage_counts_misses_as_zero() {
    let gold = gold(&["m1"]);
    let mut resolver = |_layer: &str, _id: &str| Some(labels(&["m9"]));

    let metrics = measure_units(&labels(&["L1:a", "L2:b"]), &gold, &mut resolver);
    assert_eq!(metrics.coverage, 0.0);
    assert_eq!(metrics.first_hit_rank, None);
    assert_eq!(metrics.units, 2);
}

/// 不可映射单元只计数，不进入覆盖率分子分母；空 gold 题不参与检索指标。
#[test]
fn unmappable_units_and_empty_gold_skip() {
    let gold = gold(&["m1"]);
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m1"])),
        _ => None,
    };
    let metrics = measure_units(
        &labels(&["graph:加班", "L3:7", "L1:", "L2:9", "L1:a"]),
        &gold,
        &mut resolver,
    );
    assert_eq!(metrics.coverage, 1.0, "不可映射单元不影响覆盖率");
    assert_eq!(metrics.unmappable, 4, "前缀不可解析与库侧解析失败都计入");
    assert_eq!(metrics.first_hit_rank, Some(5));

    let empty = record(
        "q-empty",
        &[],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    assert!(
        measure_item(&empty, &mut resolver).is_none(),
        "空 gold 题不参与检索指标"
    );
    let failed = failure_record("q-failed", &["m1"]);
    assert!(
        measure_item(&failed, &mut resolver).is_none(),
        "失败题无召回观测，不参与检索指标"
    );
}

/// 预算前取融合顺序、预算后取注入条目再接 utt 候选。
#[test]
fn post_set_appends_utt_candidates() {
    let mut resolver = |_layer: &str, id: &str| match id {
        "1" => Some(labels(&["m3"])),
        _ => None,
    };
    let record = record(
        "q1",
        &["m1", "m3"],
        Some("回复"),
        Some(observation(&["L2:1"], &[], &["L0:1"])),
    );

    let metrics = measure_item(&record, &mut resolver).expect("应参与检索指标");
    assert_eq!(metrics.coverage_pre, 0.5);
    assert_eq!(metrics.rank_pre, Some(1));
    assert_eq!(metrics.coverage_post, 0.5, "预算后集合只含 utt 候选");
    assert_eq!(metrics.rank_post, Some(1));
    assert_eq!(metrics.units_post, 1);
}

// =========================================================
// 判分入口
// =========================================================

/// 缺工作库：不报错，检索列留 null 并记缺项，其余列照常产出。
///
/// 可信度列只认题型：不可回答题进 UA 分母，冲突题进 CD 分母，其余题两类都不计。
#[tokio::test]
async fn missing_work_db_degrades_retrieval() {
    let dir = temp_dir("no-db");
    let mut records = vec![
        record(
            "q2",
            &[],
            Some("她在家里"),
            Some(observation(&[], &[], &[])),
        ),
        failure_record("q3", &["m2"]),
        record(
            "q4",
            &["m1"],
            Some("回复"),
            Some(observation(&["L1:a"], &["L1:a"], &[])),
        ),
        record(
            "q5",
            &[],
            Some("我不知道"),
            Some(observation(&[], &[], &[])),
        ),
    ];
    records[0].category = vec![4];
    records[2].category = vec![7];
    records[3].category = vec![4];
    write_run(&dir, &records);

    let (report, _usage) = run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("缺工作库不应报错");
    assert_eq!(report.run_id, "run-test");
    assert_eq!(report.judge, JudgeStatus::Disabled);
    assert_eq!(report.conditions.len(), 1);

    let condition = &report.conditions[0];
    assert_eq!(condition.questions, 4);
    assert_eq!(condition.scored, 1, "只有有 gold 且未成功的题参与判分");
    assert_eq!(condition.skipped_no_gold, 2);
    assert_eq!(condition.failed_items, 1);
    assert!(condition.retrieval.message.is_none(), "缺库时检索列留 null");
    assert_eq!(condition.credibility.ua_answered, 2, "只统计不可回答题");
    assert_eq!(condition.credibility.ua_refusal_hits, 1);
    assert_eq!(condition.credibility.ua_refusal_rate, Some(0.5));
    assert_eq!(condition.credibility.cd_questions, 1, "只统计冲突题");
    assert!(condition.credibility.cd_false_premise_accept_rate.is_none());
    assert!(
        condition
            .missing
            .iter()
            .any(|entry| entry.kind == MISSING_WORK_DB),
        "应记工作库缺项"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 工作库存在：检索列照常产出，空库下单元可映射但覆盖为空。
#[tokio::test]
async fn work_db_present_scores_message_retrieval() {
    let dir = temp_dir("with-db");
    let db = dir.join("work").join("ramaria.db");
    std::fs::create_dir_all(db.parent().expect("工作库应有父目录")).expect("目录应可创建");
    let engine = ramaria_service::Engine::open(&db)
        .await
        .expect("工作库应可装配");
    drop(engine);

    let records = vec![record(
        "q1",
        &["m1"],
        Some("回复"),
        Some(observation(
            &["L1:0b0f0f8e-0000-4000-8000-000000000001"],
            &["L2:42"],
            &[],
        )),
    )];
    write_run(&dir, &records);

    let (report, _usage) = run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("有库时判分应成功");
    let condition = &report.conditions[0];
    let message = condition
        .retrieval
        .message
        .as_ref()
        .expect("应产出消息级指标");
    assert_eq!(message.questions, 1);
    assert_eq!(message.coverage_pre, 0.0, "空库下单元覆盖为空");
    assert_eq!(message.coverage_post, 0.0);
    assert_eq!(message.mrr_pre, 0.0);
    assert_eq!(message.unmappable_total, 0, "空库单元仍可映射");
    assert!(message.delta_coverage.abs() < 1e-12);
    assert!(
        condition
            .missing
            .iter()
            .any(|entry| entry.kind == artifact::MISSING_ARTIFACT),
        "缺映射应记 artifact 缺项"
    );
    assert!(
        condition.retrieval.artifact.is_none(),
        "缺映射时 artifact 列留 null"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一输入两次判分：两份产物逐字节一致，且逐题行保持末次出现顺序。
#[tokio::test]
async fn repeated_score_is_byte_identical() {
    let dir = temp_dir("idempotent");
    let first = record(
        "q1",
        &["m1"],
        Some("回复"),
        Some(observation(&["L1:a"], &["L1:a"], &[])),
    );
    // 同一复合键追加一行失败行：判分以末行为准，且保持末次出现顺序。
    let mut again = first.clone();
    again.reply = None;
    again.recall = None;
    again.error = Some("模型不可达".to_string());
    let second = record(
        "q2",
        &["m2"],
        Some("回复"),
        Some(observation(&["L1:b"], &["L1:b"], &[])),
    );
    again.score_points.clear();
    write_run(&dir, &[first, again, second]);

    run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("首次判分应成功");
    let score_dir = dir.join(SCORE_DIR);
    let first_score = std::fs::read(score_dir.join(SCORE_FILE)).expect("应写出汇总");
    let first_scored = std::fs::read(score_dir.join(SCORED_FILE)).expect("应写出逐题");

    run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("再次判分应成功");
    let second_score = std::fs::read(score_dir.join(SCORE_FILE)).expect("应写出汇总");
    let second_scored = std::fs::read(score_dir.join(SCORED_FILE)).expect("应写出逐题");
    assert_eq!(first_score, second_score, "两次判分的汇总应逐字节一致");
    assert_eq!(first_scored, second_scored, "两次判分的逐题应逐字节一致");

    let lines: Vec<&str> = std::str::from_utf8(&first_scored)
        .expect("逐题产物应为 UTF-8")
        .lines()
        .collect();
    assert_eq!(lines.len(), 2, "同一复合键只保留末行");
    let items: Vec<ScoredItem> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("逐题行应可解析"))
        .collect();
    assert_eq!(items[0].question_id, "q1");
    assert_eq!(items[1].question_id, "q2");
    assert!(
        items[0].notes.iter().any(|note| note == "旧产物缺分项原料"),
        "缺分项原料应写提示"
    );

    let report: ScoreReport = serde_json::from_slice(&first_score).expect("汇总应可反序列化");
    assert_eq!(report.items_sha256.len(), 64, "应是 sha256 十六进制");

    let value: serde_json::Value = serde_json::from_slice(&first_score).expect("汇回应为 JSON");
    assert_eq!(value["artifact_kind"], SCORE_ARTIFACT_KIND);
    assert_eq!(value["judge"]["status"], "disabled", "本批只出未启用");
    assert!(
        value["conditions"][0]["retrieval"]["artifact"].is_null(),
        "artifact 口径留空待补"
    );
    assert_eq!(report.ruleset_version, RULESET_VERSION);
    assert!(
        report
            .missing
            .iter()
            .any(|entry| entry.kind == artifact::MISSING_JUDGE),
        "应记 judge 未接入缺项"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// artifact 级指标
// =========================================================

/// 构造一份档位映射夹具（样本键 → artifact → 消息 id）。
fn artifact_map(samples: serde_json::Value) -> ArtifactMap {
    serde_json::from_value(json!({
        "artifact_kind": crate::baseline::artifacts::ARTIFACT_KIND,
        "schema_version": crate::baseline::artifacts::ARTIFACT_SCHEMA_VERSION,
        "tier_id": "u1-m1",
        "corpus": { "name": "lifebench", "version": "2.0", "sha256": "corpus-sha" },
        "samples": samples
    }))
    .expect("映射夹具应可构造")
}

/// 一条带 evidence 的记录。
fn record_with_evidence(question: &str, evidence: &[&str], gold: &[&str]) -> RunItemRecord {
    let mut record = record(
        question,
        gold,
        Some("回复"),
        Some(observation(&["L1:a"], &["L1:a"], &[])),
    );
    record.evidence = evidence.iter().map(|item| (*item).to_string()).collect();
    record
}

/// gold artifact 全被首个单元覆盖：覆盖率与名次取满值，缺失为 0。
#[tokio::test]
async fn artifact_condition_counts_full_hits() {
    let index = ArtifactIndex::from_map(&artifact_map(json!({
        "s1": { "chat1": ["m1"], "chat2": ["m2"] }
    })));
    let record = record_with_evidence("q1", &["chat1", "chat2"], &["m1", "m2"]);
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m1", "m2"])),
        _ => None,
    };
    let source: &mut dyn retrieval::SourceResolver = &mut resolver;

    let (score, items, _usage) = condition::score_condition(
        CONDITION,
        vec![(0, record)],
        Some(source),
        Some(&index),
        None,
        None,
    )
    .await
    .expect("条件判分应成功");
    let artifact = score.retrieval.artifact.expect("应产出 artifact 指标");
    assert_eq!(artifact.questions, 1);
    assert_eq!(artifact.coverage_pre, 1.0);
    assert_eq!(artifact.coverage_post, 1.0);
    assert_eq!(artifact.mrr_pre, 1.0);
    assert!(artifact.delta_coverage.abs() < 1e-12);
    assert_eq!(artifact.units_pre_avg, 1.0);
    assert_eq!(artifact.missing_gold_total, 0);

    let item = items[0]
        .item
        .retrieval
        .as_ref()
        .and_then(|retrieval| retrieval.artifact.as_ref())
        .expect("单题应带 artifact 指标");
    assert_eq!(item.gold_artifacts, 2);
    assert_eq!(item.rank_pre, Some(1));
}

/// 部分命中：覆盖率按 gold artifact 算，映射表外的 evidence 计入缺失且不进分母。
#[tokio::test]
async fn artifact_condition_counts_partial_hits() {
    let index = ArtifactIndex::from_map(&artifact_map(json!({
        "s1": { "chat1": ["m1"], "chat2": ["m2"] }
    })));
    let record = record_with_evidence("q1", &["chat1", "chat2", "chat9"], &["m1"]);
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m1"])),
        _ => None,
    };
    let source: &mut dyn retrieval::SourceResolver = &mut resolver;

    let (score, _items, _usage) = condition::score_condition(
        CONDITION,
        vec![(0, record)],
        Some(source),
        Some(&index),
        None,
        None,
    )
    .await
    .expect("条件判分应成功");
    let artifact = score.retrieval.artifact.expect("应产出 artifact 指标");
    assert!(
        (artifact.coverage_pre - 0.5).abs() < 1e-9,
        "分母只含映射表内的 gold"
    );
    assert_eq!(
        artifact.missing_gold_total, 1,
        "映射表外的 evidence 计一条缺失"
    );
}

/// 缺 artifacts.json：artifact 列留 null，条件级缺项带期望路径。
#[tokio::test]
async fn missing_artifacts_file_degrades_to_null() {
    let dir = temp_dir("no-map");
    let tier_dir = dir.join("tiers").join("u1-m1");
    std::fs::create_dir_all(&tier_dir).expect("档位目录应可创建");
    let tiers = json!([{
        "id": "u1-m1",
        "dir": tier_dir.to_string_lossy(),
        "manifest_sha256": "sha",
        "rows": 1
    }]);
    let record = record_with_evidence("q1", &["chat1"], &["m1"]);
    write_run_with_tiers(&dir, &[record], tiers);

    let (report, _usage) = run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("缺映射不应报错");
    let condition = &report.conditions[0];
    assert!(condition.retrieval.artifact.is_none(), "artifact 列留 null");
    let entry = condition
        .missing
        .iter()
        .find(|entry| entry.kind == artifact::MISSING_ARTIFACT)
        .expect("应记 artifact 缺项");
    assert!(
        entry.detail.contains("artifacts.json"),
        "缺项应带期望路径，实际 {}",
        entry.detail
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预置上下文条件不经过召回：检索两列留 null，并记一条缺项。
#[tokio::test]
async fn preset_context_conditions_have_no_retrieval_metrics() {
    let dir = temp_dir("preset");
    let mut records = vec![
        record(
            "q1",
            &["m1"],
            Some("回复"),
            Some(observation(&[], &[], &[])),
        ),
        record(
            "q2",
            &["m2"],
            Some("回复"),
            Some(observation(&[], &[], &[])),
        ),
    ];
    for record in &mut records {
        record.condition_id = "u1-m1/full-context/400/0".to_string();
        record.pipeline = "full-context".to_string();
    }
    write_run(&dir, &records);

    let (report, _usage) = run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("预置上下文条件不应报错");
    let condition = &report.conditions[0];
    assert_eq!(condition.questions, 2);
    assert!(condition.retrieval.message.is_none(), "消息级列留 null");
    assert!(
        condition.retrieval.artifact.is_none(),
        "artifact 级列留 null"
    );
    assert!(
        condition
            .missing
            .iter()
            .any(|entry| entry.kind == MISSING_PRESET_RECALL),
        "应记预置上下文缺项"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 分项判分与 judge 接入
// =========================================================

/// 一题的 rubric 票面（按要点顺序给出命中情况）。
fn points_body(hits: &[bool]) -> String {
    let points: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(index, hit)| format!("{{\"index\":{index},\"hit\":{hit},\"reason\":\"r{index}\"}}"))
        .collect();
    format!("{{\"points\":[{}]}}", points.join(","))
}

/// 不可回答任务的票面。
fn refusal_body(value: bool) -> String {
    format!("{{\"refused\":{value},\"reason\":\"r\"}}")
}

/// 冲突检测任务的票面。
fn conflict_body(value: bool) -> String {
    format!("{{\"accepted_premise\":{value},\"reason\":\"r\"}}")
}

/// 两个要点（6 / 4 分）的逐题产物。
fn two_point_record(question: &str) -> RunItemRecord {
    let mut record = record(
        question,
        &["m1"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    record.score_points = vec![
        QrelsScorePoint {
            description: "第一点".to_string(),
            score: 6.0,
        },
        QrelsScorePoint {
            description: "第二点".to_string(),
            score: 4.0,
        },
    ];
    record
}

/// 用 mock 构造判分句柄（缓存落在判分产物目录）。
fn judge_for(mock: &Arc<MockLlm>, dir: &Path) -> Judge {
    let provider: Arc<dyn LlmProvider> = mock.clone();
    Judge::new(
        provider,
        "judge-model".to_string(),
        AnswerModelKind::Local,
        JudgeSource::Matrix,
        ScoreReport::dir_of(dir).join(judge::CACHE_FILE),
        Duration::ZERO,
    )
    .expect("判分句柄应可构造")
}

/// 一题两要点、命中其一的判分夹具（供聚合用例直接吸收）。
fn rubric_fixture() -> RubricScore {
    let point = |index: usize, weight: f64, hit: bool| RubricPointScore {
        index,
        description: format!("要点 {index}"),
        weight,
        hit: Some(hit),
        votes: vec![Some(hit); 3],
        reason: Some("r".to_string()),
    };
    RubricScore {
        points: vec![point(0, 6.0, true), point(1, 4.0, false)],
        score: 6.0,
        max: 10.0,
        ratio: Some(0.6),
        full_hit: false,
        judge_failed: false,
    }
}

/// 两要点权重 6 / 4、命中其一：得分 6、满分 10、得分率 0.6，且未全命中。
#[tokio::test]
async fn rubric_scores_weighted_points() {
    let dir = temp_dir("rubric-weight");
    let record = two_point_record("q1");
    let mock = Arc::new(MockLlm::with_reply(&points_body(&[true, false])));
    let judge = judge_for(&mock, &dir);

    let score = rubric::score(&judge, &record)
        .await
        .expect("判分流程不应报错")
        .expect("有回复与要点应产出分项结果");

    assert_eq!(score.score, 6.0);
    assert_eq!(score.max, 10.0);
    assert!((score.ratio.expect("应产出得分率") - 0.6).abs() < 1e-9);
    assert!(!score.full_hit);
    assert!(!score.judge_failed);
    assert_eq!(score.points.len(), 2);
    assert_eq!(score.points[1].hit, Some(false));
    assert!(score.points[0].unanimous(), "三票一致应标为无分歧");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 全部要点未命中时得分收敛为正零，产物里不出现负零。
#[tokio::test]
async fn rubric_zero_hits_scores_positive_zero() {
    let dir = temp_dir("rubric-zero");
    let record = two_point_record("q1");
    let mock = Arc::new(MockLlm::with_reply(&points_body(&[false, false])));
    let judge = judge_for(&mock, &dir);

    let score = rubric::score(&judge, &record)
        .await
        .expect("判分流程不应报错")
        .expect("有回复与要点应产出分项结果");

    assert_eq!(score.score, 0.0);
    assert!(score.score.is_sign_positive(), "零分应为正零");
    assert_eq!(score.ratio, Some(0.0));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 多标签题同时计入两个类别；九项固定齐全，无题类别均值留空。
#[test]
fn macro_splits_multi_label_questions() {
    let mut sums = RubricSums::default();
    sums.absorb(&rubric_fixture(), &[0, 2]);
    let summary = sums.finish().expect("有题应产出汇总");

    assert_eq!(summary.questions, 1);
    assert_eq!(summary.judged, 1);
    assert_eq!(summary.macro_by_category.len(), 9, "九类固定齐全");
    assert_eq!(summary.macro_by_category[0].questions, 1);
    assert_eq!(summary.macro_by_category[2].questions, 1);
    assert_eq!(summary.macro_by_category[1].questions, 0);
    assert!(
        summary.macro_by_category[1].mean_ratio.is_none(),
        "无题类别均值应为空"
    );
    assert!((summary.micro_mean_ratio.expect("应有均分") - 0.6).abs() < 1e-9);
    assert_eq!(summary.micro_full_hit_rate, Some(0.0));
    assert_eq!(summary.disagreement_rate, Some(0.0));
}

/// UA 只判不可回答题、CD 只判冲突题：其余题一次模型都不调。
#[tokio::test]
async fn judge_calls_only_cover_matching_categories() {
    let dir = temp_dir("judge-scope");
    let mut plain = record(
        "q0",
        &["m1"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    plain.category = vec![0];
    plain.score_points.clear();
    let mut ua = record(
        "q4",
        &[],
        Some("我不知道"),
        Some(observation(&[], &[], &[])),
    );
    ua.category = vec![4];
    ua.score_points.clear();
    let mut cd = record(
        "q7",
        &["m2"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    cd.category = vec![7];
    cd.score_points.clear();

    let mock = Arc::new(MockLlm::new(""));
    mock.set_responses(vec![
        refusal_body(true),
        refusal_body(true),
        refusal_body(true),
        conflict_body(false),
        conflict_body(false),
        conflict_body(false),
        "4".to_string(),
        "4".to_string(),
        "4".to_string(),
    ]);
    let judge = judge_for(&mock, &dir);
    let entries = vec![(0, plain), (1, ua), (2, cd)];

    let (score, items, usage) =
        condition::score_condition(CONDITION, entries, None, None, None, Some(&judge))
            .await
            .expect("条件判分应成功");

    assert_eq!(mock.chat_calls(), 9, "UA / CD 各三票，语气维每题单票");
    assert!(items[0].item.credibility.ua_judge.is_none());
    assert!(items[0].item.credibility.cd_judge.is_none());
    assert_eq!(items[1].item.credibility.ua_judge, Some(true));
    assert_eq!(items[2].item.credibility.cd_judge, Some(false));

    let ua_rate = score.credibility.ua_judge.expect("应产出 UA 判定");
    assert_eq!(ua_rate.judged, 1);
    assert_eq!(ua_rate.hits, 1);
    assert_eq!(ua_rate.rate, 1.0);
    let cd_rate = score.credibility.cd_judge.expect("应产出 CD 判定");
    assert_eq!(cd_rate.hits, 0);
    assert_eq!(score.credibility.cd_false_premise_accept_rate, Some(0.0));
    assert!(score.rubric.is_none(), "无要点不产出分项汇总");
    assert_eq!(score.judge_failed, 0);

    // mock 未上报用量：调用次数照实记，token 与命中率留空。
    assert_eq!(
        usage.judge.calls,
        mock.chat_calls(),
        "条件用量调用数应等于实际模型调用次数"
    );
    assert_eq!(usage.judge.prompt_tokens, 0);
    assert_eq!(usage.judge.cache_hit_rate, None, "无 token 时命中率未知");
    assert_eq!(usage.answer.calls, 0, "逐题无用量时不计数");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 人格三维按题计一次：多标签题不重复计入，语气维失败只计数，无回复题不参与。
#[tokio::test]
async fn persona_columns_score_once_per_question() {
    let dir = temp_dir("persona");
    let mut good = record(
        "q1",
        &["m1"],
        Some("别难过，我理解你，会好的"),
        Some(observation(&[], &[], &[])),
    );
    good.question = "今天被领导批评了，好难过".to_string();
    good.category = vec![0, 2];
    good.score_points.clear();

    let mut failed_tone = record(
        "q2",
        &["m1"],
        Some("我知道了"),
        Some(observation(&[], &[], &[])),
    );
    failed_tone.question = "我升职了，太开心了".to_string();
    failed_tone.score_points.clear();

    write_run(&dir, &[good, failed_tone, failure_record("q3", &["m1"])]);

    let mock = Arc::new(MockLlm::new(""));
    mock.set_responses(vec![
        "4".to_string(),
        "看不出语气".to_string(),
        "看不出语气".to_string(),
    ]);
    let state = super::read_state(&dir).expect("运行状态应可读");
    let (report, _usage) = super::finish(&dir, state, Vec::new(), Some(judge_for(&mock, &dir)))
        .await
        .expect("判分应成功");

    let condition = &report.conditions[0];
    let persona = condition.persona.as_ref().expect("应有三维汇总");
    assert_eq!(persona.questions, 2, "无回复的失败题不参与");
    assert_eq!(persona.tone, Some(4.0), "只算解析成功的一题");
    assert_eq!(persona.tone_failed, 1);
    assert_eq!(persona.emotion, Some(0.5), "1.0 与 0.0 的均值");
    assert_eq!(persona.fact, Some(0.0), "参考仅两字且回复无重叠");
    assert_eq!(persona.fact_variant, "keyword", "无嵌入时用纯关键词");
    assert_eq!(mock.chat_calls(), 3, "两题语气单票 + 一次重试");

    let items: Vec<ScoredItem> = std::fs::read_to_string(dir.join(SCORE_DIR).join(SCORED_FILE))
        .expect("逐题产物应可读")
        .lines()
        .map(|line| serde_json::from_str(line).expect("逐题行应可解析"))
        .collect();
    let first = items[0].persona.as_ref().expect("q1 应有三维结果");
    assert_eq!(first.tone, Some(4));
    assert_eq!(first.reference, "gold_answer");
    assert_eq!(items[1].persona.as_ref().and_then(|item| item.tone), None);
    assert!(items[2].persona.is_none(), "失败题不参与三维");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 两个可映射单元、一个命中 gold：消息级精确率为 0.5。
#[test]
fn precision_counts_hit_units_over_mappable() {
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m1"])),
        "b" => Some(labels(&["m9"])),
        _ => None,
    };
    let record = record(
        "q1",
        &["m1"],
        Some("回复"),
        Some(observation(&["L1:a", "L1:b"], &["L1:a", "L1:b"], &[])),
    );

    let metrics = measure_item(&record, &mut resolver).expect("应参与检索指标");
    assert!((metrics.precision_pre - 0.5).abs() < 1e-9);
    assert!((metrics.precision_post - 0.5).abs() < 1e-9);
    assert_eq!(metrics.unmappable_pre, 0, "两个单元都可映射");
}

/// artifact 级同一口径：两个可映射单元、一个命中 gold artifact，精确率 0.5。
#[tokio::test]
async fn artifact_precision_counts_hit_units_over_mapped() {
    let index = ArtifactIndex::from_map(&artifact_map(json!({
        "s1": { "chat1": ["m1"], "chat2": ["m9"] }
    })));
    let mut record = record_with_evidence("q1", &["chat1"], &["m1"]);
    record.recall = Some(observation(&["L1:a", "L1:b"], &["L1:a", "L1:b"], &[]));
    let mut resolver = |_layer: &str, id: &str| match id {
        "a" => Some(labels(&["m1"])),
        "b" => Some(labels(&["m9"])),
        _ => None,
    };
    let source: &mut dyn retrieval::SourceResolver = &mut resolver;

    let (score, items, _usage) = condition::score_condition(
        CONDITION,
        vec![(0, record)],
        Some(source),
        Some(&index),
        None,
        None,
    )
    .await
    .expect("条件判分应成功");

    let artifact = score.retrieval.artifact.expect("应产出 artifact 指标");
    assert!((artifact.precision_pre - 0.5).abs() < 1e-9);
    assert!((artifact.precision_post - 0.5).abs() < 1e-9);
    let item = items[0]
        .item
        .retrieval
        .as_ref()
        .and_then(|retrieval| retrieval.artifact.as_ref())
        .expect("单题应带 artifact 指标");
    assert!((item.precision_pre - 0.5).abs() < 1e-9);
}

/// `--no-judge`：判分列整列留空并记缺项，确定性列照常产出。
#[tokio::test]
async fn no_judge_leaves_judge_columns_null() {
    let dir = temp_dir("no-judge");
    let mut record = record(
        "q4",
        &[],
        Some("我不知道"),
        Some(observation(&[], &[], &[])),
    );
    record.category = vec![4];
    write_run(&dir, &[record]);

    let (report, _usage) = run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("跳过判分不应报错");
    assert_eq!(report.judge, JudgeStatus::Disabled);
    let condition = &report.conditions[0];
    assert!(condition.rubric.is_none(), "分项判分列留空");
    let persona = condition
        .persona
        .as_ref()
        .expect("确定性两维不依赖判分模型，照常产出");
    assert!(persona.tone.is_none(), "未启用判分时语气维留空");
    assert_eq!(persona.questions, 1);
    assert!(persona.fact.is_some(), "事实维照常产出");
    assert!(persona.emotion.is_some(), "情感维照常产出");
    assert_eq!(persona.fact_variant, "keyword", "无嵌入时记录纯关键词口径");
    assert_eq!(persona.tone_failed, 0, "未启用判分不计语气维失败");
    assert!(condition.credibility.ua_judge.is_none());
    assert!(condition.credibility.cd_judge.is_none());
    assert_eq!(condition.judge_failed, 0);
    assert_eq!(condition.credibility.ua_refusal_hits, 1, "确定性列照常产出");
    assert!(
        report
            .missing
            .iter()
            .any(|entry| entry.kind == artifact::MISSING_JUDGE),
        "应记 judge 跳过缺项"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一输入两次判分：第二次命中缓存零模型调用，`score.json` 与 `scored.jsonl` 逐字节一致。
///
/// 说明: 用量不在这两份产物里，缓存命中与否都不影响判分结果。
#[tokio::test]
async fn cached_judge_keeps_products_byte_identical() {
    let dir = temp_dir("judge-idempotent");
    write_run(&dir, &[two_point_record("q1")]);
    let mock = Arc::new(MockLlm::with_reply(&points_body(&[true, false])));
    mock.set_responses(vec![
        points_body(&[true, false]),
        points_body(&[true, false]),
        points_body(&[true, false]),
        "4".to_string(),
    ]);

    let state = super::read_state(&dir).expect("运行状态应可读");
    super::finish(&dir, state, Vec::new(), Some(judge_for(&mock, &dir)))
        .await
        .expect("首次判分应成功");
    let calls = mock.chat_calls();
    assert_eq!(calls, 4, "分项三票 + 语气维单票");
    let score_dir = dir.join(SCORE_DIR);
    let first_score = std::fs::read(score_dir.join(SCORE_FILE)).expect("应写出汇总");
    let first_scored = std::fs::read(score_dir.join(SCORED_FILE)).expect("应写出逐题");

    let state = super::read_state(&dir).expect("运行状态应可读");
    let (report, _usage) = super::finish(&dir, state, Vec::new(), Some(judge_for(&mock, &dir)))
        .await
        .expect("再次判分应成功");

    assert_eq!(mock.chat_calls(), calls, "命中缓存不再调用模型");
    assert_eq!(
        std::fs::read(score_dir.join(SCORE_FILE)).expect("应写出汇总"),
        first_score,
        "两次判分的汇总应逐字节一致"
    );
    assert_eq!(
        std::fs::read(score_dir.join(SCORED_FILE)).expect("应写出逐题"),
        first_scored,
        "两次判分的逐题应逐字节一致"
    );
    assert!(matches!(report.judge, JudgeStatus::Configured { .. }));

    let _ = std::fs::remove_dir_all(&dir);
}
