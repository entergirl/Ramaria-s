//! crates/ramaria-eval/src/baseline/build.rs - Ramaria 评测基线建库
//!
//! 设计特点:
//! - 建库全程只调用服务层用例：历史写入 → 封存 → 级联 → 索引 → 快照，不复制记忆算法
//! - 人格在灌入前一次性建齐：级联的"对话另一方"仅在 persona 总数恰好为 2 时有值
//! - 逐日会话即一个记忆分组：写一条断言一条，封存后立刻级联，与产品调度节奏一致
//! - 检查点复用同一条构建库；已完成的档位按静态指纹跳过，u10 可从 u5 检查点续建
//! - 后端不降级：连通性、向量模型或后端应用任一失败即中止，避免产出不可比基线
//! - 构建目录只在成功时清理；失败保留现场，错误文案带档位 / 样本 / 日期

mod artifacts;
mod reuse;
mod stage;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{Engine, L1BrowseRequest, L2BrowseRequest, SessionBrowseRequest};

use crate::assembly;
use crate::corpus::{CorpusFileSpec, CorpusLanguage, CorpusSpec, Sample};
use crate::sandbox::backend::{apply_backend, resolve_backend};

use super::BaselineArtifact;
use super::config::MemoryConfig;
use super::tier::{BuildStep, U5_Y1, select_samples};

use artifacts::{ArtifactEnv, ArtifactInput, write_artifact};
use reuse::ResumeState;
use stage::{create_personas, ingest_sample, window_counts};

// =========================================================
// 常量
// =========================================================

/// 评测建库的会话来源通道。
const CHANNEL: &str = "eval";

/// 外部对话标识前缀（后接 user_key 与日期）。
const CONVERSATION_PREFIX: &str = "lifebench";

/// 构建目录下的人格文件子目录。
const PERSONA_DIR: &str = "personas";

/// 评测人格 uid 前缀（与 Sample::persona_uid 同口径）。
const PERSONA_PREFIX: &str = "lifebench-";

/// 单个样本灌入完成后提示未吸收 L1 的阈值。
///
/// 达到 L2 触发条数即说明有整组待消费；L2 是异步补偿，残留不阻断建库，
/// 清单按库内实际条数记录。
const UNASSORBED_L1_LIMIT: usize = 5;

// =========================================================
// 输入上下文
// =========================================================

/// 一次建库步骤的固定上下文。
///
/// 字段约定:
/// - `corpus` / `file`: 语料登记项（清单口径）；
/// - `sample_ids`: 语料文件内全部样本标识（清单记录来源规模）；
/// - `config`: 评测模型配置；
/// - `output_root`: 档位产物根目录。
pub(crate) struct StepContext<'a> {
    pub corpus: &'a CorpusSpec,
    pub language: CorpusLanguage,
    pub file: &'a CorpusFileSpec,
    pub sample_ids: &'a [String],
    pub config: &'a MemoryConfig,
    pub output_root: &'a Path,
}

/// 建成的人格记录（构建目录内的文件 + uid）。
struct PersonaRecord {
    uid: String,
    source: PathBuf,
}

/// 单个样本的灌入统计。
#[derive(Debug, Default)]
struct SampleStats {
    sessions: usize,
    messages: usize,
    l1: usize,
    consumed: usize,
    events: usize,
    traits: usize,
    artifacts: HashMap<String, Vec<String>>,
}

// =========================================================
// 步骤入口
// =========================================================

