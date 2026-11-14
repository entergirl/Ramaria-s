//! crates/ramaria-eval/src/score/artifact.rs - Ramaria 评测判分产物结构
//!
//! 设计特点:
//! - 结构即契约：score.json 顶层、条件汇总与逐题行列一次定死，后续批次只填值不改键
//! - 不写时间戳与机器信息：同一输入两次判分逐字节一致，可直接做 diff
//! - 未产出的列固定 `null` 并在 `missing` 记一条，读方按缺项降级而不是报错
//! - 浮点保留原值，聚合为固定顺序的算术均值，不四舍五入
//! - 用量单独落 `usage.json`：属运行遥测，不进判分结果，缓存命中不产生用量
//! - 落盘先写临时文件再改名，中断不会留下半份 JSON

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::LlmUsage;
use serde::{Deserialize, Serialize};

use crate::run::AnswerModelKind;

use super::judge::TemplateHash;
use super::persona::PersonaScore;
use super::rubric::RubricScore;

/// 判分产物类型标识。
pub const SCORE_ARTIFACT_KIND: &str = "ramaria_eval_score";

/// 判分产物自身的 schema 版本。
///
/// 版本差异:
/// - 3: 判分列换成强类型（`rubric` / `persona` / `credibility.*_judge`），
///   检索列补精确率，`judge` 段带票数与模板摘要；
/// - 2: `retrieval.artifact` 为 artifact 级强类型指标（缺映射时为 `null`）；
/// - 1: `retrieval.artifact` 恒为 `null`，只有消息级口径。
pub const SCORE_SCHEMA_VERSION: u32 = 3;

/// 指标口径版本（指标定义或聚合方式变化时递增）。
pub const RULESET_VERSION: &str = "2026-11-01.2";

/// 判分产物目录名（运行根下）。
pub const SCORE_DIR: &str = "score";

/// 条件汇总文件名。
pub const SCORE_FILE: &str = "score.json";

/// 逐题产物文件名。
pub const SCORED_FILE: &str = "scored.jsonl";

/// 运行用量文件名（与判分结果分开落盘的遥测产物）。
pub const USAGE_FILE: &str = "usage.json";

/// 用量产物类型标识。
pub const USAGE_ARTIFACT_KIND: &str = "ramaria_eval_score_usage";

/// 用量产物自身的 schema 版本。
pub const USAGE_SCHEMA_VERSION: u32 = 1;

/// 缺项类别：工作库不存在，无法解析单元证据。
pub const MISSING_WORK_DB: &str = "work_db_missing";

/// 缺项类别：逐题产物不存在。
pub const MISSING_ITEMS: &str = "items_missing";

/// 缺项类别：judge 判分未接入。
pub const MISSING_JUDGE: &str = "judge_pending";

/// 缺项类别：artifact 级检索指标缺少 artifacts.json。
pub const MISSING_ARTIFACT: &str = "artifact_retrieval_pending";

/// 缺项类别：预置上下文条件（full-context / oracle-l1）没有召回轨迹，检索指标不适用。
pub const MISSING_PRESET_RECALL: &str = "preset_recall_absent";

/// 缺项类别：分项要点判分未启用。
pub const MISSING_RUBRIC: &str = "rubric_pending";

/// 缺项类别：CD 判分行待接入。
pub const MISSING_CD: &str = "cd_judge_pending";

/// 缺项类别：判分模型不可用（配置非法 / 密钥缺失 / 缓存不可读）。
pub const MISSING_JUDGE_UNAVAILABLE: &str = "judge_unavailable";

// =========================================================
// 顶层汇总
// =========================================================

/// 一次判分的完整产物（`score.json`）。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物识别与版本；
/// - `run_id`: 被判分的运行标识；
/// - `items_sha256`: 逐题产物字节摘要，用于判断判分输入是否被改写；
/// - `ruleset_version`: 指标口径版本；
/// - `judge`: 判分模型状态（未接入时固定 disabled）；
/// - `conditions`: 按条件标识字典序排列的条件汇总；
/// - `missing`: 运行级缺项（固定待接入项与产物缺失）。
///
/// 说明: 用量不在这里——它是运行遥测，单独写 `usage.json`，
/// 因此同一输入两次判分（含命中判分缓存）的 `score.json` 逐字节一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreReport {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub run_id: String,
    pub items_sha256: String,
    pub ruleset_version: String,
    pub judge: JudgeStatus,
    pub conditions: Vec<ConditionScore>,
    pub missing: Vec<MissingEntry>,
}

