//! crates/ramaria-eval/src/baseline/build/artifacts.rs - 基线检查点产物导出
//!
//! 设计特点:
//! - 一个档位目录自包含：数据库快照、清单、qrels、配置快照与人格文件同根
//! - 快照前先重建索引并核对状态，索引不就绪或缺少向量模型标识即中止
//! - 计数先与灌入统计对账，再取浏览用例口径，避免产物与过程不一致
//! - 续跑检查点的 qrels 由前缀行与新生成行拼接，前缀样本不重复生成
//! - 清单只写相对标识与语料事实：不写绝对路径、服务地址与密钥
//! - 未命中 evidence 计入告警写入清单，不静默丢弃

use std::path::Path;
use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::IndexStatus;
use ramaria_service::{Engine, L1BrowseRequest, L2BrowseRequest, SessionBrowseRequest};

use crate::assembly;
use crate::corpus::Sample;
use crate::measure::elapsed_ms;
use crate::sandbox::backend::BackendReport;

use crate::baseline::manifest::{self, BaselineManifest, Counts};
use crate::baseline::qrels::{self, QrelsRow};
use crate::baseline::tier::{Checkpoint, TierSpec};
use crate::baseline::{BaselineArtifact, EVAL_PIPELINE_VERSION};
use crate::baseline::{artifacts as tier_artifacts, artifacts::CorpusFacts};

use super::{PERSONA_DIR, PersonaRecord, SampleStats, StepContext};

// =========================================================
// 输入结构
// =========================================================

/// 检查点写产物时使用的运行期口径。
pub(super) struct ArtifactEnv<'a> {
    pub report: &'a BackendReport,
    pub temperature: f64,
    pub max_tokens: u32,
    pub dimension: usize,
    pub schema_version: i32,
    pub started: Instant,
}

/// 检查点写产物所需的输入。
///
/// 字段约定:
/// - prefix_rows: 续跑检查点已产出的 qrels 行，非续跑时为空；
/// - prefix_samples: prefix_rows 覆盖的样本数，这些样本不再生成 qrels。
pub(super) struct ArtifactInput<'a> {
    pub checkpoint: &'a Checkpoint,
    pub selected: &'a [Sample],
    pub stats: &'a [SampleStats],
    pub personas: &'a [PersonaRecord],
    pub context: &'a StepContext<'a>,
    pub env: &'a ArtifactEnv<'a>,
    pub prefix_rows: &'a [QrelsRow],
    pub prefix_samples: usize,
}

/// 索引状态快照（清单口径）。
struct IndexCounts {
    status: String,
    loaded: usize,
    total: usize,
    model: String,
}

/// 清单装配输入（字段齐全后一次构造，避免长参数列表）。
struct ManifestInput<'a> {
    tier: &'a TierSpec,
    used: &'a [Sample],
    context: &'a StepContext<'a>,
    counts: &'a Counts,
    in_window: usize,
    env: &'a ArtifactEnv<'a>,
    index: &'a IndexCounts,
    warnings: Vec<String>,
}

// =========================================================
// 检查点产物
// =========================================================

