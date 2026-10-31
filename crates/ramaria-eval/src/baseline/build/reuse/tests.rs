//! crates/ramaria-eval/src/baseline/build/reuse/tests.rs - Ramaria 评测基线复用测试
//!
//! 设计特点:
//! - 只用手工清单与临时目录，不建库、不加载模型
//! - 复用判定逐项断言：指纹、产物齐全、样本一致
//! - 续跑判定断言起点、前缀行与必要条件
//! - 每个用例独立临时目录，结束即删

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use ramaria_service::ChatRole;

use super::super::stage::window_counts;
use super::*;
use crate::baseline::config::MemoryConfig;
use crate::baseline::manifest::{
    ManifestCorpus, ManifestEmbedding, ManifestIndex, ManifestMemoryModel, ManifestPipeline,
    ManifestQa, ManifestTier, ManifestTiming,
};
use crate::baseline::tier::{TierSelection, U1_Y1};
use crate::corpus::{CorpusFileSpec, CorpusLanguage, CorpusSpec, Message, Session};
use crate::sandbox::backend::SandboxModelKind;

// =========================================================
// 夹具
// =========================================================

/// 临时目录路径。
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-reuse-{tag}-{nanos}"))
}

/// 夹具日期。
fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("夹具日期应合法")
}

/// 构造样本：每天两条消息，无 QA。
fn sample(id: &str, user: &str, days: &[&str]) -> Sample {
    Sample {
        sample_id: id.to_string(),
        user_key: user.to_string(),
        speaker_a: "甲".to_string(),
        speaker_b: "乙".to_string(),
        sessions: days
            .iter()
            .enumerate()
            .map(|(index, day)| Session {
                index: index as u32 + 1,
                date: date(day),
                messages: vec![
                    Message {
                        role: ChatRole::User,
                        speaker: "甲".to_string(),
                        dia_id: format!("{day}_chat{}", index + 1),
                        text: "问".to_string(),
                    },
                    Message {
                        role: ChatRole::Assistant,
                        speaker: "乙".to_string(),
                        dia_id: format!("{day}_chat{}", index + 1),
                        text: "答".to_string(),
                    },
                ],
            })
            .collect(),
        qa: Vec::new(),
    }
}

/// 一条 qrels 行（只填复用判定关心的字段）。
fn row(user: &str, question: &str) -> QrelsRow {
    QrelsRow {
        question_id: question.to_string(),
        sample_key: user.to_string(),
        ask_time: "2025-06-01".to_string(),
        ask_time_ms: 0,
        question: "问".to_string(),
        answer: "答".to_string(),
        category: vec![1],
        question_type: vec!["Single_hop".to_string()],
        evidence: Vec::new(),
        gold_message_ids: Vec::new(),
        score_points: Vec::new(),
    }
}

/// 建库计数。
fn counts(sessions: usize, messages: usize) -> Counts {
    Counts {
        personas: 0,
        sessions,
        messages,
        l1: 0,
        l2: 0,
        l3: 0,
        unabsorbed_l1: 0,
    }
}

/// 评测配置（复用判定只读类别与模型标识）。
fn config() -> MemoryConfig {
    MemoryConfig {
        kind: SandboxModelKind::Local,
        base_url: String::new(),
        model: "fixture-model".to_string(),
        embedding_path: PathBuf::from("fixture-embedding"),
        source: PathBuf::from(".eval/eval.toml"),
    }
}

/// 复用判定所需的上下文与样本。
struct Fixture {
    root: PathBuf,
    samples: Vec<Sample>,
    sample_ids: Vec<String>,
    config: MemoryConfig,
    corpus: &'static CorpusSpec,
    file: &'static CorpusFileSpec,
}

impl Fixture {
    fn new(tag: &str, users: usize) -> Self {
        let corpus = crate::corpus::lifebench();
        let file = corpus
            .files
            .iter()
            .find(|file| file.language == CorpusLanguage::Zh)
            .expect("中文语料应已登记");
        let samples: Vec<Sample> = (1..=users)
            .map(|index| {
                sample(
                    &format!("样本{index}"),
                    &format!("u{index}"),
                    &["2025-03-01", "2025-03-02"],
                )
            })
            .collect();
        let sample_ids = samples.iter().map(|item| item.sample_id.clone()).collect();
        Self {
            root: temp_dir(tag),
            samples,
            sample_ids,
            config: config(),
            corpus,
            file,
        }
    }