/// 判分模型状态。
///
/// 状态:
/// - `Disabled`: 本次只产出确定性指标，未调用判分模型；
/// - `Configured`: 判分模型已定案，`source` 说明它来自矩阵还是回退首个答题模型，
///   `templates` 记录本次用到的提示词摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JudgeStatus {
    Disabled,
    Configured {
        model: String,
        kind: AnswerModelKind,
        source: JudgeSource,
        /// 每题投票次数（语气维走单票入口）
        votes: usize,
        /// 采样温度
        temperature: f64,
        /// 本次判分使用的提示词摘要
        templates: Vec<TemplateHash>,
    },
}

/// 判分模型来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeSource {
    /// 矩阵 `[judge]` 段显式声明
    Matrix,
    /// 回退到 `answer_models[0]`
    AnswerModelFallback,
}

/// 一条缺项说明。
///
/// 字段约定:
/// - `scope`: 条件标识；运行级缺项为空串；
/// - `kind`: 稳定类别标识（用于机器分支）；
/// - `detail`: 人读说明（不含题目正文与回复）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingEntry {
    pub scope: String,
    pub kind: String,
    pub detail: String,
}

impl MissingEntry {
    /// 运行级缺项。
    pub fn run_level(kind: &str, detail: impl Into<String>) -> Self {
        Self {
            scope: String::new(),
            kind: kind.to_string(),
            detail: detail.into(),
        }
    }

    /// 条件级缺项。
    pub fn condition(condition_id: &str, kind: &str, detail: impl Into<String>) -> Self {
        Self {
            scope: condition_id.to_string(),
            kind: kind.to_string(),
            detail: detail.into(),
        }
    }
}

// =========================================================
// 用量汇总
// =========================================================

/// 某一路（答案 / 判分）的 token 用量汇总。
///
/// 字段约定:
/// - `calls`: 实际发出的模型调用次数（命中缓存不计）；
/// - `prompt_tokens` / `completion_tokens`: 输入 / 输出 token 合计；
/// - `cache_hit_tokens` / `cache_miss_tokens`: 输入中命中 / 未命中 provider 上下文缓存的合计；
/// - `cache_hit_rate`: `hit / (hit + miss)`；输入计为 0 时为 `None`（无用量信息）。
///
/// 说明: 缺缓存字段的 provider 按全 miss 记，因此命中率为 0 而不是 `None`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub calls: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    pub cache_hit_rate: Option<f64>,
}

impl UsageSummary {
    /// 由一次调用的用量构造（`calls` 为该路已发生的调用次数）。
    pub fn from_usage(calls: usize, usage: LlmUsage) -> Self {
        Self {
            calls,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_hit_tokens: usage.cache_hit_tokens,
            cache_miss_tokens: usage.cache_miss_tokens,
            cache_hit_rate: usage.cache_hit_rate(),
        }
    }

    /// 累加另一份汇总（逐字段相加后重算命中率）。
    pub fn add(&mut self, other: UsageSummary) {
        self.calls = self.calls.saturating_add(other.calls);
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.cache_hit_tokens = self.cache_hit_tokens.saturating_add(other.cache_hit_tokens);
        self.cache_miss_tokens = self
            .cache_miss_tokens
            .saturating_add(other.cache_miss_tokens);
        // 命中率按累加后的缓存字段重算（口径与 provider 上报时一致）。
        self.cache_hit_rate = LlmUsage {
            cache_hit_tokens: self.cache_hit_tokens,
            cache_miss_tokens: self.cache_miss_tokens,
            ..LlmUsage::default()
        }
        .cache_hit_rate();
    }

    /// 渲染为一行摘要：调用数 / 输入（命中，命中率）/ 输出。
    ///
    /// 参数:
    /// - `label`: 口径标签（`答案` / `判分`）。
    ///
    /// 返回:
    /// - 形如 `  用量（答案）: 调用 12 / 输入 3400（命中 1024，命中率 30.1%）/ 输出 900` 的一行；
    /// - 命中率无定义时显示 `未知`。
    pub fn render(&self, label: &str) -> String {
        let rate = match self.cache_hit_rate {
            Some(rate) => format!("{:.1}%", rate * 100.0),
            None => "未知".to_string(),
        };
        format!(
            "  用量（{label}）: 调用 {} / 输入 {}（命中 {}，命中率 {}）/ 输出 {}",
            self.calls, self.prompt_tokens, self.cache_hit_tokens, rate, self.completion_tokens
        )
    }
}

