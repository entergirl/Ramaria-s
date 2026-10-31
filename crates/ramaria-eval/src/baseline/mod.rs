//! crates/ramaria-eval/src/baseline/mod.rs - Ramaria 评测基线入口
//!
//! 设计特点:
//! - 入口只做参数解析、计划编排与产物汇总：档位、配置、建库、清单与 qrels 分文件
//! - 语料先校验再加载：哈希不符不建库；样本按语料顺序取前 N 个
//! - 产物按档位自包含（库 / 清单 / qrels / 配置 / 人格），构建用临时目录隔离
//! - 失败即中止：后端、写入、封存、级联、索引与快照任一失败都不产出半成品
//! - 进度写标准错误，最终产物摘要写标准输出，管道取标准输出即得路径

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::corpus::{self, CorpusLanguage, CorpusSpec};
use crate::measure::elapsed_ms;

mod build;
mod config;
mod manifest;
mod qrels;
mod tier;

pub use config::{EVAL_CONFIG_RELATIVE_PATH, MemoryConfig};
pub use manifest::Counts;
pub use tier::{TIER_IDS, TierSelection, TierSpec, parse_tier};

/// 建库流水线版本（参与代码指纹）。
pub(crate) const EVAL_PIPELINE_VERSION: &str = "m7-v1";

/// 未指定语料时使用的登记名。
const DEFAULT_CORPUS: &str = "lifebench";

/// 评测根相对路径（基线产物默认落在其下）。
const BASELINE_ROOT_RELATIVE: &str = ".eval/baselines";

// =========================================================
// 产物与报告
// =========================================================

/// 单个档位的产物摘要。
///
/// 字段约定:
/// - `tier_id`: 档位标识（产物子目录名）；
/// - `dir`: 档位产物目录；
/// - `counts`: 清单同口径计数；
/// - `qa_in_window`: qrels 行数；
/// - `elapsed_ms`: 该档位从建库开始到产物落盘的耗时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineArtifact {
    pub tier_id: &'static str,
    pub dir: PathBuf,
    pub counts: Counts,
    pub qa_in_window: usize,
    pub elapsed_ms: u64,
}

/// 一次基线运行的结果。
///
/// 字段约定:
/// - `artifacts`: 按构建顺序排列的档位产物；
/// - `total_elapsed_ms`: 从命令进入至全部产物落盘的耗时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineReport {
    pub artifacts: Vec<BaselineArtifact>,
    pub total_elapsed_ms: u64,
}

impl BaselineReport {
    /// 渲染为人读报告（每个档位一行路径 + 一行计数）。
    pub fn render(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "基线产物 {} 个（总耗时 {} ms）:",
            self.artifacts.len(),
            self.total_elapsed_ms
        );
        for artifact in &self.artifacts {
            let _ = writeln!(output, "  {}  {}", artifact.tier_id, artifact.dir.display());
            let _ = writeln!(
                output,
                "      qrels {} / 会话 {} / 消息 {} / L1 {} / L2 {} / L3 {} / 未吸收 {} / 人格 {}",
                artifact.qa_in_window,
                artifact.counts.sessions,
                artifact.counts.messages,
                artifact.counts.l1,
                artifact.counts.l2,
                artifact.counts.l3,
                artifact.counts.unabsorbed_l1,
                artifact.counts.personas
            );
        }
        output
    }
}

// =========================================================
// 命令入口
// =========================================================

/// 构建评测基线库。
///
/// 参数:
/// - `corpus`: 语料名（缺省 `lifebench`）；
/// - `tier`: 档位（缺省 `all`）；
/// - `language`: 语料语言（缺省 `zh`）；
/// - `output`: 产物根目录（缺省 `.eval/baselines/<corpus>-<version>-<language>`）。
///
/// 返回:
/// - 成功时返回各档位产物摘要；
/// - 语料缺失 / 哈希不符 / 配置非法 / 建库失败返回结构化错误并中止。
pub async fn run(
    corpus: Option<String>,
    tier: Option<String>,
    language: Option<String>,
    output: Option<PathBuf>,
) -> RamariaResult<BaselineReport> {
    let started = Instant::now();
    let corpus_name = corpus.as_deref().unwrap_or(DEFAULT_CORPUS);
    let spec = corpus::find(corpus_name).ok_or_else(|| {
        RamariaError::validation(format!(
            "未知语料: {corpus_name}（已登记: {}）",
            corpus::lifebench().name
        ))
    })?;
    let language = language
        .as_deref()
        .map(CorpusLanguage::parse)
        .transpose()?
        .unwrap_or(CorpusLanguage::Zh);
    let file = spec
        .files
        .iter()
        .find(|file| file.language == language)
        .ok_or_else(|| {
            RamariaError::validation(format!(
                "语料 {} 未登记 {} 版本",
                spec.name,
                language.as_str()
            ))
        })?;
    let selection = parse_tier(tier.as_deref().unwrap_or("all"))?;
    let output_root = match output {
        Some(path) => path,
        None => default_output_root(spec, language)?,
    };

    eprintln!(
        "语料: {} {} / 语言 {} / 文件 {} / 产物根 {}",
        spec.name,
        spec.version,
        language.as_str(),
        file.file_name,
        output_root.display()
    );
    corpus::verify(spec.name, Some(language))?;
    let samples = corpus::load_language(&corpus::default_root()?, language)?;
    let sample_ids: Vec<String> = samples
        .iter()
        .map(|sample| sample.sample_id.clone())
        .collect();

    let config = MemoryConfig::load(&config_path()?)?;
    eprintln!(
        "模型: {}（{}）/ 向量模型目录已配置",
        config.model,
        config.kind_label()
    );

    let context = build::StepContext {
        corpus: spec,
        language,
        file,
        sample_ids: &sample_ids,
        config: &config,
        output_root: &output_root,
    };

    let mut artifacts = Vec::new();
    for step in selection.plan() {
        let step_artifacts = build::run_step(&step, &samples, &context).await?;
        artifacts.extend(step_artifacts);
    }

    Ok(BaselineReport {
        artifacts,
        total_elapsed_ms: elapsed_ms(started),
    })
}

/// 默认产物根：当前工作目录下的 `.eval/baselines/<corpus>-<version>-<language>`。
fn default_output_root(spec: &CorpusSpec, language: CorpusLanguage) -> RamariaResult<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))?;
    Ok(cwd.join(BASELINE_ROOT_RELATIVE).join(format!(
        "{}-{}-{}",
        spec.name,
        spec.version,
        language.as_str()
    )))
}

/// 评测配置文件路径：当前工作目录下的 `.eval/eval.toml`。
fn config_path() -> RamariaResult<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))?;
    Ok(cwd.join(EVAL_CONFIG_RELATIVE_PATH))
}