    fn context(&self) -> StepContext<'_> {
        StepContext {
            corpus: self.corpus,
            language: CorpusLanguage::Zh,
            file: self.file,
            sample_ids: &self.sample_ids,
            config: &self.config,
            output_root: &self.root,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 构造与上下文一致的清单。
fn manifest_for(
    tier: &TierSpec,
    used: &[Sample],
    context: &StepContext<'_>,
    counts: Counts,
    in_window: usize,
) -> BaselineManifest {
    let crate_version = env!("CARGO_PKG_VERSION");
    let schema_version = ramaria_service::DB_SCHEMA_VERSION;
    let prompt_template_version = ramaria_service::PROMPT_TEMPLATE_VERSION;
    BaselineManifest {
        artifact_kind: manifest::ARTIFACT_KIND.to_string(),
        schema_version: manifest::ARTIFACT_SCHEMA_VERSION,
        baseline_id: tier.baseline_id(
            context.corpus.name,
            context.corpus.version,
            context.language.as_str(),
        ),
        created_at: "2026-10-29T08:00:00Z".to_string(),
        corpus: ManifestCorpus {
            name: context.corpus.name.to_string(),
            version: context.corpus.version.to_string(),
            language: context.language.as_str().to_string(),
            file_name: context.file.file_name.to_string(),
            sha256: context.file.sha256.to_string(),
            size: context.file.size,
            qa_total: context.file.qa_count,
            sample_ids: context.sample_ids.to_vec(),
        },
        tier: ManifestTier {
            id: tier.id.to_string(),
            users: tier.users,
            memory_start: format_date(tier.memory_start),
            memory_end: format_date(tier.memory_end),
            query_end: format_date(tier.query_end),
            span_days: tier.span_days(),
            sample_ids: used.iter().map(|item| item.sample_id.clone()).collect(),
        },
        qa: ManifestQa {
            total: in_window,
            in_window,
        },
        pipeline: ManifestPipeline {
            crate_version: crate_version.to_string(),
            schema_version,
            prompt_template_version: prompt_template_version.to_string(),
            pipeline_version: EVAL_PIPELINE_VERSION.to_string(),
            code_fingerprint: manifest::code_fingerprint(
                crate_version,
                schema_version,
                prompt_template_version,
                EVAL_PIPELINE_VERSION,
            ),
        },
        memory_model: ManifestMemoryModel {
            kind: context.config.kind_str().to_string(),
            model: context.config.model.clone(),
            temperature: 0.3,
            max_tokens: 2048,
        },
        embedding: ManifestEmbedding {
            fingerprint: "fixture-embedding".to_string(),
            dimension: Some(1024),
        },
        index: ManifestIndex {
            status: "ready".to_string(),
            loaded: 1,
            total: 1,
        },
        counts,
        timing: ManifestTiming {
            build_elapsed_ms: 1,
        },
        warnings: Vec::new(),
    }
}

/// 落一份可复用的档位目录。
fn write_tier(dir: &Path, manifest: &BaselineManifest, rows: &[QrelsRow], personas: usize) {
    std::fs::create_dir_all(dir).expect("档位目录应可创建");
    manifest::write(dir, manifest).expect("清单应可写出");
    qrels::write(dir, rows).expect("qrels 应可写出");
    std::fs::write(dir.join(assembly::EVAL_CONFIG_FILE), "[memory]\n").expect("配置快照应可写出");
    std::fs::write(dir.join(assembly::EVAL_DB_FILE), b"fixture-db").expect("库占位应可写出");
    let persona_dir = dir.join(PERSONA_DIR);
    std::fs::create_dir_all(&persona_dir).expect("人格目录应可创建");
    for index in 1..=personas {
        std::fs::write(
            persona_dir.join(format!("lifebench-u{index}.toml")),
            "assistant_name = \"乙\"\n",
        )
        .expect("人格文件应可写出");
    }
}

// =========================================================
// 档位复用
// =========================================================

/// 产物齐全且指纹一致时返回清单摘要。
#[test]
fn existing_artifact_reads_complete_tier() {
    let fixture = Fixture::new("complete", 5);
    let context = fixture.context();
    let used = &fixture.samples[..5];
    let counts = counts(10, 20);
    let manifest = manifest_for(&U5_Y1, used, &context, counts, 7);
    let dir = fixture.root.join(U5_Y1.id);
    let rows: Vec<QrelsRow> = (1..=5)
        .map(|index| row(&format!("u{index}"), &format!("q{index}")))
        .chain(std::iter::once(row("u1", "q6")))
        .chain(std::iter::once(row("u2", "q7")))
        .collect();
    write_tier(&dir, &manifest, &rows, 5);

    let artifact = existing_artifact(&U5_Y1, used, &context)
        .expect("复用判定不应报错")
        .expect("产物齐全时应复用");

    assert_eq!(artifact.tier_id, U5_Y1.id);
    assert_eq!(artifact.dir, dir);
    assert_eq!(artifact.counts, counts);
    assert_eq!(artifact.qa_in_window, 7);
    assert_eq!(artifact.elapsed_ms, 0);

    fixture.cleanup();
}

/// 语料哈希、代码指纹或模型标识任一变化即不复用。
#[test]
fn existing_artifact_rejects_fingerprint_mismatch() {
    let fixture = Fixture::new("fingerprint", 5);
    let context = fixture.context();
    let used = &fixture.samples[..5];
    let dir = fixture.root.join(U5_Y1.id);
    let rows = vec![row("u1", "q1")];
    let manifest = manifest_for(&U5_Y1, used, &context, counts(1, 2), 1);
    write_tier(&dir, &manifest, &rows, 5);

    let mut changed = manifest.clone();
    changed.corpus.sha256 = "0".repeat(64);
    manifest::write(&dir, &changed).expect("清单应可写出");
    assert_eq!(
        existing_artifact(&U5_Y1, used, &context).expect("判定不应报错"),
        None,
        "语料哈希不同不应复用"
    );

    let mut changed = manifest.clone();
    changed.pipeline.code_fingerprint = "f".repeat(64);
    manifest::write(&dir, &changed).expect("清单应可写出");
    assert_eq!(
        existing_artifact(&U5_Y1, used, &context).expect("判定不应报错"),
        None,
        "代码指纹不同不应复用"
    );

    let mut changed = manifest.clone();
    changed.memory_model.model = "other-model".to_string();
    manifest::write(&dir, &changed).expect("清单应可写出");
    assert_eq!(
        existing_artifact(&U5_Y1, used, &context).expect("判定不应报错"),
        None,
        "模型标识不同不应复用"
    );

    fixture.cleanup();
}

/// 缺 qrels 或缺数据库都不复用。
#[test]
fn existing_artifact_rejects_incomplete_files() {
    let fixture = Fixture::new("files", 5);
    let context = fixture.context();
    let used = &fixture.samples[..5];
    let dir = fixture.root.join(U5_Y1.id);
    write_tier(
        &dir,
        &manifest_for(&U5_Y1, used, &context, counts(1, 2), 1),
        &[row("u1", "q1")],
        5,
    );

    std::fs::remove_file(dir.join(qrels::QRELS_FILE)).expect("qrels 应可删除");
    assert_eq!(
        existing_artifact(&U5_Y1, used, &context).expect("判定不应报错"),
        None,
        "缺 qrels 不应复用"
    );

    qrels::write(&dir, &[row("u1", "q1")]).expect("qrels 应可回写");
    std::fs::remove_file(dir.join(assembly::EVAL_DB_FILE)).expect("库应可删除");
    assert_eq!(
        existing_artifact(&U5_Y1, used, &context).expect("判定不应报错"),
        None,
        "缺数据库不应复用"
    );

    fixture.cleanup();
}

/// 档位样本与当前样本顺序不一致即不复用。
#[test]
fn existing_artifact_rejects_wrong_samples() {
    let fixture = Fixture::new("samples", 5);
    let context = fixture.context();
    let used = &fixture.samples[..5];
    let dir = fixture.root.join(U5_Y1.id);
    let mut manifest = manifest_for(&U5_Y1, used, &context, counts(1, 2), 1);
    manifest.tier.sample_ids.swap(0, 1);
    write_tier(&dir, &manifest, &[row("u1", "q1")], 5);

    assert_eq!(
        existing_artifact(&U5_Y1, used, &context).expect("判定不应报错"),
        None,
        "样本顺序不一致不应复用"
    );

    fixture.cleanup();
}

// =========================================================
// 检查点续跑
// =========================================================

/// 全年十用户步骤的建库计划。
fn u10_step() -> BuildStep {
    TierSelection::All
        .plan()
        .into_iter()
        .find(|step| step.window.id == U10_Y1.id)
        .expect("计划应包含十用户步骤")
}

/// u1 与 u5 检查点都可复用时返回续跑状态与前缀 qrels。
#[test]
fn resume_state_reads_prefix_rows() {
    let fixture = Fixture::new("resume", 10);
    let context = fixture.context();
    let u1_counts = Counts {
        personas: 10,
        ..counts(2, 4)
    };
    let manifest = manifest_for(&U1_Y1, &fixture.samples[..1], &context, u1_counts, 2);
    write_tier(
        &fixture.root.join(U1_Y1.id),
        &manifest,
        &[row("u1", "q1"), row("u1", "q2")],
        1,
    );

    let u5_counts = Counts {
        personas: 10,
        ..counts(10, 20)
    };
    let u5_manifest = manifest_for(&U5_Y1, &fixture.samples[..5], &context, u5_counts, 6);
    let rows: Vec<QrelsRow> = (1..=5)
        .map(|index| row(&format!("u{index}"), &format!("q{index}")))
        .chain(std::iter::once(row("u3", "q6")))
        .collect();
    write_tier(&fixture.root.join(U5_Y1.id), &u5_manifest, &rows, 5);

    let state = resume_state(&u10_step(), &fixture.samples, &context)
        .expect("续跑判定不应报错")
        .expect("两个检查点都可复用时应续跑");

    assert_eq!(state.from_index, 5);
    assert_eq!(state.prefix_rows.len(), 6);
    assert_eq!(state.prior, u5_counts);
    assert_eq!(state.embedding_dimension, Some(1024));
    assert_eq!(state.u5_dir, fixture.root.join(U5_Y1.id));

    fixture.cleanup();
}

/// 缺少 u1 检查点时从头构建。
#[test]
fn resume_state_requires_u1() {
    let fixture = Fixture::new("resume-u1", 10);
    let context = fixture.context();
    let u5_manifest = manifest_for(&U5_Y1, &fixture.samples[..5], &context, counts(10, 20), 5);
    let rows: Vec<QrelsRow> = (1..=5)
        .map(|index| row(&format!("u{index}"), &format!("q{index}")))
        .collect();
    write_tier(&fixture.root.join(U5_Y1.id), &u5_manifest, &rows, 5);

    let state = resume_state(&u10_step(), &fixture.samples, &context).expect("判定不应报错");
    assert!(state.is_none(), "缺 u1 检查点不应续跑");

    fixture.cleanup();
}

/// 分别单跑的检查点人格行数不同，不能拼接前缀 qrels。
#[test]
fn resume_state_rejects_mixed_persona_counts() {
    let fixture = Fixture::new("resume-personas", 10);
    let context = fixture.context();
    let u1_manifest = manifest_for(
        &U1_Y1,
        &fixture.samples[..1],
        &context,
        Counts {
            personas: 1,
            ..counts(2, 4)
        },
        2,
    );
    write_tier(
        &fixture.root.join(U1_Y1.id),
        &u1_manifest,
        &[row("u1", "q1"), row("u1", "q2")],
        1,
    );

    let u5_manifest = manifest_for(
        &U5_Y1,
        &fixture.samples[..5],
        &context,
        Counts {
            personas: 5,
            ..counts(10, 20)
        },
        5,
    );
    let rows: Vec<QrelsRow> = (1..=5)
        .map(|index| row(&format!("u{index}"), &format!("q{index}")))
        .collect();
    write_tier(&fixture.root.join(U5_Y1.id), &u5_manifest, &rows, 5);

    let state = resume_state(&u10_step(), &fixture.samples, &context).expect("判定不应报错");
    assert!(state.is_none(), "人格行数不同的检查点不应续跑");

    fixture.cleanup();
}

// =========================================================
// 语料事实
// =========================================================

/// 窗口内会话数与消息数和灌入口径一致。
#[test]
fn window_counts_matches_window() {
    let days = ["2025-01-01", "2025-06-01", "2026-02-01"];
    let mut sample = sample("样本1", "u1", &days);
    sample.sessions[0].messages.truncate(2);
    sample.sessions[1].messages.truncate(1);
    sample.sessions[2].messages.truncate(2);

    let (sessions, messages) = window_counts(&sample, &U10_Y1);
    assert_eq!(sessions, 2, "窗口外会话不计入");
    assert_eq!(messages, 3, "只统计窗口内会话的消息");
}