/// 条件 / 运行级用量：答案与判分两路分开记。
///
/// 字段约定:
/// - `answer`: 答题模型用量（逐题累加，provider 未上报的题不计）；
/// - `judge`: 判分模型用量（含三次投票与解析重试）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ConditionUsage {
    pub answer: UsageSummary,
    pub judge: UsageSummary,
}

impl ConditionUsage {
    /// 累加另一份用量（运行级合计用）。
    pub fn add(&mut self, other: ConditionUsage) {
        self.answer.add(other.answer);
        self.judge.add(other.judge);
    }
}

// =========================================================
// 用量产物
// =========================================================

/// 单条件的用量条目（`usage.json` 的 `conditions[]` 一项）。
///
/// 字段约定:
/// - `condition_id`: 条件标识；
/// - `answer` / `judge`: 该条件两路的用量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionUsageEntry {
    pub condition_id: String,
    pub answer: UsageSummary,
    pub judge: UsageSummary,
}

impl ConditionUsageEntry {
    /// 由条件标识与该条件用量构造。
    pub fn new(condition_id: &str, usage: ConditionUsage) -> Self {
        Self {
            condition_id: condition_id.to_string(),
            answer: usage.answer,
            judge: usage.judge,
        }
    }
}

/// 一次判分的用量产物（`usage.json`）。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物识别与版本；
/// - `run_id` / `items_sha256`: 与 `score.json` 同源，用于对齐同一批判分输入；
/// - `conditions`: 按条件标识字典序排列的单条件用量；
/// - `total`: 各条件之和。
///
/// 说明: 这是运行遥测而不是判分结果，不参与"同一输入逐字节一致"的约束——
/// 判分缓存命中的再次判分零调用，用量段随之变小。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub run_id: String,
    pub items_sha256: String,
    pub conditions: Vec<ConditionUsageEntry>,
    pub total: ConditionUsage,
}

// =========================================================
// 条件汇总
// =========================================================

/// 单个条件的判分汇总。
///
/// 字段约定:
/// - `questions`: 该条件去重后的题目数；
/// - `scored`: 参与确定性指标的题数（有 gold 且未失败）；
/// - `skipped_no_gold`: 不可回答题数（不计入均值，单独计数）；
/// - `failed_items`: 失败题数（执行失败不计成检索失败，单列）；
/// - `judge_failed`: 该条件内任一判分任务失败的题数；
/// - `retrieval` / `credibility` / `rubric` / `persona`: 四类指标列；
/// - `missing`: 该条件的缺项（如工作库不存在）。
///
/// 说明: `questions = scored + skipped_no_gold + failed_items`，三档互斥且失败优先；
/// `rubric` 只在有分项要点的题上产出，`judge_failed` 同时覆盖 UA / CD 判分失败。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionScore {
    pub condition_id: String,
    pub questions: usize,
    pub scored: usize,
    pub skipped_no_gold: usize,
    pub failed_items: usize,
    pub judge_failed: usize,
    pub retrieval: RetrievalScore,
    pub credibility: ConditionCredibility,
    pub rubric: Option<RubricSummary>,
    pub persona: Option<PersonaSummary>,
    pub missing: Vec<MissingEntry>,
}

/// 分项要点判分的条件汇总。
///
/// 字段约定:
/// - `questions`: 参与分项判分的题数（有要点且有回复）；
/// - `judged`: 判分成功、产出分值的题数（`questions - judged` 即失败题数）；
/// - `micro_mean_ratio`: 判分成功题的平均得分率；
/// - `micro_full_hit_rate`: 判分成功题中全部要点命中的比例；
/// - `macro_by_category`: 固定 0..=8 共 9 项，无题类别的 `mean_ratio` 为 `None`；
/// - `disagreement_rate`: 非一致要点数 / 已判要点总数（本条件累计）。
///
/// 说明: 宏观口径为 `macro_by_category` 中非 `None` 项的算术均值；多标签题计入它的每个类别。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RubricSummary {
    pub questions: usize,
    pub judged: usize,
    pub micro_mean_ratio: Option<f64>,
    pub micro_full_hit_rate: Option<f64>,
    pub macro_by_category: Vec<CategoryScore>,
    pub disagreement_rate: Option<f64>,
}

