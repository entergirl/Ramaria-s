//! crates/ramaria-eval/src/run/workspace.rs - Ramaria 评测条件工作区模块
//!
//! 设计特点:
//! - 工作区自包含：运行根、工作库与条件快照都落在同一目录树下
//! - 工作库按需复制：首次从基线拷贝，后续直接复用，供断点续跑
//! - 条件快照以基线配置为基底，只覆盖路径、通道、调度与注入预算相关项
//! - 严格模式按条件隔离一份库，默认模式所有条件共用一个工作库
//! - 快照先写临时文件再改名，中断不会留下半份配置

use std::path::{Path, PathBuf};

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{Engine, EngineOptions};

use crate::assembly;
use crate::sandbox::backend::{
    BackendReport, SandboxBackendChoice, SandboxModelKind, apply_backend, resolve_backend,
};

use super::matrix::{AnswerModelKind, Condition, Pipeline};

#[cfg(test)]
mod tests;

/// 运行状态文件名（运行器写入）。
pub const RUN_FILE: &str = "run.json";

/// 逐题产物文件名（运行器写入）。
pub const ITEMS_FILE: &str = "items.jsonl";

/// 工作目录名。
pub const WORK_DIR: &str = "work";

/// 记忆条数上限：预算轴只受字符预算约束。
const RAG_MAX_MEMORIES: u32 = 20;

/// flat-rag 条件的块级字符预算：允许整块进入上下文，再由请求预算截断。
const FLAT_RAG_BLOCK_CHARS: u32 = 8_000;

// =========================================================
// 基线产物
// =========================================================

/// 一个档位的三条产物路径。
///
/// 字段约定:
/// - `dir`: 档位产物目录；
/// - `db`: 基线库；
/// - `config`: 基线配置快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineProduct {
    pub dir: PathBuf,
    pub db: PathBuf,
    pub config: PathBuf,
}

impl BaselineProduct {
    /// 按档位目录推导库与配置路径。
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let db = dir.join(assembly::EVAL_DB_FILE);
        let config = dir.join(assembly::EVAL_CONFIG_FILE);
        Self { dir, db, config }
    }
}

// =========================================================
// 工作区
// =========================================================

/// 一次运行的目录工作区。
///
/// 字段约定:
/// - `root`: 运行根（调用方给定），`run.json` 与 `items.jsonl` 后续落在根下。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWorkspace {
    root: PathBuf,
}

impl RunWorkspace {
    /// 创建运行根与工作目录；已存在时保留，供续跑复用。
    ///
    /// 参数:
    /// - `root`: 运行根目录。
    pub fn new(root: impl Into<PathBuf>) -> RamariaResult<Self> {
        let root = root.into();
        let work = root.join(WORK_DIR);
        std::fs::create_dir_all(&work).map_err(|error| {
            RamariaError::io(format!("创建工作区失败: {}", work.display()), Some(error))
        })?;
        Ok(Self { root })
    }

    /// 运行根。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 条件使用的数据库路径。
    ///
    /// 参数:
    /// - `condition`: 运行条件；
    /// - `strict`: 是否按条件隔离。
    ///
    /// 返回:
    /// - 默认：`<root>/work/ramaria.db`；
    /// - 严格：`<root>/work/{slug}/ramaria.db`（条件标识的 `/` 替换为 `_`）。
    pub fn work_db(&self, condition: &Condition, strict: bool) -> PathBuf {
        let work = self.root.join(WORK_DIR);
        if strict {
            work.join(condition_slug(condition))
                .join(assembly::EVAL_DB_FILE)
        } else {
            work.join(assembly::EVAL_DB_FILE)
        }
    }

