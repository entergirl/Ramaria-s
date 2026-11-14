//! crates/ramaria-eval/src/report/artifact.rs - Ramaria 评测报告产物结构
//!
//! 设计特点:
//! - 结构即契约：顶层、运行、条件、比较、结论与缺项一次定死，后续只填值不改键
//! - 指标列直接复用判分产物类型，报告不重新定义口径
//! - 未产出的列固定 `null`，缺项另记三态，不以 N/A 或零值顶替
//! - 结论状态只有 supported / partial / not_evidenced 三档
//! - 不写时间戳与机器信息，同一输入两次生成逐字节一致

use serde::{Deserialize, Serialize};

use crate::score::{
    CategoryScore, ConditionCredibility, ConditionUsage, JudgeStatus, PersonaSummary,
    RetrievalScore, RubricSummary,
};

/// 报告产物类型标识。
pub const REPORT_ARTIFACT_KIND: &str = "ramaria_eval_report";

/// 报告产物自身的 schema 版本。
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// 报告目录名（运行根下）。
pub const REPORT_DIR: &str = "report";

/// 报告 JSON 文件名。
pub const REPORT_FILE: &str = "report.json";

/// 报告 markdown 文件名。
pub const REPORT_MARKDOWN_FILE: &str = "report.md";

// =========================================================
// 顶层结构
// =========================================================

/// 一次运行的完整报告（`report.json`）。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物识别与版本；
/// - `run_id`: 主运行标识；
/// - `source`: 判分输入口径（逐题摘要、指标版本、判分模型）；
/// - `runs`: 参与报告的全部运行（主 / oracle / 对照）；
/// - `conditions`: 主运行的条件汇总，按条件标识字典序；
/// - `comparisons`: 配对比较，按标识字典序；
/// - `hypotheses`: H1~H6 结论，固定顺序；
/// - `missing`: 缺项三态，按范围与类别排序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportArtifact {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub run_id: String,
    pub source: ReportSource,
    pub runs: Vec<ReportRun>,
    pub conditions: Vec<ReportCondition>,
    pub comparisons: Vec<ComparisonRow>,
    pub hypotheses: Vec<Hypothesis>,
    pub missing: Vec<MissingFact>,
}

/// 报告的判分输入口径。
///
/// 字段约定:
/// - `items_sha256`: 判分输入的逐题产物摘要，来自 `score.json`；
/// - `ruleset_version`: 指标口径版本；
/// - `score_schema_version`: 判分产物 schema 版本；
/// - `judge`: 判分模型状态；判分产物缺失时按未启用记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportSource {
    pub items_sha256: String,
    pub ruleset_version: String,
    pub score_schema_version: u32,
    pub judge: JudgeStatus,
}

// =========================================================
// 运行
// =========================================================

/// 报告引用的运行角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunRole {
    /// 主运行
    Primary,
    /// 压缩层 oracle
    Oracle,
    /// `--compare` 挂载的对照运行
    Compare,
}

impl RunRole {
    /// 稳定小写标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Oracle => "oracle",
            Self::Compare => "compare",
        }
    }
}

/// 参与报告的一次运行。
///
/// 字段约定:
/// - `role` / `run_id` / `root`: 角色、标识与运行根路径；
/// - `tiers` / `pipelines` / `budgets`: 冻结矩阵的轴取值；
/// - `question_sample`: 每档分层抽样题数；未抽样为 `null`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRun {
    pub role: RunRole,
    pub run_id: String,
    pub root: String,
    pub tiers: Vec<String>,
    pub pipelines: Vec<String>,
    pub budgets: Vec<u32>,
    pub question_sample: Option<usize>,
}

// =========================================================
// 条件
// =========================================================

/// 主运行中一个条件的报告行。
///
/// 字段约定:
/// - `condition_id` / `tier` / `pipeline` / `budget_chars` / `answer_model`: 条件轴；
/// - `questions` / `scored` / `skipped_no_gold` / `failed_items` / `judge_failed`: 计数；
/// - `rubric` / `retrieval` / `persona` / `credibility`: 四类指标列；
/// - `usage`: 答案与判分两路用量；`usage.json` 缺失时为 `null`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportCondition {
    pub condition_id: String,
    pub tier: String,
    pub pipeline: String,
    pub budget_chars: u32,
    pub answer_model: String,
    pub questions: usize,
    pub scored: usize,
    pub skipped_no_gold: usize,
    pub failed_items: usize,
    pub judge_failed: usize,
    pub rubric: Option<ReportRubric>,
    pub retrieval: RetrievalScore,
    pub persona: Option<PersonaSummary>,
    pub credibility: ConditionCredibility,
    pub usage: Option<ConditionUsage>,
}