/// 单个类别的分项判分汇总。
///
/// 字段约定:
/// - `category`: 类别码（0..=8）；
/// - `questions`: 该类别下判分成功的题数（多标签题每个类别各计一次）；
/// - `mean_ratio`: 该类别平均得分率；无题时为 `None`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryScore {
    pub category: u8,
    pub questions: usize,
    pub mean_ratio: Option<f64>,
}

/// 人格表达三维判分的条件汇总。
///
/// 字段约定:
/// - `questions`: 参与三维判分的题数（有回复且未失败）；
/// - `tone` / `fact` / `emotion`: 三个维度各自的均值；该维无有效分时为 `None`；
/// - `fact_variant`: 事实维实际口径（`cosine+keyword` / `keyword`）；无事实维分时为空串；
/// - `tone_failed`: 语气维启用判分却无分值的题数（解析失败、调用失败或参考为空）。
///
/// 说明: 三维只作回归线，缺值即留空，不写缺项、不进主证据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaSummary {
    pub questions: usize,
    pub tone: Option<f64>,
    pub fact: Option<f64>,
    pub emotion: Option<f64>,
    pub fact_variant: String,
    pub tone_failed: usize,
}

/// 检索指标双口径。
///
/// 字段约定:
/// - `artifact`: artifact 级口径（报告主口径），需要 artifacts.json 才能建立消息到 artifact 的映射；
/// - `message`: 消息级口径（诊断口径），单元展开到消息后与 gold 求交。
///
/// 说明: 两列的参与题集合各按自身 gold 判定，空 gold 的题不进入该列均值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalScore {
    pub message: Option<MessageRetrieval>,
    pub artifact: Option<ArtifactRetrieval>,
}

/// artifact 级检索指标（条件均值）。
///
/// 字段约定:
/// - `questions`: 参与聚合的题数（gold artifact 非空且未失败）；
/// - `coverage_pre` / `coverage_post`: 预算前 / 预算后的 gold artifact 覆盖率均值；
/// - `mrr_pre` / `mrr_post`: 预算前 / 预算后首个命中单元名次的倒数额均值；
/// - `precision_pre` / `precision_post`: 命中 gold 的单元数 / 可映射到 artifact 的单元数（均值）；
/// - `delta_coverage`: `coverage_post - coverage_pre`；
/// - `units_pre_avg` / `units_post_avg`: 预算前 / 预算后平均可映射 artifact 数；
/// - `missing_gold_total`: 本题集累计缺失 evidence 数（语料 evidence 在该样本映射表里找不到）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRetrieval {
    pub questions: usize,
    pub coverage_pre: f64,
    pub coverage_post: f64,
    pub precision_pre: f64,
    pub precision_post: f64,
    pub mrr_pre: f64,
    pub mrr_post: f64,
    pub delta_coverage: f64,
    pub units_pre_avg: f64,
    pub units_post_avg: f64,
    pub missing_gold_total: usize,
}

/// 消息级检索指标（条件均值）。
///
/// 字段约定:
/// - `questions`: 参与聚合的题数；
/// - `coverage_pre` / `coverage_post`: 预算前 / 预算后的 gold 消息覆盖率均值；
/// - `mrr_pre` / `mrr_post`: 预算前 / 预算后首个命中单元名次的倒数额均值；
/// - `precision_pre` / `precision_post`: 命中 gold 的单元数 / 可映射单元数（均值）；
/// - `delta_coverage`: `coverage_post - coverage_pre`；
/// - `units_pre_avg` / `units_post_avg`: 预算前 / 预算后平均单元数；
/// - `unmappable_total`: 不可映射单元累计数（graph / 未知前缀 / 解析失败）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRetrieval {
    pub questions: usize,
    pub coverage_pre: f64,
    pub coverage_post: f64,
    pub precision_pre: f64,
    pub precision_post: f64,
    pub mrr_pre: f64,
    pub mrr_post: f64,
    pub delta_coverage: f64,
    pub units_pre_avg: f64,
    pub units_post_avg: f64,
    pub unmappable_total: usize,
}

