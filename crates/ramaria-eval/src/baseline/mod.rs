//! crates/ramaria-eval/src/baseline/mod.rs - Ramaria 评测基线入口
//!
//! 设计特点:
//! - 入口只做参数解析、计划编排与产物汇总：档位、配置、建库、清单与 qrels 分文件
//! - 语料先校验再加载：哈希不符不建库；样本按语料顺序取前 N 个
//! - 产物按档位自包含（库 / 清单 / qrels / 配置 / 人格），构建用临时目录隔离
//! - 失败即中止：后端、写入、封存、级联、索引与快照任一失败都不产出半成品
//! - 进度写标准错误，最终产物摘要写标准输出，管道取标准输出即得路径

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{Engine, EngineOptions};

use crate::assembly;
use crate::corpus::{self, CorpusFileSpec, CorpusLanguage, CorpusSpec, Sample};
use crate::measure::elapsed_ms;

pub(crate) mod artifacts;
mod build;
mod config;
mod manifest;
mod qrels;
mod tier;

#[cfg(test)]
mod tests;

pub use artifacts::{ARTIFACTS_FILE, ArtifactMap, CorpusFacts};
pub use config::{EVAL_CONFIG_RELATIVE_PATH, MemoryConfig};
pub use manifest::{
    ARTIFACT_KIND, ARTIFACT_SCHEMA_VERSION, BaselineManifest, Counts, MANIFEST_FILE,
};
pub use qrels::{QRELS_FILE, QrelsRow, QrelsScorePoint};
pub use tier::{TIER_IDS, TierSelection, TierSpec, parse_tier};

use tier::select_samples;

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
    let (spec, language, file) = resolve_corpus(corpus, language)?;
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

// =========================================================
// Artifact 映射回填
// =========================================================

/// 单个档位的映射回填结果。
///
/// 字段约定:
/// - `tier_id`: 档位标识；
/// - `dir`: 档位产物目录；
/// - `samples`: 写入映射的样本数；
/// - `artifacts`: 写入映射的 artifact 数；
/// - `skipped`: 映射已存在且口径一致，本次未重写。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBackfill {
    pub tier_id: &'static str,
    pub dir: PathBuf,
    pub samples: usize,
    pub artifacts: usize,
    pub skipped: bool,
}

/// 一次回填的结果。
///
/// 字段约定:
/// - `tiers`: 按档位构建顺序排列的回填摘要；
/// - `total_elapsed_ms`: 从命令进入至收尾的耗时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBackfillReport {
    pub tiers: Vec<ArtifactBackfill>,
    pub total_elapsed_ms: u64,
}

impl ArtifactBackfillReport {
    /// 渲染为人读报告（每个档位一行路径 + 一行计数）。
    pub fn render(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "artifact 映射 {} 个档位（总耗时 {} ms）:",
            self.tiers.len(),
            self.total_elapsed_ms
        );
        for tier in &self.tiers {
            let state = if tier.skipped {
                "已存在，跳过"
            } else {
                "已写出"
            };
            let _ = writeln!(
                output,
                "  {}  {}（{state} / 样本 {} / artifact {}）",
                tier.tier_id,
                tier.dir.display(),
                tier.samples,
                tier.artifacts
            );
        }
        output
    }
}

/// 为已建成的档位补写 artifact 映射。
///
/// 参数:
/// - `corpus` / `language` / `output`: 与建库命令同口径的语料、语言与产物根；
/// - `tier`: 档位取值，必须显式给出（缺省返回 `Validation`）。
///
/// 返回:
/// - 成功时返回各档位回填摘要；
/// - 缺档位、档位目录 / 库 / 清单 / 语料缺失或不自洽返回结构化错误，文案带期望路径。
///
/// 说明:
/// - 只写档位目录下的 artifacts.json，不改库、清单与 qrels；
/// - 已存在且语料摘要一致时跳过该档位，不重写；
/// - `all` 按构建顺序回填全部档位目录。
pub async fn backfill(
    corpus: Option<String>,
    tier: Option<String>,
    language: Option<String>,
    output: Option<PathBuf>,
) -> RamariaResult<ArtifactBackfillReport> {
    let started = Instant::now();
    let tier = tier.ok_or_else(|| {
        RamariaError::validation(format!(
            "回填 artifact 映射需要显式指定档位: --tier <{}>（或 all）",
            TIER_IDS.join(" / ")
        ))
    })?;
    let (spec, language, file) = resolve_corpus(corpus, language)?;
    let selection = parse_tier(&tier)?;
    let output_root = match output {
        Some(path) => path,
        None => default_output_root(spec, language)?,
    };
    corpus::verify(spec.name, Some(language))?;
    let samples = corpus::load_language(&corpus::default_root()?, language)?;
    let facts = CorpusFacts::from_spec(spec, file);

    let mut tiers = Vec::new();
    for step in selection.plan() {
        for checkpoint in &step.checkpoints {
            tiers.push(backfill_tier(&samples, &checkpoint.tier, &output_root, &facts).await?);
        }
    }

    Ok(ArtifactBackfillReport {
        tiers,
        total_elapsed_ms: elapsed_ms(started),
    })
}