/// 执行一次建库步骤并产出该步骤的全部档位产物。
///
/// 参数:
/// - `step`: 建库窗口、样本数与检查点；
/// - `samples`: 语料文件全部样本（按文件顺序）；
/// - `context`: 语料、配置与输出上下文。
///
/// 返回:
/// - 按检查点顺序返回档位产物；
/// - 样本不足、后端不可用、写入 / 封存 / 级联 / 快照失败返回结构化错误。
pub(crate) async fn run_step(
    step: &BuildStep,
    samples: &[Sample],
    context: &StepContext<'_>,
) -> RamariaResult<Vec<BaselineArtifact>> {
    let selected = select_samples(samples, step.sample_count)?;
    verify_plan(step, selected.len())?;

    let mut reused: Vec<Option<BaselineArtifact>> = Vec::with_capacity(step.checkpoints.len());
    for checkpoint in &step.checkpoints {
        reused.push(reuse::existing_artifact(
            &checkpoint.tier,
            &selected[..checkpoint.after_samples],
            context,
        )?);
    }
    if reused.iter().all(Option::is_some) {
        let artifacts: Vec<BaselineArtifact> = reused.iter().flatten().cloned().collect();
        for artifact in &artifacts {
            eprintln!("档位 {}: 产物已存在且指纹匹配，跳过", artifact.tier_id);
        }
        return Ok(artifacts);
    }

    let resume = reuse::resume_state(step, selected, context)?;
    // 不能续跑时忽略复用结果：从头构建会重写全部检查点，旧目录留着只会产出过期摘要。
    let reused = if resume.is_some() {
        reused
    } else {
        vec![None; step.checkpoints.len()]
    };
    let build_root = context
        .output_root
        .join(format!(".build-{}", step.window.id));
    // 未完成构建保留原库：已写入的会话在续跑时按外部标识跳过，避免整档重来。
    let resuming_build = build_root.join(assembly::EVAL_DB_FILE).exists();
    if resuming_build {
        eprintln!("档位 {}: 检测到未完成构建，从现有库继续", step.window.id);
    } else {
        prepare_dir(&build_root)?;
    }
    if let Some(state) = &resume {
        copy_database(&state.u5_dir, &build_root)?;
    }
    for (checkpoint, existing) in step.checkpoints.iter().zip(&reused) {
        if existing.is_none() {
            prepare_dir(&context.output_root.join(checkpoint.tier.id))?;
        }
    }

    eprintln!(
        "档位 {}: 开始建库（{} 个样本 / 消息窗口 {}..={}）",
        step.window.id,
        selected.len(),
        step.window.memory_start,
        step.window.memory_end
    );

    let outcome = run_with_engine(
        step,
        selected,
        context,
        &build_root,
        resume.as_ref(),
        &reused,
    )
    .await;
    if outcome.is_ok() {
        if let Err(error) = std::fs::remove_dir_all(&build_root) {
            tracing::warn!(
                error = %error,
                path = %build_root.display(),
                "清理基线构建目录失败"
            );
        }
    }
    outcome
}

/// 校验检查点与样本数一致，避免计划与产物错位。
fn verify_plan(step: &BuildStep, selected: usize) -> RamariaResult<()> {
    let mut previous = 0usize;
    for checkpoint in &step.checkpoints {
        if checkpoint.after_samples <= previous || checkpoint.after_samples > selected {
            return Err(RamariaError::validation(format!(
                "档位 {} 的检查点样本数非法: {}（已选样本 {selected}）",
                checkpoint.tier.id, checkpoint.after_samples
            )));
        }
        if checkpoint.tier.users != checkpoint.after_samples {
            return Err(RamariaError::validation(format!(
                "档位 {} 的用户数与检查点样本数不一致: {} / {}",
                checkpoint.tier.id, checkpoint.tier.users, checkpoint.after_samples
            )));
        }
        previous = checkpoint.after_samples;
    }
    Ok(())
}

/// 装配引擎 → 执行 → 无论成败关闭连接池。
async fn run_with_engine(
    step: &BuildStep,
    selected: &[Sample],
    context: &StepContext<'_>,
    build_root: &Path,
    resume: Option<&ResumeState>,
    reused: &[Option<BaselineArtifact>],
) -> RamariaResult<Vec<BaselineArtifact>> {
    let engine = assembly::open_engine(build_root).await?;
    let outcome = build_with(&engine, step, selected, context, build_root, resume, reused).await;
    close_engine(engine).await;
    outcome
}