    /// 准备工作库：目标不存在才从基线复制，已存在直接复用。
    ///
    /// 参数:
    /// - `condition`: 运行条件；
    /// - `strict`: 是否按条件隔离；
    /// - `baseline_db`: 基线库路径。
    ///
    /// 返回:
    /// - 成功时返回工作库路径；
    /// - 目录创建或复制失败返回 `Io`（文案含源与目标路径）。
    pub fn prepare_work_db(
        &self,
        condition: &Condition,
        strict: bool,
        baseline_db: &Path,
    ) -> RamariaResult<PathBuf> {
        let target = self.work_db(condition, strict);
        if target.exists() {
            return Ok(target);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                RamariaError::io(
                    format!("创建工作库目录失败: {}", parent.display()),
                    Some(error),
                )
            })?;
        }
        std::fs::copy(baseline_db, &target).map_err(|error| {
            RamariaError::io(
                format!(
                    "复制基线库失败: {} -> {}",
                    baseline_db.display(),
                    target.display()
                ),
                Some(error),
            )
        })?;
        Ok(target)
    }

    /// 为条件装配引擎。
    ///
    /// 参数:
    /// - `condition`: 运行条件（管线、预算、答题模型）；
    /// - `strict`: 是否按条件隔离工作库；
    /// - `baseline`: 基线档位产物路径。
    ///
    /// 返回:
    /// - 成功时返回已应用答题模型的引擎，以及后端应用报告（来源 / 模型 / 连通性）；
    /// - 库复制、配置解析或引擎装配失败返回对应错误。
    ///
    /// 说明:
    /// - 配置以基线快照为基底（携带真实的检索 / 衰减 / 嵌入配置），只覆盖
    ///   路径、通道、主动对话、记忆条数上限与 utt 开关；
    /// - 条件快照写在工作库目录，不写回基线目录；
    /// - 答题模型经独立后端选项应用，不覆盖嵌入模型路径；
    /// - 后端降级只记日志与报告，不中止条件运行。
    pub async fn open_condition_engine(
        &self,
        condition: &Condition,
        strict: bool,
        baseline: &BaselineProduct,
    ) -> RamariaResult<(Engine, BackendReport)> {
        let db_path = self.prepare_work_db(condition, strict, &baseline.db)?;
        let work_dir = db_path.parent().ok_or_else(|| {
            RamariaError::validation(format!("工作库缺少父目录: {}", db_path.display()))
        })?;

        let mut config = read_baseline_config(&baseline.config)?;
        apply_condition_overrides(&mut config, work_dir, condition.pipeline);
        let snapshot = write_condition_config(work_dir, &config)?;

        let engine =
            Engine::open_with(EngineOptions::new(db_path.as_path()).with_config_path(&snapshot))
                .await?;

        let answer = &condition.answer_model;
        let choice = SandboxBackendChoice::Independent {
            kind: match answer.kind {
                AnswerModelKind::Local => SandboxModelKind::Local,
                AnswerModelKind::Cloud => SandboxModelKind::Cloud,
            },
            base_url: answer.base_url.clone(),
            model_id: answer.model.clone(),
            embedding_model_path: None,
        };
        let resolved = resolve_backend(&choice, None).await?;
        let (report, warnings) = apply_backend(&engine, resolved).await;
        for warning in warnings {
            tracing::warn!(condition = %condition.id, warning = %warning, "评测条件后端降级");
        }
        tracing::debug!(
            condition = %condition.id,
            model = %report.model,
            "评测条件后端已应用"
        );
        Ok((engine, report))
    }
}

/// 条件标识转目录名：分隔符 `/` 替换为 `_`。
fn condition_slug(condition: &Condition) -> String {
    condition.id.replace('/', "_")
}

// =========================================================
// 配置快照
// =========================================================

/// 读取基线配置快照。
fn read_baseline_config(path: &Path) -> RamariaResult<RamariaConfig> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        RamariaError::io(format!("读取基线配置失败: {}", path.display()), Some(error))
    })?;
    toml::from_str(&text).map_err(|error| RamariaError::Config {
        context: format!("解析基线配置失败: {}", path.display()),
        source: Some(Box::new(error)),
        trace_id: None,
    })
}

/// 覆盖条件级配置项。
///
/// 说明:
/// - 路径全部指向工作库所在目录，与评测装配的"库所在目录即数据根"一致；
/// - 通道与主动对话关闭，评测进程不外发消息；
/// - 记忆条数上限放开到 20，让字符预算成为唯一瓶颈；
/// - utt 通道保持开启，flat-rag 需要它，其余条件由注入掩码控闸门。
fn apply_condition_overrides(config: &mut RamariaConfig, work_dir: &Path, pipeline: Pipeline) {
    let data_dir = work_dir.to_string_lossy().into_owned();
    config.paths.data_dir = data_dir.clone();
    config.paths.config_dir = data_dir;
    config.paths.log_dir = work_dir.join("logs").to_string_lossy().into_owned();
    config.paths.vector_index_dir = work_dir.join("vectors").to_string_lossy().into_owned();

    config.channel.enabled = false;
    config.channel.qq_enabled = false;
    config.channel.telegram_enabled = false;
    config.channel.proactive_delivery_enabled = false;
    config.proactive.enabled = false;

    config.retrieval.rag_max_memories = RAG_MAX_MEMORIES;
    // 检索候选池与条数上限同宽：预算轴只由 max_chars 决定，避免 top_k 先截断。
    config.retrieval.l1_retrieve_top_k = RAG_MAX_MEMORIES;
    config.retrieval.l2_retrieve_top_k = RAG_MAX_MEMORIES;
    config.utt.retrieve_top_k = RAG_MAX_MEMORIES;
    config.utt.enabled = true;
    // flat-rag 只有原文通道，超预算块整块丢弃会让该条件在长块上完全无注入；
    // 放开块级预算，由请求的字符预算统一截断。
    if pipeline == Pipeline::FlatRag {
        config.utt.max_block_chars = FLAT_RAG_BLOCK_CHARS;
    }
}

/// 写出条件配置快照（先临时文件再改名）。
fn write_condition_config(dir: &Path, config: &RamariaConfig) -> RamariaResult<PathBuf> {
    let text = toml::to_string_pretty(config).map_err(|error| RamariaError::Config {
        context: "序列化条件配置快照失败".to_string(),
        source: Some(Box::new(error)),
        trace_id: None,
    })?;
    let path = dir.join(assembly::EVAL_CONFIG_FILE);
    let temp = dir.join(format!("{}.tmp", assembly::EVAL_CONFIG_FILE));
    std::fs::write(&temp, text).map_err(|error| {
        RamariaError::io(
            format!("写入条件配置临时文件失败: {}", temp.display()),
            Some(error),
        )
    })?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        RamariaError::io(
            format!("替换条件配置快照失败: {}", path.display()),
            Some(error),
        )
    })?;
    Ok(path)
}