/// 可信度列（条件级）。
///
/// 字段约定:
/// - `ua_answered`: 有回复的题数（UA 判定的分母）；
/// - `ua_refusal_hits`: 命中拒答标记的题数（确定性交叉列，不是判定线）；
/// - `ua_refusal_rate`: `hits / answered`；无回复时为 `None`；
/// - `cd_questions`: 冲突题数（CD 判分分母）；
/// - `cd_false_premise_accept_rate`: 冲突题被接受错误前提的比例（取 `cd_judge.rate` 的简写）；
/// - `ua_judge` / `cd_judge`: judge 判定结果；判分未接入或本题集无有效判定时为 `None`。
///
/// 说明: 确定性列（`ua_refusal_*`）只做字面交叉，判定线以 `*_judge` 为准。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionCredibility {
    pub ua_answered: usize,
    pub ua_refusal_hits: usize,
    pub ua_refusal_rate: Option<f64>,
    pub cd_questions: usize,
    pub cd_false_premise_accept_rate: Option<f64>,
    pub ua_judge: Option<JudgeRate>,
    pub cd_judge: Option<JudgeRate>,
}

/// 单类判定的条件汇总。
///
/// 字段约定:
/// - `judged`: 有有效判定的题数（判分失败不进分母）；
/// - `hits`: 命中数（UA = 判为拒答，CD = 判为接受错误前提）；
/// - `rate`: `hits / judged`；
/// - `disagreement_rate`: 票面不一致的题数 / `judged`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeRate {
    pub judged: usize,
    pub hits: usize,
    pub rate: f64,
    pub disagreement_rate: f64,
}

// =========================================================
// 逐题产物
// =========================================================

/// 一道题在某个条件上的判分结果（`scored.jsonl` 一行）。
///
/// 字段约定:
/// - `question_id` / `condition_id`: 复合键；
/// - `tier` / `pipeline` / `budget_chars` / `answer_model`: 条件轴；
/// - `sample_key` / `category` / `gold_count`: 题目口径；
/// - `retrieval`: 消息级指标；未参与聚合（空 gold / 失败 / 缺库）时为 `None`；
/// - `credibility`: UA / CD 交叉列与 judge 判定；
/// - `rubric`: 分项要点判分；无要点或无回复时为 `None`；
/// - `persona`: 人格表达三维；无回复或执行失败的题为 `None`；
/// - `notes`: 数据侧提示（如旧产物缺分项原料），不表示错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredItem {
    pub question_id: String,
    pub condition_id: String,
    pub tier: String,
    pub pipeline: String,
    pub budget_chars: u32,
    pub answer_model: String,
    pub sample_key: String,
    pub category: Vec<u8>,
    pub gold_count: usize,
    pub retrieval: Option<ItemRetrieval>,
    pub credibility: ItemCredibility,
    pub rubric: Option<RubricScore>,
    pub persona: Option<PersonaScore>,
    pub notes: Vec<String>,
}

/// 单题消息级检索指标。
///
/// 字段约定:
/// - `coverage_pre` / `coverage_post`: gold 消息被预算前 / 预算后单元集合覆盖的比例；
/// - `precision_pre` / `precision_post`: 命中 gold 的单元数 / 可映射单元数（无单元时为 0）；
/// - `rank_pre` / `rank_post`: 首个命中单元在集合中的位置（1 起）；未命中为 `None`；
/// - `units_pre` / `units_post`: 两个集合的单元数（含不可映射单元）；
/// - `unmappable_pre` / `unmappable_post`: 不可映射单元数（不参与覆盖率与精确率分子分母）；
/// - `artifact`: artifact 级口径；缺 artifacts.json 或 gold artifact 为空时为 `None`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemRetrieval {
    pub coverage_pre: f64,
    pub coverage_post: f64,
    pub precision_pre: f64,
    pub precision_post: f64,
    pub rank_pre: Option<usize>,
    pub rank_post: Option<usize>,
    pub units_pre: usize,
    pub units_post: usize,
    pub unmappable_pre: usize,
    pub unmappable_post: usize,
    pub artifact: Option<ItemArtifactRetrieval>,
}