/// 在检查点落一个档位的全部产物。
///
/// 说明:
/// - prefix_rows 为续跑检查点已产出的 qrels 行，覆盖前 prefix_samples 个样本；
/// - 前缀样本不再生成 qrels，其余样本按本次灌入的 artifact 映射逐条生成。
pub(super) async fn write_artifact(
    engine: &Engine,
    input: &ArtifactInput<'_>,
    warnings: &[String],
) -> RamariaResult<BaselineArtifact> {
    let tier = &input.checkpoint.tier;
    let used = &input.selected[..input.checkpoint.after_samples];
    if used.len() > input.stats.len() {
        return Err(RamariaError::validation("检查点与样本统计不一致"));
    }

    let docs = engine.rebuild_index().await?;
    let status = engine.index_status();
    let ready =
        status.status == IndexStatus::Ready || (status.total > 0 && status.loaded == status.total);
    if !ready {
        return Err(RamariaError::validation(format!(
            "索引未就绪: {}（loaded {} / total {}）",
            status.status.as_str(),
            status.loaded,
            status.total
        )));
    }
    let model = status.model.clone().unwrap_or_default();
    let model = model.trim();
    if model.is_empty() {
        return Err(RamariaError::validation("索引状态缺少向量模型标识"));
    }
    let index = IndexCounts {
        status: status.status.as_str().to_string(),
        loaded: status.loaded,
        total: status.total,
        model: model.to_string(),
    };
    eprintln!(
        "档位 {}: 索引就绪（{docs} 篇 / 模型 {model}），开始快照",
        tier.id
    );

    let tier_dir = input.context.output_root.join(tier.id);
    if input.personas.len() < used.len() {
        return Err(RamariaError::validation("检查点与人格记录不一致"));
    }
    // 档位只覆盖前 N 个样本：人格文件与分层计数都按同一前缀裁剪，避免小档位带上后建人格。
    let active_personas = &input.personas[..used.len()];
    let counts = collect_counts(engine, active_personas, &input.stats[..used.len()]).await?;
    let db_path = tier_dir.join(assembly::EVAL_DB_FILE);
    engine.snapshot_database(&db_path).await?;

    let mut missing = 0usize;
    let mut rows = input.prefix_rows.to_vec();
    for (index, sample) in used.iter().enumerate().skip(input.prefix_samples) {
        let sample_rows = qrels::rows(sample, tier, &input.stats[index].artifacts);
        missing += sample_rows.missing_evidence;
        rows.extend(sample_rows.rows);
    }
    qrels::write(&tier_dir, &rows)?;
    // 映射一律从库里重新采集：续跑时前缀样本没有内存统计，拼装会缺段。
    let corpus_facts = CorpusFacts::from_spec(input.context.corpus, input.context.file);
    let map = tier_artifacts::collect(engine, used, tier, tier.id, corpus_facts).await?;
    tier_artifacts::write(&tier_dir, &map)?;
    assembly::write_config_snapshot(&tier_dir)?;
    copy_personas(&tier_dir, active_personas)?;

    let mut manifest_warnings = warnings.to_vec();
    if missing > 0 {
        manifest_warnings.push(format!(
            "{missing} 条 evidence 引用不在建库窗口内，对应 gold 为空"
        ));
    }

    let manifest = build_manifest(ManifestInput {
        tier,
        used,
        context: input.context,
        counts: &counts,
        in_window: rows.len(),
        env: input.env,
        index: &index,
        warnings: manifest_warnings,
    });
    manifest::write(&tier_dir, &manifest)?;

    let elapsed = elapsed_ms(input.env.started);
    eprintln!(
        "档位 {}: 产物完成（qrels {} 条 / 会话 {} / 消息 {} / L1 {} / L2 {} / L3 {}）",
        tier.id,
        rows.len(),
        counts.sessions,
        counts.messages,
        counts.l1,
        counts.l2,
        counts.l3
    );

    Ok(BaselineArtifact {
        tier_id: tier.id,
        dir: tier_dir,
        counts,
        qa_in_window: rows.len(),
        elapsed_ms: elapsed,
    })
}

/// 汇总计数：会话 / 消息以全库聚合为准，L1 取逐会话条数之和。
///
/// 说明:
/// - `personas` 只含档位活跃人格，分层计数按它们逐个查询；
/// - `counts.personas` 记库内实际人格行数，可能多于活跃人格（人格在灌入前一次性建齐）；
/// - L1 不取 persona 直查口径的 `total`：该字段按取回条数收敛，不能承担精确计数；
/// - 会话 / 消息仍与灌入统计对账，续跑时前缀统计由语料事实补齐。
async fn collect_counts(
    engine: &Engine,
    personas: &[PersonaRecord],
    stats: &[SampleStats],
) -> RamariaResult<Counts> {
    let persona_rows = engine.persona_list().await?.len();
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await?;
    let listed_messages: usize = page
        .items
        .iter()
        .map(|session| session.message_count as usize)
        .sum();
    let ingested_sessions: usize = stats.iter().map(|sample| sample.sessions).sum();
    let ingested_messages: usize = stats.iter().map(|sample| sample.messages).sum();
    if page.total != ingested_sessions || listed_messages != ingested_messages {
        return Err(RamariaError::validation(format!(
            "会话聚合与灌入计数不一致（会话 {}/{ingested_sessions}，消息 {listed_messages}/{ingested_messages}）",
            page.total
        )));
    }

    let mut l1 = 0usize;
    for session in &page.items {
        l1 += engine.memory_l1_by_session(session.id).await?.len();
    }

    let mut l2 = 0usize;
    let mut l3 = 0usize;
    let mut unabsorbed = 0usize;
    for persona in personas {
        l2 += engine
            .memory_l2(L2BrowseRequest {
                persona: Some(persona.uid.clone()),
                limit: Some(1),
                offset: Some(0),
            })
            .await?
            .total;
        l3 += engine.memory_l3(Some(&persona.uid)).await?.len();
        unabsorbed += engine
            .memory_l1(L1BrowseRequest {
                persona: Some(persona.uid.clone()),
                unabsorbed_only: true,
                limit: Some(1),
                offset: Some(0),
            })
            .await?
            .total;
    }

    Ok(Counts {
        personas: persona_rows,
        sessions: page.total,
        messages: listed_messages,
        l1,
        l2,
        l3,
        unabsorbed_l1: unabsorbed,
    })
}