/// 应用后端、建人格、逐样本灌入并在检查点落产物。
async fn build_with(
    engine: &Engine,
    step: &BuildStep,
    selected: &[Sample],
    context: &StepContext<'_>,
    build_root: &Path,
    resume: Option<&ResumeState>,
    reused: &[Option<BaselineArtifact>],
) -> RamariaResult<Vec<BaselineArtifact>> {
    let started = Instant::now();
    let resolved = resolve_backend(&context.config.backend_choice(), None).await?;
    let temperature = resolved.config.temperature;
    let max_tokens = resolved.config.max_tokens;
    let (report, mut warnings) = apply_backend(engine, resolved).await;

    if let Some(fatal) = warnings
        .iter()
        .find(|item| item.contains("后端配置应用失败"))
    {
        return Err(RamariaError::validation(format!(
            "后端配置应用失败: {fatal}"
        )));
    }
    if !engine.probe_llm_health().await {
        return Err(RamariaError::validation(format!(
            "模型服务不可达: {}（{}），基线构建需要可用模型",
            report.model_text(),
            context.config.kind_label()
        )));
    }
    let embedding = engine.embedding_model().await?;
    let dimension = embedding.and_then(|view| view.dimension).ok_or_else(|| {
        RamariaError::validation("向量模型未加载（缺少维度），基线构建需要可用向量模型")
    })?;
    let schema_version = engine.schema_version().await?;
    eprintln!(
        "档位 {}: 后端就绪（{} / {} / {} 维）",
        step.window.id,
        report.model_text(),
        context.config.kind_label(),
        dimension
    );

    if let Some(state) = resume {
        if state.embedding_dimension != Some(dimension) {
            return Err(RamariaError::validation(format!(
                "档位 {}: 嵌入维度与检查点不一致（检查点 {:?} / 当前 {dimension}），删除 {} 后重跑",
                step.window.id, state.embedding_dimension, U5_Y1.id
            )));
        }
        validate_resume(engine, step, selected, state).await?;
        eprintln!(
            "档位 {}: 从 {} 检查点续建（已灌 {} 个样本 / 会话 {} / 消息 {}）",
            step.window.id, U5_Y1.id, state.from_index, state.prior.sessions, state.prior.messages
        );
    }

    // 续建现场可能有封存失败留下的待补 L1：先补扫，再重建话语块，保证快照自包含。
    let retried_l1 = engine.retry_pending_l1_jobs().await;
    if retried_l1 > 0 {
        eprintln!(
            "档位 {}: 补扫 L1 任务 {retried_l1} 个，重建话语块",
            step.window.id
        );
        let rebuilt = engine.rebuild_utt_blocks(true).await?;
        eprintln!(
            "档位 {}: 话语块重建完成（新建 {} / 文档 {}）",
            step.window.id, rebuilt.chunks_created, rebuilt.doc_count
        );
        warnings.push(format!("续建补扫 L1 任务 {retried_l1} 个，话语块已重建"));
    }

    let personas = create_personas(engine, selected, &build_root.join(PERSONA_DIR)).await?;

    // 续跑时前缀样本不再灌入，统计只补语料事实（会话 / 消息），其余字段由库内查询重建。
    let mut stats: Vec<SampleStats> = selected
        .iter()
        .enumerate()
        .map(|(index, sample)| match resume {
            Some(state) if index < state.from_index => {
                let (sessions, messages) = window_counts(sample, &step.window);
                SampleStats {
                    sessions,
                    messages,
                    ..SampleStats::default()
                }
            }
            _ => SampleStats::default(),
        })
        .collect();
    // 续跑时前缀检查点不再重写：直接沿用其摘要（这些档位目录在本次构建中保持不变）。
    let mut artifacts: Vec<BaselineArtifact> = Vec::new();
    for summary in reused.iter().flatten() {
        eprintln!("档位 {}: 产物已存在且指纹匹配，跳过", summary.tier_id);
        artifacts.push(summary.clone());
    }

    for (index, sample) in selected.iter().enumerate() {
        if resume.is_some_and(|state| index < state.from_index) {
            continue;
        }
        let uid = sample.persona_uid();
        eprintln!(
            "档位 {}: 样本 {}/{} {}（{}）开始",
            step.window.id,
            index + 1,
            selected.len(),
            sample.sample_id,
            sample.user_key
        );
        let sample_stats = ingest_sample(engine, sample, &uid, &step.window)
            .await
            .map_err(|error| {
                scoped(
                    error,
                    &format!(
                        "档位 {} 样本 {}（{}）",
                        step.window.id, sample.sample_id, sample.user_key
                    ),
                )
            })?;
        eprintln!(
            "档位 {}: 样本 {}/{} 完成（会话 {} / 消息 {} / L1 {} / L1 消费 {} / L2 +{} / L3 +{}）",
            step.window.id,
            index + 1,
            selected.len(),
            sample_stats.sessions,
            sample_stats.messages,
            sample_stats.l1,
            sample_stats.consumed,
            sample_stats.events,
            sample_stats.traits
        );
        stats[index] = sample_stats;

        if let Some(checkpoint) = step
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.after_samples == index + 1)
        {
            let env = ArtifactEnv {
                report: &report,
                temperature,
                max_tokens,
                dimension,
                schema_version,
                started,
            };
            let (prefix_rows, prefix_samples) = match resume {
                Some(state) if checkpoint.tier.id == step.window.id => {
                    (state.prefix_rows.as_slice(), state.from_index)
                }
                _ => (&[][..], 0),
            };
            let input = ArtifactInput {
                checkpoint,
                selected,
                stats: &stats,
                personas: &personas,
                context,
                env: &env,
                prefix_rows,
                prefix_samples,
            };
            let artifact = write_artifact(engine, &input, &warnings)
                .await
                .map_err(|error| {
                    scoped(
                        error,
                        &format!("档位 {} 检查点 {}", step.window.id, checkpoint.tier.id),
                    )
                })?;
            artifacts.push(artifact);
        }
    }

    Ok(artifacts)
}

