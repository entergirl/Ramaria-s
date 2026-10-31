//! crates/ramaria-eval/src/baseline/build/reuse.rs - Ramaria 评测基线档位复用
//!
//! 设计特点:
//! - 复用只看静态指纹：语料哈希、流水线版本、模型标识与档位样本，任一项变化即重建
//! - 不比较嵌入指纹与生成参数：前者要加载向量模型才能得到，后者由产品默认派生
//! - 续跑以 u5 检查点为起点：库内人格 / 会话 / 分层计数与清单逐项对齐后才继续灌入
//! - 判定不满足时返回 None 由调用方从头构建，判定过程不写盘也不改产物
//! - 只读文件系统与清单，不装配引擎、不加载模型，故不依赖运行期后端

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ramaria_core::error::RamariaResult;

use crate::assembly;
use crate::corpus::Sample;

use crate::baseline::manifest::{self, BaselineManifest, Counts};
use crate::baseline::qrels::{self, QrelsRow};
use crate::baseline::tier::{BuildStep, TierSpec, U5_Y1, U10_Y1};
use crate::baseline::{BaselineArtifact, EVAL_PIPELINE_VERSION};

use super::{PERSONA_DIR, StepContext};

// =========================================================
// 档位复用
// =========================================================

/// 读取档位清单；缺失或解析失败返回 None。
///
/// 说明:
/// - 清单残缺说明上一次构建未落到最后一步，按不可复用处理，不报错；
/// - 只读档位目录下的 manifest.json，不触碰其他产物。
pub(super) fn read_manifest(tier_dir: &Path) -> Option<BaselineManifest> {
    let text = std::fs::read_to_string(tier_dir.join(manifest::MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 档位产物完整且静态指纹与当前上下文一致时返回摘要。
///
/// 参数:
/// - tier: 目标档位（决定目录名、窗口与用户数）；
/// - used: 该档位覆盖的样本（按语料顺序的前 N 个）；
/// - context: 语料、语言与模型配置上下文。
///
/// 返回:
/// - 全部条件满足返回摘要（elapsed_ms 记 0，本次未建库）；
/// - 任一条件不满足返回 None，由调用方重建该档位。
///
/// 说明:
/// - 不比较 memory_model.temperature / max_tokens：两者由产品默认派生，随默认值变化；
/// - 不比较 embedding.fingerprint：该字段要加载向量模型才能得到，复用判定刻意不加载模型；
///   更换嵌入模型时删除档位目录即可重建。
pub(super) fn existing_artifact(
    tier: &TierSpec,
    used: &[Sample],
    context: &StepContext<'_>,
) -> RamariaResult<Option<BaselineArtifact>> {
    let dir = context.output_root.join(tier.id);
    let Some(manifest) = read_manifest(&dir) else {
        return Ok(None);
    };
    if !matches_context(&manifest, tier, used, context) || !files_complete(&dir, &manifest) {
        return Ok(None);
    }
    Ok(Some(BaselineArtifact {
        tier_id: tier.id,
        dir,
        counts: manifest.counts,
        qa_in_window: manifest.qa.in_window,
        elapsed_ms: 0,
    }))
}

/// 清单静态指纹与当前上下文逐项比对。
fn matches_context(
    manifest: &BaselineManifest,
    tier: &TierSpec,
    used: &[Sample],
    context: &StepContext<'_>,
) -> bool {
    let crate_version = env!("CARGO_PKG_VERSION");
    let schema_version = ramaria_service::DB_SCHEMA_VERSION;
    let prompt_template_version = ramaria_service::PROMPT_TEMPLATE_VERSION;
    let pipeline = &manifest.pipeline;
    let saved = &manifest.tier;

    manifest.artifact_kind == manifest::ARTIFACT_KIND
        && manifest.schema_version == manifest::ARTIFACT_SCHEMA_VERSION
        && manifest.baseline_id
            == tier.baseline_id(
                context.corpus.name,
                context.corpus.version,
                context.language.as_str(),
            )
        && manifest.corpus.sha256 == context.file.sha256
        && manifest.corpus.language == context.language.as_str()
        && pipeline.crate_version == crate_version
        && pipeline.schema_version == schema_version
        && pipeline.prompt_template_version == prompt_template_version
        && pipeline.pipeline_version == EVAL_PIPELINE_VERSION
        && pipeline.code_fingerprint
            == manifest::code_fingerprint(
                crate_version,
                schema_version,
                prompt_template_version,
                EVAL_PIPELINE_VERSION,
            )
        && manifest.memory_model.kind == context.config.kind_str()
        && manifest.memory_model.model == context.config.model
        && saved.id == tier.id
        && saved.users == tier.users
        && saved.memory_start == format_date(tier.memory_start)
        && saved.memory_end == format_date(tier.memory_end)
        && saved.query_end == format_date(tier.query_end)
        && saved.span_days == tier.span_days()
        && saved.sample_ids.len() == used.len()
        && saved
            .sample_ids
            .iter()
            .zip(used)
            .all(|(saved, sample)| *saved == sample.sample_id)
}

/// 档位目录是否四件产物齐全（库 / qrels / 配置 / 人格）。
fn files_complete(dir: &Path, manifest: &BaselineManifest) -> bool {
    let db_non_empty = std::fs::metadata(dir.join(assembly::EVAL_DB_FILE))
        .map(|meta| meta.len() > 0)
        .unwrap_or(false);
    db_non_empty
        && dir.join(assembly::EVAL_CONFIG_FILE).is_file()
        && line_count(&dir.join(qrels::QRELS_FILE)) == Some(manifest.qa.in_window)
        && persona_files(dir) == Some(manifest.tier.users)
}

/// 非空行数；文件不可读返回 None。
fn line_count(path: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.lines().filter(|line| !line.trim().is_empty()).count())
}

/// 人格目录下的 toml 文件数；目录不可读返回 None。
fn persona_files(dir: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(dir.join(PERSONA_DIR)).ok()?;
    let mut count = 0usize;
    for entry in entries {
        let path = entry.ok()?.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            count += 1;
        }
    }
    Some(count)
}

/// 日期按清单口径格式化。
fn format_date(date: chrono::NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

// =========================================================
// 检查点续跑
// =========================================================

/// u10 构建的续跑状态。
///
/// 字段约定:
/// - from_index: 检查点已覆盖的样本数，也是续建的首个样本下标；
/// - prefix_rows: 检查点档位已产出的 qrels 行，对应前 from_index 个样本；
/// - prior: 检查点清单的建库计数，用于续建库逐项核对；
/// - embedding_dimension: 检查点的向量维度，必须与当前模型一致；
/// - u5_dir: 检查点档位目录，其数据库快照作为续建起点。
pub(super) struct ResumeState {
    pub from_index: usize,
    pub prefix_rows: Vec<QrelsRow>,
    pub prior: Counts,
    pub embedding_dimension: Option<usize>,
    pub u5_dir: PathBuf,
}

/// 满足续跑条件时返回状态；不满足返回 None（调用方从头构建）。
///
/// 参数:
/// - step: 当前建库步骤（只对全年十用户步骤生效）；
/// - selected: 本次步骤覆盖的样本；
/// - context: 语料、语言与模型配置上下文。
///
/// 返回:
/// - 检查点档位与更早档位都可复用、qrels 覆盖样本一致且题号唯一时返回状态；
/// - 任一条件不满足返回 None，并打印一行原因。
///
/// 说明:
/// - 更早档位必须一并可复用：那些档位的前缀样本不再灌入，无法从续建库重算 qrels；
/// - 不满足条件不是错误，重建代价远小于中断后完全无法继续。
pub(super) fn resume_state(
    step: &BuildStep,
    selected: &[Sample],
    context: &StepContext<'_>,
) -> RamariaResult<Option<ResumeState>> {
    if step.window.id != U10_Y1.id || selected.len() < U5_Y1.users {
        return Ok(None);
    }
    let Some(checkpoint) = step
        .checkpoints
        .iter()
        .find(|item| item.after_samples == U5_Y1.users && item.tier.id == U5_Y1.id)
    else {
        return Ok(None);
    };

    let Some(u5) = existing_artifact(
        &checkpoint.tier,
        &selected[..checkpoint.after_samples],
        context,
    )?
    else {
        return Ok(fallback(step));
    };

    // 检查点必须来自同一次建库：人格在灌入前一次性建齐，各检查点行数应相同。
    // 数量不同说明是分别单跑的档位，库内消息 id 不同源，不能混用前缀 qrels。
    for earlier in step
        .checkpoints
        .iter()
        .filter(|item| item.after_samples < checkpoint.after_samples)
    {
        let Some(artifact) =
            existing_artifact(&earlier.tier, &selected[..earlier.after_samples], context)?
        else {
            return Ok(fallback(step));
        };
        if artifact.counts.personas != u5.counts.personas {
            return Ok(fallback(step));
        }
    }
    let Some(manifest) = read_manifest(&u5.dir) else {
        return Ok(fallback(step));
    };

    let prefix = &selected[..checkpoint.after_samples];
    let covered = manifest.tier.sample_ids.len() == prefix.len()
        && manifest
            .tier
            .sample_ids
            .iter()
            .zip(prefix)
            .all(|(saved, sample)| *saved == sample.sample_id);
    if !covered {
        return Ok(fallback(step));
    }

    let Some(rows) = read_qrels(&u5.dir) else {
        return Ok(fallback(step));
    };
    let users: HashSet<&str> = prefix
        .iter()
        .map(|sample| sample.user_key.as_str())
        .collect();
    let mut questions: HashSet<&str> = HashSet::new();
    let valid = rows.len() == manifest.qa.in_window
        && rows
            .iter()
            .all(|row| users.contains(row.sample_key.as_str()))
        && rows
            .iter()
            .all(|row| questions.insert(row.question_id.as_str()));
    if !valid {
        return Ok(fallback(step));
    }

    Ok(Some(ResumeState {
        from_index: checkpoint.after_samples,
        prefix_rows: rows,
        prior: manifest.counts,
        embedding_dimension: manifest.embedding.dimension,
        u5_dir: u5.dir,
    }))
}

/// 读取检查点 qrels；任一行为空或解析失败返回 None。
fn read_qrels(dir: &Path) -> Option<Vec<QrelsRow>> {
    let text = std::fs::read_to_string(dir.join(qrels::QRELS_FILE)).ok()?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// 续跑条件不满足时的统一提示。
fn fallback(step: &BuildStep) -> Option<ResumeState> {
    eprintln!("档位 {}: 检查点不满足续跑条件，从头构建", step.window.id);
    None
}

#[cfg(test)]
mod tests;
