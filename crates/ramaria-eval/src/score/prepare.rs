//! crates/ramaria-eval/src/score/prepare.rs - Ramaria 评测判分准备
//!
//! 设计特点:
//! - 判分模型来自矩阵：优先 `[judge]`，缺省回退首个答题模型并在产物里标明来源
//! - provider 经服务层无引擎出口构造：判分不缺工作库，也不影响产品配置
//! - 不可用只降级：配置非法或密钥缺失记一条缺项，其余指标照常产出
//! - 本地模型地址留空时补默认端点，避免矩阵里只写 `kind = "local"` 时报错
//! - 环境变量给得出评测专用密钥时用它构造 provider（只进内存，日志只记是否存在）
//! - 缓存路径固定落在判分产物目录下，同一运行的两份产物共用一份缓存

use std::path::Path;

use ramaria_core::types::{BackendConfig, LlmProvider};

use super::artifact::{JudgeSource, MISSING_JUDGE_UNAVAILABLE, MissingEntry, ScoreReport};
use super::judge::{self, Judge};
use crate::run::{AnswerModelKind, RunMatrix};

/// 按矩阵定案判分模型并构造判分句柄。
///
/// 参数:
/// - `run_root`: 运行根目录（缓存文件落在判分产物目录下）；
/// - `matrix`: 冻结的运行矩阵；
/// - `missing`: 运行级缺项收集器（不可用时追加一条）。
///
/// 返回:
/// - 可用时返回判分句柄；矩阵无可用模型、配置非法或密钥缺失时返回 `None`。
pub(super) fn prepare_judge(
    run_root: &Path,
    matrix: &RunMatrix,
    missing: &mut Vec<MissingEntry>,
) -> Option<Judge> {
    let (kind, base_url, model, source) = match &matrix.judge {
        Some(judge) => (
            judge.kind,
            judge.base_url.clone(),
            judge.model.clone(),
            JudgeSource::Matrix,
        ),
        None => match matrix.answer_models.first() {
            Some(first) => (
                first.kind,
                first.base_url.clone(),
                first.model.clone(),
                JudgeSource::AnswerModelFallback,
            ),
            None => {
                missing.push(MissingEntry::run_level(
                    MISSING_JUDGE_UNAVAILABLE,
                    "矩阵没有可用判分模型",
                ));
                return None;
            }
        },
    };

    let base_url = match kind {
        AnswerModelKind::Local if base_url.trim().is_empty() => {
            BackendConfig::lm_studio_default().base_url
        }
        _ => base_url,
    };
    let config =
        BackendConfig::new_with_defaults(provider_of(kind, &base_url), base_url, model.clone());
    let cache_path = ScoreReport::dir_of(run_root).join(judge::CACHE_FILE);
    let eval_key = crate::eval_api_key();
    tracing::info!(has_eval_key = eval_key.is_some(), "判分模型密钥来源");
    let vote_delay = judge::vote_delay_from_env();
    let built = ramaria_service::eval_llm_with_key(&config, eval_key.as_deref())
        .and_then(|provider| Judge::new(provider, model, kind, source, cache_path, vote_delay))
        .map(|judge| judge.with_concurrency(judge::concurrency_from_env()));
    match built {
        Ok(judge) => Some(judge),
        Err(error) => {
            missing.push(MissingEntry::run_level(
                MISSING_JUDGE_UNAVAILABLE,
                format!("判分模型不可用: {error}"),
            ));
            None
        }
    }
}

/// 判分模型类别与地址 → 产品后端 provider。
///
/// 说明: 云端按地址区分 DeepSeek 与 OpenAI-compatible，其余一律按 OpenAI 口径。
fn provider_of(kind: AnswerModelKind, base_url: &str) -> LlmProvider {
    match kind {
        AnswerModelKind::Local => LlmProvider::LmStudio,
        AnswerModelKind::Cloud if base_url.contains("api.deepseek.com") => LlmProvider::DeepSeek,
        AnswerModelKind::Cloud => LlmProvider::OpenAI,
    }
}