/// 组装清单（字段口径见 manifest 模块）。
fn build_manifest(input: ManifestInput<'_>) -> BaselineManifest {
    let ManifestInput {
        tier,
        used,
        context,
        counts,
        in_window,
        env,
        index,
        warnings,
    } = input;
    let crate_version = env!("CARGO_PKG_VERSION");
    let prompt_template_version = ramaria_service::PROMPT_TEMPLATE_VERSION;
    BaselineManifest {
        artifact_kind: manifest::ARTIFACT_KIND.to_string(),
        schema_version: manifest::ARTIFACT_SCHEMA_VERSION,
        baseline_id: tier.baseline_id(
            context.corpus.name,
            context.corpus.version,
            context.language.as_str(),
        ),
        created_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        corpus: manifest::ManifestCorpus {
            name: context.corpus.name.to_string(),
            version: context.corpus.version.to_string(),
            language: context.language.as_str().to_string(),
            file_name: context.file.file_name.to_string(),
            sha256: context.file.sha256.to_string(),
            size: context.file.size,
            qa_total: context.file.qa_count,
            sample_ids: context.sample_ids.to_vec(),
        },
        tier: manifest::ManifestTier {
            id: tier.id.to_string(),
            users: tier.users,
            memory_start: tier.memory_start.format("%Y-%m-%d").to_string(),
            memory_end: tier.memory_end.format("%Y-%m-%d").to_string(),
            query_end: tier.query_end.format("%Y-%m-%d").to_string(),
            span_days: tier.span_days(),
            sample_ids: used.iter().map(|sample| sample.sample_id.clone()).collect(),
        },
        qa: manifest::ManifestQa {
            total: used.iter().map(|sample| sample.qa.len()).sum(),
            in_window,
        },
        pipeline: manifest::ManifestPipeline {
            crate_version: crate_version.to_string(),
            schema_version: env.schema_version,
            prompt_template_version: prompt_template_version.to_string(),
            pipeline_version: EVAL_PIPELINE_VERSION.to_string(),
            code_fingerprint: manifest::code_fingerprint(
                crate_version,
                env.schema_version,
                prompt_template_version,
                EVAL_PIPELINE_VERSION,
            ),
        },
        memory_model: manifest::ManifestMemoryModel {
            kind: env.report.kind.as_str().to_string(),
            model: env.report.model.clone(),
            temperature: env.temperature,
            max_tokens: env.max_tokens,
        },
        embedding: manifest::ManifestEmbedding {
            fingerprint: index.model.clone(),
            dimension: Some(env.dimension),
        },
        index: manifest::ManifestIndex {
            status: index.status.clone(),
            loaded: index.loaded,
            total: index.total,
        },
        counts: *counts,
        timing: manifest::ManifestTiming {
            build_elapsed_ms: elapsed_ms(env.started),
        },
        warnings,
    }
}

/// 复制人格文件到档位产物目录。
fn copy_personas(tier_dir: &Path, personas: &[PersonaRecord]) -> RamariaResult<()> {
    let dir = tier_dir.join(PERSONA_DIR);
    std::fs::create_dir_all(&dir).map_err(|error| {
        RamariaError::io(
            format!("创建产物人格目录失败: {}", dir.display()),
            Some(error),
        )
    })?;
    for persona in personas {
        let target = dir.join(format!("{}.toml", persona.uid));
        std::fs::copy(&persona.source, &target).map_err(|error| {
            RamariaError::io(
                format!("复制人格文件失败: {}", target.display()),
                Some(error),
            )
        })?;
    }
    Ok(())
}
