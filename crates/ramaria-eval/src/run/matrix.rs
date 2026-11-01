//! crates/ramaria-eval/src/run/matrix.rs - Ramaria 评测矩阵定义模块
//!
//! 设计特点:
//! - 矩阵按 TOML 声明：四个轴与可选参数显式写全，缺省值只在字段级定义
//! - 条件标识由档位、管线、预算与模型序号拼成，展开顺序固定且可复现
//! - 管线枚举固定七种消融形态，掩码只在评测侧做减法，不改变产品默认
//! - 解析失败一律返回校验错误，文案带文件路径与字段名
//! - 只描述数据，不装配引擎、不读取语料

use std::path::Path;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::EvalInjectionMask;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// 单题失败重试次数的缺省值。
pub const DEFAULT_RETRY_LIMIT: u32 = 2;

// =========================================================
// 矩阵结构
// =========================================================

/// 一次批量评测的矩阵定义。
///
/// 字段约定:
/// - `run`: 运行轴（语料、档位、管线、预算与运行选项）；
/// - `answer_models`: 答题模型列表，条件按序号引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMatrix {
    pub run: RunSection,
    pub answer_models: Vec<AnswerModel>,
}

/// 运行轴与运行选项。
///
/// 字段约定:
/// - `corpus` / `language`: 语料登记名与语言标签；
/// - `tiers`: 基线档位标识，非空；
/// - `pipelines`: 管线组合，非空；
/// - `budgets`: 注入预算（字符），每项大于 0；
/// - `question_limit`: 每档题集截断上限，`None` 表示不限；
/// - `strict_isolation`: 是否按条件复制基线库；
/// - `retry_limit`: 单题失败重试次数，允许 0。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSection {
    pub corpus: String,
    pub language: String,
    pub tiers: Vec<String>,
    pub pipelines: Vec<Pipeline>,
    pub budgets: Vec<u32>,
    #[serde(default)]
    pub question_limit: Option<usize>,
    #[serde(default)]
    pub strict_isolation: bool,
    #[serde(default = "default_retry_limit")]
    pub retry_limit: u32,
}

/// 答题模型声明。
///
/// 字段约定:
/// - `kind`: 本地或云端；
/// - `base_url`: 模型服务地址（云端必填）；
/// - `model`: 模型标识（云端必填）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerModel {
    pub kind: AnswerModelKind,
    pub base_url: String,
    pub model: String,
}

/// 答题模型类别（可见文本只区分本地与云端）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnswerModelKind {
    /// 本地模型
    Local,
    /// 云端模型
    Cloud,
}

/// 注入管线组合。
///
/// 状态:
/// - 七个变体覆盖分层全开、逐层消融、flat-RAG 与 full-context；
/// - `mask()` 只做减法：开启不等于必然注入，配置闸门与检索命中照常生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pipeline {
    /// 九层全开（等价产品默认注入面）
    #[serde(rename = "layered")]
    Layered,
    /// 只开 L1 摘要
    #[serde(rename = "l1")]
    L1,
    /// L1 + L2
    #[serde(rename = "l1-l2")]
    L1L2,
    /// L1 + L2 + L3
    #[serde(rename = "l1-l2-l3")]
    L1L2L3,
    /// L1 + L2 + L3 + utt
    #[serde(rename = "l1-l2-l3-utt")]
    L1L2L3Utt,
    /// 只开 utt（平面片段 RAG）
    #[serde(rename = "flat-rag")]
    FlatRag,
    /// 不召回，由运行器注入全量上下文
    #[serde(rename = "full-context")]
    FullContext,
}

impl Pipeline {
    /// 稳定小写标识（条件标识与产物字段使用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Layered => "layered",
            Self::L1 => "l1",
            Self::L1L2 => "l1-l2",
            Self::L1L2L3 => "l1-l2-l3",
            Self::L1L2L3Utt => "l1-l2-l3-utt",
            Self::FlatRag => "flat-rag",
            Self::FullContext => "full-context",
        }
    }

    /// 该管线对应的注入层掩码。
    ///
    /// 返回:
    /// - 记忆四路按变体开关，其余注入层（知识 / 脉络 / 行为 / 风格 / 示例）关闭；
    /// - `Layered` 返回 `EvalInjectionMask::all()`；
    /// - `FullContext` 返回全关掩码（上下文由运行器另行注入）。
    pub fn mask(&self) -> EvalInjectionMask {
        match self {
            Self::Layered => EvalInjectionMask::all(),
            Self::L1 => memory_mask(true, false, false, false),
            Self::L1L2 => memory_mask(true, true, false, false),
            Self::L1L2L3 => memory_mask(true, true, true, false),
            Self::L1L2L3Utt => memory_mask(true, true, true, true),
            Self::FlatRag => memory_mask(false, false, false, true),
            Self::FullContext => memory_mask(false, false, false, false),
        }
    }
}