// =========================================================
// 续跑校验
// =========================================================

/// 校验续建库与检查点清单一致：人格、会话与分层计数全部对齐才继续灌入。
///
/// 说明:
/// - 失败即中止，不回退从头构建：快照与清单不符说明库被动过，继续灌入只会产出与清单不符的档位；
/// - 会话按 external_ref 还原 user_key 与日期，确认库内只有检查点样本的会话；
/// - L1 按会话逐条计数：persona 直查口径的 total 有取回上限，不能承担计数语义。
async fn validate_resume(
    engine: &Engine,
    step: &BuildStep,
    selected: &[Sample],
    state: &ResumeState,
) -> RamariaResult<()> {
    let prefix = &selected[..state.from_index];
    let expected: Vec<String> = prefix.iter().map(|sample| sample.persona_uid()).collect();
    let allowed: HashSet<String> = selected.iter().map(|sample| sample.persona_uid()).collect();

    let people = engine.persona_list().await?;
    let present: HashSet<&str> = people.iter().map(|persona| persona.uid.as_str()).collect();
    for uid in &expected {
        if !present.contains(uid.as_str()) {
            return Err(resume_mismatch(step, format!("缺少人格 {uid}")));
        }
    }
    // 人格在灌入前一次性建齐，检查点快照允许含本次步骤其余样本的人格，但不允许计划外人格。
    for uid in &present {
        if uid.starts_with(PERSONA_PREFIX) && !allowed.contains(*uid) {
            return Err(resume_mismatch(step, format!("含计划外人格 {uid}")));
        }
    }

    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await?;
    if page.total != state.prior.sessions {
        return Err(resume_mismatch(
            step,
            format!("会话数 {} 与清单 {} 不符", page.total, state.prior.sessions),
        ));
    }
    let messages: usize = page
        .items
        .iter()
        .map(|session| session.message_count as usize)
        .sum();
    if messages != state.prior.messages {
        return Err(resume_mismatch(
            step,
            format!("消息数 {messages} 与清单 {} 不符", state.prior.messages),
        ));
    }

    let users: HashSet<&str> = prefix
        .iter()
        .map(|sample| sample.user_key.as_str())
        .collect();
    let mut seen: HashSet<(String, NaiveDate)> = HashSet::new();
    let mut l1 = 0usize;
    for session in &page.items {
        let reference = session
            .external_ref
            .as_deref()
            .ok_or_else(|| resume_mismatch(step, format!("会话 {} 缺少外部标识", session.id)))?;
        let (user, date_text) = reference
            .strip_prefix(CONVERSATION_PREFIX)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once(':'))
            .ok_or_else(|| resume_mismatch(step, format!("会话外部标识格式不符: {reference}")))?;
        if !users.contains(user) {
            return Err(resume_mismatch(
                step,
                format!("会话 {reference} 的用户不在检查点范围"),
            ));
        }
        let date = NaiveDate::parse_from_str(date_text, "%Y-%m-%d")
            .map_err(|_| resume_mismatch(step, format!("会话日期非法: {reference}")))?;
        if !step.window.includes_date(date) {
            return Err(resume_mismatch(
                step,
                format!("会话 {reference} 的日期不在建库窗口内"),
            ));
        }
        if !seen.insert((user.to_string(), date)) {
            return Err(resume_mismatch(step, format!("会话重复: {reference}")));
        }
        l1 += engine.memory_l1_by_session(session.id).await?.len();
    }
    if l1 != state.prior.l1 {
        return Err(resume_mismatch(
            step,
            format!("L1 {l1} 与清单 {} 不符", state.prior.l1),
        ));
    }

    let mut l2 = 0usize;
    let mut l3 = 0usize;
    let mut unabsorbed = 0usize;
    for uid in &expected {
        l2 += engine
            .memory_l2(L2BrowseRequest {
                persona: Some(uid.clone()),
                limit: Some(1),
                offset: Some(0),
            })
            .await?
            .total;
        l3 += engine.memory_l3(Some(uid)).await?.len();
        unabsorbed += engine
            .memory_l1(L1BrowseRequest {
                persona: Some(uid.clone()),
                unabsorbed_only: true,
                limit: Some(1),
                offset: Some(0),
            })
            .await?
            .total;
    }
    if l2 != state.prior.l2 {
        return Err(resume_mismatch(
            step,
            format!("L2 {l2} 与清单 {} 不符", state.prior.l2),
        ));
    }
    if l3 != state.prior.l3 {
        return Err(resume_mismatch(
            step,
            format!("L3 {l3} 与清单 {} 不符", state.prior.l3),
        ));
    }
    if unabsorbed != state.prior.unabsorbed_l1 {
        return Err(resume_mismatch(
            step,
            format!(
                "未吸收 L1 {unabsorbed} 与清单 {} 不符",
                state.prior.unabsorbed_l1
            ),
        ));
    }
    Ok(())
}