/// 单题 artifact 级检索指标。
///
/// 字段约定:
/// - `coverage_pre` / `coverage_post`: gold artifact 被预算前 / 预算后单元覆盖的比例；
/// - `precision_pre` / `precision_post`: 命中 gold 的单元数 / 可映射到 artifact 的单元数；
/// - `rank_pre` / `rank_post`: 首个命中单元在集合中的位置（1 起）；未命中为 `None`；
/// - `gold_artifacts`: 能在该样本映射表里找到的 gold artifact 数（覆盖率分母）；
/// - `missing_gold`: 该题 evidence 中不在映射表里的条数（不进分母，单独计数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemArtifactRetrieval {
    pub coverage_pre: f64,
    pub coverage_post: f64,
    pub precision_pre: f64,
    pub precision_post: f64,
    pub rank_pre: Option<usize>,
    pub rank_post: Option<usize>,
    pub gold_artifacts: usize,
    pub missing_gold: usize,
}

/// 单题可信度交叉列。
///
/// 字段约定:
/// - `ua_refusal_hint`: 不可回答题的回复是否命中拒答标记；非该题型与无回复时为 `None`；
/// - `cd_score_points`: 冲突题的分项要点数（CD 判分分母）；其他题型为 0；
/// - `ua_judge` / `cd_judge`: judge 判定；非该题型、无回复或判分失败时为 `None`；
/// - `judge_failed`: 本题任一判分任务失败（有效票不足两票）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCredibility {
    pub ua_refusal_hint: Option<bool>,
    pub cd_score_points: usize,
    pub ua_judge: Option<bool>,
    pub cd_judge: Option<bool>,
    pub judge_failed: bool,
}

// =========================================================
// 渲染与落盘
// =========================================================

impl ScoreReport {
    /// 运行根 → 判分产物目录。
    pub fn dir_of(run_root: &Path) -> PathBuf {
        run_root.join(SCORE_DIR)
    }

    /// 渲染为人读摘要（路径 + 计数 + 两行用量）。
    ///
    /// 参数:
    /// - `dir`: 判分产物目录；
    /// - `usage`: 本次运行级用量，来自 `usage.json` 的内存结构（遥测，不属于判分结果）。
    pub fn render(&self, dir: &Path, usage: &ConditionUsage) -> String {
        let questions: usize = self.conditions.iter().map(|item| item.questions).sum();
        let scored: usize = self.conditions.iter().map(|item| item.scored).sum();
        let no_gold: usize = self
            .conditions
            .iter()
            .map(|item| item.skipped_no_gold)
            .sum();
        let failed: usize = self.conditions.iter().map(|item| item.failed_items).sum();

        let mut output = String::new();
        let _ = writeln!(output, "判分 {} 完成:", self.run_id);
        let _ = writeln!(output, "  产物: {}", dir.display());
        let _ = writeln!(
            output,
            "  条件: {} / 题目: 计划 {} / 参与判分 {} / 无 gold {} / 失败 {}",
            self.conditions.len(),
            questions,
            scored,
            no_gold,
            failed
        );
        let _ = writeln!(output, "{}", usage.answer.render("答案"));
        let _ = writeln!(output, "{}", usage.judge.render("判分"));
        if !self.missing.is_empty() {
            let _ = writeln!(output, "  缺项 {} 条:", self.missing.len());
            for entry in &self.missing {
                let _ = writeln!(output, "    - {}: {}", entry.kind, entry.detail);
            }
        }
        output
    }
}

/// 原子写盘：先写 `{name}.tmp` 再改名，避免中断留下半份文件。
///
/// 参数:
/// - `dir`: 目标目录（不存在时创建）；
/// - `name`: 文件名；
/// - `body`: 完整文本。
///
/// 返回:
/// - 成功时返回目标路径；目录创建、写入或改名失败返回 `Io`。
pub fn write_atomic(dir: &Path, name: &str, body: &str) -> RamariaResult<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|error| {
        RamariaError::io(
            format!("创建判分产物目录失败: {}", dir.display()),
            Some(error),
        )
    })?;
    let path = dir.join(name);
    let temp = dir.join(format!("{name}.tmp"));
    std::fs::write(&temp, body).map_err(|error| {
        RamariaError::io(
            format!("写入判分产物临时文件失败: {}", temp.display()),
            Some(error),
        )
    })?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        RamariaError::io(format!("替换判分产物失败: {}", path.display()), Some(error))
    })?;
    Ok(path)
}