/// 只开记忆四路的掩码，其余注入层一律关闭。
fn memory_mask(l1: bool, l2: bool, l3: bool, utt: bool) -> EvalInjectionMask {
    EvalInjectionMask {
        l1,
        l2,
        l3,
        utt,
        knowledge: false,
        narrative: false,
        behavior: false,
        style: false,
        examples: false,
    }
}

/// 一个可运行的条件。
///
/// 字段约定:
/// - `id`: `{tier}/{pipeline}/{budget}/{model_index}`，矩阵内唯一；
/// - `tier`: 基线档位标识；
/// - `pipeline`: 注入管线组合；
/// - `budget_chars`: 注入预算（字符）；
/// - `answer_model`: 该条件使用的答题模型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub id: String,
    pub tier: String,
    pub pipeline: Pipeline,
    pub budget_chars: u32,
    pub answer_model: AnswerModel,
}

// =========================================================
// 展开与加载
// =========================================================

impl RunMatrix {
    /// 按档位 → 管线 → 预算 → 模型顺序展开全部条件。
    ///
    /// 说明:
    /// - 模型序号从 0 起，与 `answer_models` 下标一致；
    /// - 展开只做组合，不校验档位产物是否存在（由题目枚举阶段负责）。
    pub fn conditions(&self) -> Vec<Condition> {
        let mut conditions = Vec::new();
        for tier in &self.run.tiers {
            for pipeline in &self.run.pipelines {
                for budget in &self.run.budgets {
                    for (index, answer_model) in self.answer_models.iter().enumerate() {
                        conditions.push(Condition {
                            id: format!("{tier}/{}/{budget}/{index}", pipeline.as_str()),
                            tier: tier.clone(),
                            pipeline: *pipeline,
                            budget_chars: *budget,
                            answer_model: answer_model.clone(),
                        });
                    }
                }
            }
        }
        conditions
    }

    /// 读取并校验矩阵定义。
    ///
    /// 参数:
    /// - `path`: TOML 文件路径。
    ///
    /// 返回:
    /// - 成功时返回通过校验的矩阵；
    /// - 读取失败返回 `Io`；解析或校验失败返回 `Validation`，文案带文件路径与字段名。
    pub fn load(path: &Path) -> RamariaResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            RamariaError::io(format!("读取矩阵定义失败: {}", path.display()), Some(error))
        })?;
        let matrix: Self = toml::from_str(&text).map_err(|error| {
            RamariaError::validation(format!("解析矩阵定义失败: {}: {error}", path.display()))
        })?;
        matrix.validate().map_err(|reason| {
            RamariaError::validation(format!("矩阵定义非法: {}: {reason}", path.display()))
        })?;
        Ok(matrix)
    }

    /// 校验轴的取值范围与答题模型必填项。
    ///
    /// 说明:
    /// - 校验失败返回字段级原因，由 `load` 补上文件路径；
    /// - 不校验语料是否存在、档位产物是否已构建。
    fn validate(&self) -> Result<(), String> {
        let run = &self.run;
        if run.tiers.is_empty() {
            return Err("run.tiers 不能为空".to_string());
        }
        if run.pipelines.is_empty() {
            return Err("run.pipelines 不能为空".to_string());
        }
        if run.budgets.is_empty() {
            return Err("run.budgets 不能为空".to_string());
        }
        if run.budgets.contains(&0) {
            return Err("run.budgets 每项必须大于 0".to_string());
        }
        if run.question_limit == Some(0) {
            return Err("run.question_limit 必须大于 0".to_string());
        }
        for (index, answer_model) in self.answer_models.iter().enumerate() {
            if answer_model.kind != AnswerModelKind::Cloud {
                continue;
            }
            if answer_model.base_url.trim().is_empty() {
                return Err(format!("answer_models[{index}].base_url 不能为空"));
            }
            if answer_model.model.trim().is_empty() {
                return Err(format!("answer_models[{index}].model 不能为空"));
            }
        }
        Ok(())
    }
}

/// serde 缺省：重试次数。
fn default_retry_limit() -> u32 {
    DEFAULT_RETRY_LIMIT
}