/// 续跑校验失败的错误（带档位与具体项，不回退从头构建）。
fn resume_mismatch(step: &BuildStep, detail: String) -> RamariaError {
    RamariaError::validation(format!(
        "档位 {}: 检查点续跑校验失败（{detail}），删除 {} 后重跑",
        step.window.id, U5_Y1.id
    ))
}

/// 复制检查点数据库作为续建起点。
fn copy_database(source_dir: &Path, build_root: &Path) -> RamariaResult<()> {
    let source = source_dir.join(assembly::EVAL_DB_FILE);
    let target = build_root.join(assembly::EVAL_DB_FILE);
    std::fs::copy(&source, &target)
        .map(|_| ())
        .map_err(|error| {
            RamariaError::io(
                format!("复制检查点数据库失败: {}", source.display()),
                Some(error),
            )
        })
}

// =========================================================
// 文件与错误
// =========================================================

/// 准备目录：已存在则删除后重建（只删给定目录）。
fn prepare_dir(dir: &Path) -> RamariaResult<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|error| {
            RamariaError::io(format!("清理基线目录失败: {}", dir.display()), Some(error))
        })?;
    }
    std::fs::create_dir_all(dir).map_err(|error| {
        RamariaError::io(format!("创建基线目录失败: {}", dir.display()), Some(error))
    })
}

/// 关闭连接池并丢弃引擎，释放 SQLite 文件句柄。
async fn close_engine(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
}

/// 给错误补一层领域上下文（保留原错误分类与 source）。
fn scoped(error: RamariaError, scope: &str) -> RamariaError {
    let context = format!("{scope}: {}", error.context());
    match error {
        RamariaError::Config {
            source, trace_id, ..
        } => RamariaError::Config {
            context,
            source,
            trace_id,
        },
        RamariaError::Storage {
            source, trace_id, ..
        } => RamariaError::Storage {
            context,
            source,
            trace_id,
        },
        RamariaError::Llm {
            source, trace_id, ..
        } => RamariaError::Llm {
            context,
            source,
            trace_id,
        },
        RamariaError::Serialization {
            source, trace_id, ..
        } => RamariaError::Serialization {
            context,
            source,
            trace_id,
        },
        RamariaError::Privacy {
            source, trace_id, ..
        } => RamariaError::Privacy {
            context,
            source,
            trace_id,
        },
        RamariaError::Index {
            source, trace_id, ..
        } => RamariaError::Index {
            context,
            source,
            trace_id,
        },
        RamariaError::Embedding {
            source, trace_id, ..
        } => RamariaError::Embedding {
            context,
            source,
            trace_id,
        },
        RamariaError::Validation { trace_id, .. } => RamariaError::Validation { context, trace_id },
        RamariaError::Io {
            source, trace_id, ..
        } => RamariaError::Io {
            context,
            source,
            trace_id,
        },
        RamariaError::Unsupported { trace_id, .. } => {
            RamariaError::Unsupported { context, trace_id }
        }
    }
}