/// 单个档位目录的映射回填。
async fn backfill_tier(
    samples: &[Sample],
    tier: &TierSpec,
    output_root: &Path,
    corpus: &CorpusFacts,
) -> RamariaResult<ArtifactBackfill> {
    let dir = output_root.join(tier.id);
    if !dir.is_dir() {
        return Err(RamariaError::validation(format!(
            "档位产物目录不存在: {}（请先构建该档位）",
            dir.display()
        )));
    }
    let db = dir.join(assembly::EVAL_DB_FILE);
    if !db.is_file() {
        return Err(RamariaError::validation(format!(
            "档位库不存在: {}",
            db.display()
        )));
    }
    check_tier_manifest(&dir, tier, corpus)?;
    if let Ok(existing) = artifacts::read(&dir, &corpus.sha256) {
        return Ok(ArtifactBackfill {
            tier_id: tier.id,
            dir,
            samples: existing.samples.len(),
            artifacts: existing.artifact_total(),
            skipped: true,
        });
    }

    let selected = select_samples(samples, tier.users)?;
    // 只读档位产物：用档位自带的配置快照装配，不重写任何档位文件。
    let config = dir.join(assembly::EVAL_CONFIG_FILE);
    if !config.is_file() {
        return Err(RamariaError::validation(format!(
            "档位配置快照不存在: {}",
            config.display()
        )));
    }
    let engine = Engine::open_with(EngineOptions::new(&db).with_config_path(&config)).await?;
    let collected = artifacts::collect(&engine, selected, tier, tier.id, corpus.clone()).await;
    close_engine(engine).await;
    let map = collected?;
    artifacts::write(&dir, &map)?;

    Ok(ArtifactBackfill {
        tier_id: tier.id,
        dir,
        samples: map.samples.len(),
        artifacts: map.artifact_total(),
        skipped: false,
    })
}

/// 校验档位清单与本次语料 / 档位一致（清单缺失或口径不符即拒绝回填）。
fn check_tier_manifest(dir: &Path, tier: &TierSpec, corpus: &CorpusFacts) -> RamariaResult<()> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(format!("读取基线清单失败: {}", path.display()), Some(error))
    })?;
    let manifest: BaselineManifest = serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!("解析基线清单失败: {}: {error}", path.display()))
    })?;
    if manifest.tier.id != tier.id {
        return Err(RamariaError::validation(format!(
            "档位清单标识不符: 期望 {}，实际 {}（{}）",
            tier.id,
            manifest.tier.id,
            path.display()
        )));
    }
    if manifest.corpus.sha256 != corpus.sha256 {
        return Err(RamariaError::validation(format!(
            "档位清单的语料摘要不符: 期望 {}，实际 {}（{}）",
            corpus.sha256,
            manifest.corpus.sha256,
            path.display()
        )));
    }
    Ok(())
}

/// 解析语料与语言，返回登记项与本次语言文件。
fn resolve_corpus(
    corpus: Option<String>,
    language: Option<String>,
) -> RamariaResult<(&'static CorpusSpec, CorpusLanguage, &'static CorpusFileSpec)> {
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
    Ok((spec, language, file))
}

/// 关闭连接池并丢弃引擎，释放 SQLite 文件句柄。
async fn close_engine(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
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