/// 报告中的分项要点汇总。
///
/// 字段约定:
/// - `micro_mean_ratio` / `micro_full_hit_rate`: 判分成功题的平均得分率与全要点命中率；
/// - `macro_by_category`: 固定 0..=8 共 9 项，多标签题计入它的每个类别；
/// - `disagreement_rate`: 非一致要点数 / 已判要点总数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportRubric {
    pub micro_mean_ratio: Option<f64>,
    pub micro_full_hit_rate: Option<f64>,
    pub macro_by_category: Vec<CategoryScore>,
    pub disagreement_rate: Option<f64>,
}

impl ReportRubric {
    /// 由判分产物的分项汇总构造。
    pub fn from_summary(summary: &RubricSummary) -> Self {
        Self {
            micro_mean_ratio: summary.micro_mean_ratio,
            micro_full_hit_rate: summary.micro_full_hit_rate,
            macro_by_category: summary.macro_by_category.clone(),
            disagreement_rate: summary.disagreement_rate,
        }
    }
}

// =========================================================
// 配对比较
// =========================================================

/// 一条配对比较。
///
/// 字段约定:
/// - `id`: `{tier}/layered-vs-{pipeline}`、`{tier}/oracle-l1-vs-layered`
///   或 `{tier}/{pipeline}/b{left}-vs-b{right}`；
/// - `tier`: 档位；
/// - `left` / `right`: 条件标识，差值方向为 `left - right`；
/// - `n_pairs`: 两侧都有有效 rubric 分的题数；
/// - `mean_diff` / `ci_low` / `ci_high` / `effect_dz`: 均值差、95% 置信区间与配对 d_z；
/// - `p_value` / `p_adjusted`: Wilcoxon 双尾 p 值与 BH-FDR 校正后 q 值；
/// - `significant`: 按 p_adj < 0.05 且 95% CI 不含 0 判定；d_z 只作效应量输出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonRow {
    pub id: String,
    pub tier: String,
    pub left: String,
    pub right: String,
    pub n_pairs: usize,
    pub mean_diff: Option<f64>,
    pub ci_low: Option<f64>,
    pub ci_high: Option<f64>,
    pub effect_dz: Option<f64>,
    pub p_value: Option<f64>,
    pub p_adjusted: Option<f64>,
    pub significant: bool,
}

// =========================================================
// 结论与缺项
// =========================================================

/// 结论状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    /// 证据支持
    Supported,
    /// 部分支持或仅满足弱条件
    Partial,
    /// 输入不足，未取证
    NotEvidenced,
}

impl HypothesisStatus {
    /// 稳定小写标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Partial => "partial",
            Self::NotEvidenced => "not_evidenced",
        }
    }
}

/// 一条假设的判定结果。
///
/// 字段约定:
/// - `id`: 假设编号（H1~H6）；
/// - `status`: 判定状态；
/// - `summary`: 一句结论；
/// - `evidence`: 支撑该结论的要点（数值与比较结果，不含题目正文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub status: HypothesisStatus,
    pub summary: String,
    pub evidence: Vec<String>,
}

/// 缺项类别（固定三态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MissingKind {
    /// 未执行
    #[serde(rename = "not_run")]
    NotRun,
    /// 执行失败或产物缺失
    #[serde(rename = "failed")]
    Failed,
    /// 本次口径不支持
    #[serde(rename = "unsupported")]
    Unsupported,
}

impl MissingKind {
    /// 稳定小写标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
        }
    }
}

/// 一条缺项说明。
///
/// 字段约定:
/// - `scope`: `report` 表示运行级，其余为条件标识；
/// - `kind`: 三态类别；
/// - `detail`: 人读说明（不含题目正文与回复）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingFact {
    pub scope: String,
    pub kind: MissingKind,
    pub detail: String,
}

impl MissingFact {
    /// 运行级缺项。
    pub fn report(kind: MissingKind, detail: impl Into<String>) -> Self {
        Self {
            scope: "report".to_string(),
            kind,
            detail: detail.into(),
        }
    }

    /// 条件级缺项。
    pub fn condition(condition_id: &str, kind: MissingKind, detail: impl Into<String>) -> Self {
        Self {
            scope: condition_id.to_string(),
            kind,
            detail: detail.into(),
        }
    }
}
