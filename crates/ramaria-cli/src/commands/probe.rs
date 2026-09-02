//! crates/ramaria-cli/src/commands/probe.rs - 探针 CLI 命令（probe build / probe run）
//!
//! 设计特点:
//! - `probe build`：从导入数据自动构建测试集（问题 × 参数档位组合，seed 固定可复跑），
//!   输出结构化 JSON 数据集；`dataset` 保留为 alias。
//! - `probe run`：按档位批量跑对话管线，结构化输出（档位 → 输出 → 指标），
//!   供 v1.6 T2 自动评分（evaluate/report）与 v1.7 T3 正式评估复用同一工具链。
//! - 探针规模：3 维（语气模仿 tone / 事实记忆 fact / 情感表达 emotion）× 每维 10 题，
//!   正式评估可通过 `--questions-per-dim` 扩大（v1.7 ≥ 30 题）。
//! - 档位为「代表配对」：baseline + 每次只动一个参数，
//!   θ_gap / 条数上限 / top_k 各 2 档，便于归因单参数影响。
//! - 静默降级：无真实数据 / 数据源查询失败 / 文件解析失败 → 内置测试夹具数据兜底
//!   （不向用户抛错，记 warn 后继续，M2 验收要求）。
//! - 确定性抽样：内置 xorshift64* 伪随机（无外部依赖），seed 相同则测试集完全一致。
//! - 隐私红线：数据集仅含问题与参考文本（评估所需），不包含完整对话原文；
//!   日志不记录完整问题与回复（用 id / 长度代替）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::{ChatRequest, EmbeddingProvider, LlmProvider};
use ramaria_core::types::{MessageRole, PersonaKind, now_ms};
use ramaria_memory::utt::builder::UttBuilder;
use uuid::Uuid;

// =========================================================
// 常量与档位定义
// =========================================================

/// 数据集 schema 版本（结构变更时递增，run 侧校验兼容性）。
const DATASET_SCHEMA_VERSION: u32 = 1;

/// 默认探针 seed（固定值，保证 `probe build` 默认输出可复跑）。
pub const DEFAULT_SEED: u64 = 2026_0810;

/// 默认每维题数（3 维 × 10 题预跑）。
pub const DEFAULT_QUESTIONS_PER_DIM: usize = 10;

/// 默认目标 persona（无白名单 persona 时的兜底；fixture 数据即以此 persona 编写）。
const DEFAULT_PERSONA: &str = "char-0001";

// =========================================================
// 消融档位 Profile（M5a，D-V17-015 / 技术报告 §16.3）
// =========================================================

/// 消融档位 Profile 名称集合（数据集 `variants[].ablation` 可取值）。
///
/// 语义（技术报告 §16.3，口径与 D-V17-010 一致）:
/// - `B0`: 基线 A——无记忆注入（纯角色 + 当前对话）。
/// - `B1`: 基线 B——压缩视图注入（仅摘要/转述 RAG，无原文无行为无知识）。
/// - `F0`: 完整体系（行为+知识+表达+脉络全开，等同 ablation=None）。
/// - `F1`~`F4`: 逐层关闭（−行为 / −知识 / −表达 / −脉络）。
/// - `S_behavior` / `S_knowledge` / `S_expression` / `S_narrative`:
///   前置单层验证——B1 基础上只单独注入该层（对照 B1 判定每层自身贡献）。
pub const ABLATION_PROFILE_NAMES: [&str; 11] = [
    "B0",
    "B1",
    "F0",
    "F1",
    "F2",
    "F3",
    "F4",
    "S_behavior",
    "S_knowledge",
    "S_expression",
    "S_narrative",
];

/// 消融档位 Profile。
///
/// 职责:
/// - 把技术报告 §16.3 的消融档位映射为 `RamariaConfig.injection`（注入层闸门）覆盖集，
///   使 B0/B1/F0/F1~F4/S_* 在单次 `send_message` 内真实关闭/保留对应注入层。
/// - `F0` 覆盖集为空（全开），与 `ablation=None` 行为完全一致（向后兼容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AblationProfile {
    /// 基线 A：无记忆注入（纯角色 + 当前对话）。
    B0,
    /// 基线 B：压缩视图注入（仅 RAG 摘要/转述，无原文无行为无知识）。
    B1,
    /// 完整体系（行为+知识+表达+脉络全开）。
    F0,
    /// −行为层（关闭行为规则注入）。
    F1,
    /// −知识层（关闭事实卡片注入）。
    F2,
    /// −表达层（关闭原文样例与风格规则）。
    F3,
    /// −脉络层（关闭近期脉络与桥接）。
    F4,
    /// 前置单层：B1 基础上单独注入行为层。
    SBehavior,
    /// 前置单层：B1 基础上单独注入知识层。
    SKnowledge,
    /// 前置单层：B1 基础上单独注入表达层。
    SExpression,
    /// 前置单层：B1 基础上单独注入脉络层。
    SNarrative,
}

impl AblationProfile {
    /// 解析档位名称（数据集 `variants[].ablation` 取值）。
    pub fn parse_name(name: &str) -> Option<Self> {
        match name {
            "B0" => Some(Self::B0),
            "B1" => Some(Self::B1),
            "F0" => Some(Self::F0),
            "F1" => Some(Self::F1),
            "F2" => Some(Self::F2),
            "F3" => Some(Self::F3),
            "F4" => Some(Self::F4),
            "S_behavior" => Some(Self::SBehavior),
            "S_knowledge" => Some(Self::SKnowledge),
            "S_expression" => Some(Self::SExpression),
            "S_narrative" => Some(Self::SNarrative),
            _ => None,
        }
    }

    /// 返回档位名称（与数据集字段一致）。
    pub fn name(self) -> &'static str {
        match self {
            Self::B0 => "B0",
            Self::B1 => "B1",
            Self::F0 => "F0",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::SBehavior => "S_behavior",
            Self::SKnowledge => "S_knowledge",
            Self::SExpression => "S_expression",
            Self::SNarrative => "S_narrative",
        }
    }

    /// 返回人类可读描述（用于档位 description / 报告标注）。
    pub fn description(self) -> &'static str {
        match self {
            Self::B0 => "基线 A：无记忆注入（纯角色 + 当前对话）",
            Self::B1 => "基线 B：压缩视图注入（仅 RAG 摘要/转述，无原文无行为无知识）",
            Self::F0 => "完整体系（行为+知识+表达+脉络全开）",
            Self::F1 => "−行为层（关闭行为规则注入）",
            Self::F2 => "−知识层（关闭事实卡片注入）",
            Self::F3 => "−表达层（关闭原文样例与风格规则）",
            Self::F4 => "−脉络层（关闭近期脉络与桥接）",
            Self::SBehavior => "前置单层：仅注入行为层（对照 B1）",
            Self::SKnowledge => "前置单层：仅注入知识层（对照 B1）",
            Self::SExpression => "前置单层：仅注入表达层（对照 B1）",
            Self::SNarrative => "前置单层：仅注入脉络层（对照 B1）",
        }
    }

    /// 把本档位映射为 `RamariaConfig.injection`（注入层闸门）覆盖集。
    ///
    /// 说明:
    /// - `F0`/`ablation=None` → 全开（与 M1 行为完全一致，回归红线）。
    /// - `F1`~`F4` → 在全开基础上关闭对应层。
    /// - `B0`/`B1`/`S_*` → 按"基座 + 单层"语义显式设置各闸门。
    pub fn apply_to(self, config: &mut ramaria_core::config::RamariaConfig) {
        use ramaria_core::config::InjectionGate;
        let g = &mut config.injection;
        match self {
            Self::B0 => {
                // 无记忆注入：除 persona 角色与当前对话外全部关闭。
                *g = InjectionGate::all_off();
            }
            Self::B1 => {
                // 压缩摘要基座：仅保留 RAG 相关记忆（经 ChatRequest 注入），
                // 关闭行为/知识/表达（风格+示例+原文）/脉络（近期脉络+桥接）。
                *g = InjectionGate::all_off();
                g.memory_rag = true;
            }
            Self::F0 => {
                // 完整体系 = 全开（与 None 等同）。
                *g = InjectionGate::all_on();
            }
            Self::F1 => {
                g.behavior = false;
            }
            Self::F2 => {
                g.knowledge = false;
            }
            Self::F3 => {
                // −表达层：说话风格/风格规则 + 对话示例 + utt 原文样例。
                g.speaking_style = false;
                g.examples = false;
                g.utt = false;
            }
            Self::F4 => {
                // −脉络层：近期对话脉络 + 桥接。
                g.narrative = false;
                g.bridge = false;
            }
            Self::SBehavior => {
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.behavior = true;
            }
            Self::SKnowledge => {
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.knowledge = true;
            }
            Self::SExpression => {
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.speaking_style = true;
                g.examples = true;
                g.utt = true;
            }
            Self::SNarrative => {
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.narrative = true;
                g.bridge = true;
            }
        }
    }
}

/// 构建消融档位 Profile 变体集合（F0 基线与 F1~F4、B0/B1、S_* 单层）。
///
/// 说明:
/// - utt 三参数取定稿基准（θ_gap=10 / 条数=80 / top_k=3，与 `baseline` 档位一致），
///   消融只改变记忆注入层（`ablation` 字段），不改变 utt 切分。
/// - 供 M5b 数据集构建方把消融档位并入数据集 variants；M1 默认数据集不包含。
pub fn ablation_variants() -> Vec<ProbeVariant> {
    ABLATION_PROFILE_NAMES
        .iter()
        .map(|name| {
            let profile = AblationProfile::parse_name(name)
                .expect("ABLATION_PROFILE_NAMES 必须与 parse_name 一一对应");
            ProbeVariant {
                id: profile.name().to_string(),
                description: format!("[消融] {}", profile.description()),
                theta_gap_minutes: 10,
                max_msgs_per_block: 80,
                retrieve_top_k: 3,
                ablation: Some(profile.name().to_string()),
            }
        })
        .collect()
}

// =========================================================
// 公共数据类型（数据集 / 结果，均需可序列化供 --json 输出与落盘）
// =========================================================

/// 探针测试集（`probe build` 的产物，`probe run` 的输入）。
///
/// 格式:
/// - `items`: 测试问题列表（tone / fact / emotion 三维）。
/// - `variants`: 参数档位组合（代表配对），run 时逐档位实验。
/// - `seed` / `questions_per_dimension`: 复跑参数（同 seed 同输出）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeDataset {
    pub schema_version: u32,
    pub seed: u64,
    pub persona_uid: String,
    pub dimensions: Vec<String>,
    pub questions_per_dimension: usize,
    /// 主数据来源: "db"（至少部分来自数据库）/ "file" / "fixture"（全部兜底）
    pub source: String,
    /// 生成时间（ISO-8601 UTC）
    pub generated_at: String,
    pub variants: Vec<ProbeVariant>,
    pub items: Vec<DatasetItem>,
}

/// 单条测试问题。
///
/// 字段约定:
/// - `id`: 维度内序号（`tone-0001` / `fact-0001`），稳定标识供结果关联。
/// - `question`: 发给对话管线的问题文本。
/// - `reference`: 参考回答（tone 为 persona 原回复、fact 为事件摘要），
///   仅作人工/自动评分参考，不注入对话管线。
/// - `source`: "db"（来自真实导入数据）或 "fixture"（内置夹具补齐）。
/// - `source_ref`: 溯源标识（session 或事件），便于回查原始数据，不记录原文。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DatasetItem {
    pub id: String,
    pub dimension: String,
    pub question: String,
    pub reference: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
}

/// 参数档位（代表配对：baseline 为 v3.1 初值，其余每次只动一个参数）。
///
/// 字段与 `[utt]` 配置组一一对应（theta_gap_minutes / max_msgs_per_block / retrieve_top_k）。
///
/// M5a 消融扩展:
/// - `ablation`: 可选消融档位 Profile 名称（`AblationProfile`）。
///   `None`（默认/旧数据集）→ 行为与 M1 完全一致（utr 三参数覆盖，无层闸门）。
///   序列化时省略（向后兼容：M1 旧数据集文件不受影响）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeVariant {
    pub id: String,
    pub description: String,
    pub theta_gap_minutes: u32,
    pub max_msgs_per_block: u32,
    pub retrieve_top_k: u32,
    /// 消融档位 Profile 名称（可选；None = 完整体系/无消融）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ablation: Option<String>,
}

/// 探针实验结果（`probe run` 的输出；evaluate/report 读取用）。
///
/// 字段约定:
/// - `variants`: 主实验明细（无 `--repeat` 时即单次结果；带 `--repeat N` 时为最后一次运行明细，供 evaluate/report 语义评分复用）。
/// - `repeat`: 统计法多次运行（`--repeat N`）的逐次聚合（均值 ± 置信区间）；未使用 `--repeat` 时为 None。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeExperiment {
    pub dataset_file: String,
    pub dataset_seed: u64,
    pub persona_uid: String,
    pub rebuild_utt: bool,
    pub variants: Vec<ProbeVariantResult>,
    /// 统计法（--repeat N）聚合结果；未指定时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<ProbeRepeatMeta>,
    pub generated_at: String,
}

/// 统计法多次运行的聚合元数据（`probe run --repeat N`）。
///
/// 格式:
/// - `count`: 重复次数 N。
/// - `per_variant`: 每个档位的逐题统计（跨 N 次）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeRepeatMeta {
    pub count: usize,
    pub per_variant: Vec<VariantRepeatStats>,
}

/// 单档位跨多次运行的逐题统计。
///
/// 说明（缺口 A 第一步，M1 报告 §6 登记项）:
/// - `per_item`: 每题跨 N 次运行的字符数/耗时均值 ± 置信区间（累积统计摘要）。
/// - `rounds`: 该档位在**每一轮**运行时的完整结果明细（`ProbeVariantResult`，逐轮全量
///   reply 保留于此）。供 evaluate 对"每一轮 reply"分别语义评分后，跨 N 轮聚合
///   fact_score 的均值 ± 置信区间（M5-005 配对统计口径），不再只评最后一轮。
/// - 向后兼容: `rounds` 序列化时为空则省略（`skip_serializing_if`）；缺失（`serde_default`）
///   反序列化时为空，旧实验文件（无逐轮明细）仍可正常读取（其 repeat 块无逐轮 reply，
///   对应的逐轮评分聚合不可用，属预期降级）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VariantRepeatStats {
    pub variant_id: String,
    pub per_item: Vec<ItemRepeatStats>,
    /// 该档位跨 N 轮的完整结果明细（每轮一份 reply 全量），供逐轮语义评分聚合。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<ProbeVariantResult>,
}

/// 单题跨多次运行的指标统计（均值 ± 置信区间）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemRepeatStats {
    pub item_id: String,
    pub reply_chars: MetricStat,
    pub elapsed_ms: MetricStat,
}

/// 单一指标的统计摘要（均值 / 标准差 / 95% 置信区间）。
///
/// 说明:
/// - `mean`: 样本均值。
/// - `stddev`: 样本标准差（N=1 时为 0）。
/// - `ci_low` / `ci_high`: 95% 置信区间，按 t 分布（N≥2）或样本均值（N=1）计算。
/// - `n`: 样本量。
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct MetricStat {
    pub mean: f64,
    pub stddev: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub n: usize,
}

/// 单档位实验结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeVariantResult {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    pub runs: Vec<ProbeRunItem>,
    /// 该档位失败的题数（单题失败不中断批量，逐题记录原因）
    pub failed_count: usize,
}

/// 档位参数（结果中的可读形态，与 ProbeVariant 字段一致）。
///
/// M5a 消融扩展: `ablation` 记录该档位使用的消融 Profile（可选；None 表示无消融）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VariantParams {
    pub theta_gap_minutes: u32,
    pub max_msgs_per_block: u32,
    pub retrieve_top_k: u32,
    /// 消融档位 Profile 名称（可选；旧结果文件反序列化为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ablation: Option<String>,
}

/// 单题运行结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeRunItem {
    pub item_id: String,
    pub dimension: String,
    pub question: String,
    pub reply: String,
    pub metrics: ProbeMetrics,
    /// 失败原因（成功为 None）
    pub error: Option<String>,
}

/// 单题可测指标（探针阶段：长度与耗时；语义质量由 v1.6 T2 自动评分）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeMetrics {
    /// 回复字符数
    pub reply_chars: usize,
    /// LLM 调用耗时（毫秒）
    pub elapsed_ms: u128,
}

/// probe 子命令（由 main.rs 解析 clap 参数后构造）。
#[derive(Debug, Clone)]
pub enum ProbeCmd {
    /// 构建测试集（`dataset` 保留为 alias）
    Build {
        /// 目标 persona_uid（None = 自动选白名单内第一个角色类 persona）
        persona: Option<String>,
        /// 每维题数（默认 10）
        questions_per_dim: usize,
        /// 抽样 seed（默认固定，可复跑）
        seed: u64,
        /// 显式数据源文件（可选；不指定则从数据库构建）
        source: Option<PathBuf>,
        /// 数据集输出文件（`-` = stdout；不指定时仅 --json 输出完整数据集）
        output: Option<String>,
        json: bool,
    },
    /// 执行档位实验
    Run {
        /// 数据集文件（probe build 产物）
        dataset: PathBuf,
        /// 只跑指定档位（逗号分隔 id，默认全部）
        variants: Option<String>,
        /// 每档位最多跑题数（默认全部）
        limit: Option<usize>,
        /// 是否按档位参数重建 utt 块（默认 true；θ_gap/条数档位必须重建才生效；
        /// top_k 档位可用 --no-rebuild-utt 复用已建块）
        rebuild_utt: bool,
        /// 统计法重复次数 N（多次运行取均值 ± 置信区间；默认 1 即不聚合）
        repeat: Option<usize>,
        /// 结果输出文件（`-` = stdout 输出原始结果 JSON）
        output: Option<String>,
        json: bool,
    },
    /// 对 probe run 实验结果自动评分（事实维 golden + 语气维 LLM-as-judge）
    Evaluate {
        /// 实验结果文件（probe run 产物）
        results: PathBuf,
        /// 数据集文件（probe build 产物；提供时按 golden reference 精确评分）
        dataset: Option<PathBuf>,
        /// 只评指定档位（逗号分隔 id，默认全部）
        variants: Option<String>,
        /// 评分数值输出文件（`-` = stdout 输出评分 JSON）
        output: Option<String>,
        /// 跳过语气维 LLM-as-judge
        no_tone_judge: bool,
        json: bool,
    },
    /// 生成档位对比报告与定稿建议（markdown/JSON 双形态）
    Report {
        /// 实验结果文件（probe run 产物）
        results: PathBuf,
        /// 评分数值文件（probe evaluate 产物）
        evaluation: Option<PathBuf>,
        /// 人工抽检校准文件（JSON 数组 {item_id, score}）
        calibration: Option<PathBuf>,
        /// 报告输出文件（`-` = stdout；.md 为 markdown、.json 为 JSON）
        output: Option<String>,
        /// 消融对比报告模式（M5a T-004）
        ablation: bool,
        json: bool,
    },
}

/// probe 命令入口（probe 不需要交互确认，yes 参数供线上 provider 隐私确认透传）。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: ProbeCmd, yes: bool) -> anyhow::Result<()> {
    match cmd {
        ProbeCmd::Build {
            persona,
            questions_per_dim,
            seed,
            source,
            output,
            json,
        } => run_build(app, persona, questions_per_dim, seed, source, output, json).await,
        ProbeCmd::Run {
            dataset,
            variants,
            limit,
            rebuild_utt,
            repeat,
            output,
            json,
        } => {
            run_experiment(
                app,
                dataset,
                variants,
                limit,
                rebuild_utt,
                repeat,
                output,
                json,
                yes,
            )
            .await
        }
        ProbeCmd::Evaluate {
            results,
            dataset,
            variants,
            output,
            no_tone_judge,
            json,
        } => {
            run_evaluate(
                app,
                &results,
                dataset.as_deref(),
                variants.as_deref(),
                output.as_deref(),
                no_tone_judge,
                json,
            )
            .await
        }
        ProbeCmd::Report {
            results,
            evaluation,
            calibration,
            output,
            ablation,
            json,
        } => {
            run_report(
                app,
                &results,
                evaluation.as_deref(),
                calibration.as_deref(),
                output.as_deref(),
                ablation,
                json,
            )
            .await
        }
    }
}

// =========================================================
// 默认档位（代表配对，各参数 2 档）
// =========================================================

/// 默认档位组合。
///
/// 设计:
/// - baseline 即当前对照基准（θ_gap=10 / 条数=80 / top_k=3）。
/// - 其余档位每次只动一个参数，便于归因单参数对输出质量的影响。
/// - 档位参数与 `[utt]` 配置组字段一一对应，直接覆盖 `UttConfig` 生效。
fn default_variants() -> Vec<ProbeVariant> {
    vec![
        ProbeVariant {
            id: "baseline".to_string(),
            description: "对照基准（θ_gap=10/条数=80/top_k=3）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        ProbeVariant {
            id: "theta_gap_60".to_string(),
            description: "θ_gap 上调至 60 分钟（相对基准只动 θ_gap）".to_string(),
            theta_gap_minutes: 60,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        ProbeVariant {
            id: "max_msgs_40".to_string(),
            description: "条数上限下调至 40（相对基准只动条数）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 40,
            retrieve_top_k: 3,
            ablation: None,
        },
        ProbeVariant {
            id: "top_k_1".to_string(),
            description: "top_k 下调至 1（相对基准只动 top_k，更保守的原文注入）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 1,
            ablation: None,
        },
    ]
}

// =========================================================
// probe build：构建测试集
// =========================================================

/// 构建测试集（供命令与脚本复用；含 fixture 兜底降级）。
///
/// 数据来源优先级:
/// 1. `--source <file>`: 显式指定的数据源文件（JSON，含 messages/events）。
/// 2. 数据库: 从导入数据构建（tone 用 persona 发言配对、fact 用 L2 事件）。
/// 3. 内置夹具: 上述路径无真实数据或构建失败时兜底（静默降级 + warn）。
///
/// 参数:
/// - `source`: 显式数据源文件（None = 从数据库构建）。
///
/// 返回:
/// - 恒成功：文件/数据库路径失败时自动降级为内置夹具（静默降级 + warn）。
pub async fn build_dataset(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<&Path>,
) -> ProbeDataset {
    let qpd = questions_per_dim.max(1);
    let target = resolve_target_persona(app, persona.as_deref()).await;

    // 按数据来源构建：文件 > 数据库 > fixture 兜底
    match source {
        Some(path) => match build_from_file(path, &target, qpd, seed).await {
            Ok(ds) => ds,
            Err(e) => {
                // 文件读取/解析失败 → 夹具兜底（静默降级，记 warn）
                tracing::warn!(
                    path = %path.display(),
                    %e,
                    "probe build 数据文件处理失败，降级为内置夹具数据"
                );
                build_from_fixture(&target, qpd, seed)
            }
        },
        None => match build_from_db(app, &target, qpd, seed).await {
            Ok(ds) => ds,
            Err(e) => {
                tracing::warn!(%e, "probe build 数据库构建失败，降级为内置夹具数据");
                build_from_fixture(&target, qpd, seed)
            }
        },
    }
}

/// 执行 `probe build`（构建 + 输出）。
async fn run_build(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<PathBuf>,
    output: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let dataset = build_dataset(app, persona, questions_per_dim, seed, source.as_deref()).await;

    // 输出：--output 写数据集文件；--json 输出信封；文本模式打印摘要
    if let Some(out) = output.as_deref() {
        write_dataset_file(out, &dataset)?;
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": dataset.persona_uid,
                "source": dataset.source,
                "items": dataset.items.len(),
                "variants": dataset.variants.len(),
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "测试集已写入 {}（{} 题，{} 档位，source={}）",
            out,
            dataset.items.len(),
            dataset.variants.len(),
            dataset.source
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&dataset);
    }

    print_dataset_summary(&dataset);
    Ok(())
}

/// 从数据库构建测试集（tone 维度配对 persona 发言、fact 维度使用 L2 事件）。
async fn build_from_db(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let tone_pairs = collect_tone_pairs(app, persona_uid).await;
    let fact_items = collect_fact_events(app, persona_uid).await;
    let emotion_cands = collect_emotion_pairs(app, persona_uid).await;

    tracing::info!(
        %persona_uid,
        tone_candidates = tone_pairs.len(),
        fact_candidates = fact_items.len(),
        emotion_candidates = emotion_cands.len(),
        "probe build 从数据库收集候选"
    );

    // 确定性抽样 + 夹具补齐（每维恒有 qpd 题，档位实验规模稳定）
    let fixture_tone = fixture_tone_pairs();
    let fixture_fact = fixture_fact_events();
    let fixture_emotion = fixture_emotion_pairs();

    let (tone_items, tone_real) = sample_with_fallback(&tone_pairs, &fixture_tone, qpd, seed);
    let (fact_cands, fact_real) = sample_with_fallback(&fact_items, &fixture_fact, qpd, seed);
    let (emotion_cands, emotion_real) = sample_with_fallback(
        &emotion_cands,
        &fixture_emotion
            .into_iter()
            .map(|(q, r)| (q, r, None))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
    for (idx, (question, reference)) in tone_items.into_iter().enumerate() {
        let is_real = idx < tone_real;
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: None,
        });
    }
    for (idx, (question, reference, event_title)) in fact_cands.into_iter().enumerate() {
        let is_real = idx < fact_real;
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: Some(event_title),
        });
    }
    for (idx, (question, reference, src_ref)) in emotion_cands.into_iter().enumerate() {
        let is_real = idx < emotion_real;
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: src_ref,
        });
    }

    let any_real = tone_real > 0 || fact_real > 0 || emotion_real > 0;
    let source = if any_real { "db" } else { "fixture" };

    if !any_real {
        tracing::warn!(%persona_uid, "probe build 无真实数据，测试集全部使用内置夹具");
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona_uid.to_string(),
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
        questions_per_dimension: qpd,
        source: source.to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    })
}

/// 从显式数据源文件构建测试集。
///
/// 输入文件格式（JSON）:
/// ```json
/// {
///   "persona_uid": "char-0001",
///   "messages": [{"question": "...", "reply": "...", "source_ref": "..."}],
///   "events":   [{"title": "...", "summary": "..."}]
/// }
/// ```
async fn build_from_file(
    path: &Path,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取数据源文件失败: {}: {e}",
            path.display()
        )))
    })?;
    let raw: ProbeSourceFile = serde_json::from_str(&text).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "解析数据源文件失败: {}: {e}",
            path.display()
        )))
    })?;

    let persona = raw.persona_uid.unwrap_or_else(|| persona_uid.to_string());

    // tone（全部 messages）与 emotion（仅含情感线索的 messages）同源筛选；
    // messages 需要非空 question 才能配对。
    let tone_pairs: Vec<(String, String, Option<String>)> = raw
        .messages
        .iter()
        .filter(|m| !m.question.trim().is_empty())
        .map(|m| (m.question.clone(), m.reply.clone(), m.source_ref.clone()))
        .collect();
    let emotion_pairs: Vec<(String, String, Option<String>)> = tone_pairs
        .iter()
        .filter(|(q, _, _)| has_emotion_cue(q))
        .cloned()
        .collect();
    let fact_cands: Vec<(String, String, String)> = raw
        .events
        .iter()
        .filter(|e| !e.title.trim().is_empty())
        .map(|e| {
            (
                format!("还记得「{}」这件事吗？", e.title),
                e.summary.clone(),
                e.title.clone(),
            )
        })
        .collect();

    let (tone_items, tone_real) = sample_with_fallback(
        &tone_pairs,
        &fixture_tone_pairs()
            .into_iter()
            .map(|(q, r)| (q, r, None))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );
    let (fact_cands, fact_real) =
        sample_with_fallback(&fact_cands, &fixture_fact_events(), qpd, seed);
    let (emotion_cands, emotion_real) = sample_with_fallback(
        &emotion_pairs,
        &fixture_emotion_pairs()
            .into_iter()
            .map(|(q, r)| (q, r, None))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
    for (idx, (question, reference, src_ref)) in tone_items.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if idx < tone_real { "file" } else { "fixture" }.to_string(),
            source_ref: src_ref,
        });
    }
    for (idx, (question, reference, title)) in fact_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: if idx < fact_real { "file" } else { "fixture" }.to_string(),
            source_ref: Some(title),
        });
    }
    for (idx, (question, reference, src_ref)) in emotion_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: if idx < emotion_real {
                "file"
            } else {
                "fixture"
            }
            .to_string(),
            source_ref: src_ref,
        });
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona,
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
        questions_per_dimension: qpd,
        source: "file".to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    })
}

/// 全部使用内置夹具构建测试集（兜底路径）。
fn build_from_fixture(persona_uid: &str, qpd: usize, seed: u64) -> ProbeDataset {
    let (tone_items, _) = sample_with_fallback(&[], &fixture_tone_pairs(), qpd, seed);
    let (fact_cands, _) = sample_with_fallback(&[], &fixture_fact_events(), qpd, seed);
    let (emotion_cands, _) = sample_with_fallback(&[], &fixture_emotion_pairs(), qpd, seed);

    let mut items = Vec::with_capacity(qpd * 3);
    for (idx, (question, reference)) in tone_items.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: "fixture".to_string(),
            source_ref: None,
        });
    }
    for (idx, (question, reference, title)) in fact_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: "fixture".to_string(),
            source_ref: Some(title),
        });
    }
    for (idx, (question, reference)) in emotion_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: "fixture".to_string(),
            source_ref: None,
        });
    }

    ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona_uid.to_string(),
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
        questions_per_dimension: qpd,
        source: "fixture".to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    }
}

/// 解析目标 persona：
/// 1. 显式指定 → 用之；
/// 2. 未指定 → 数据库第一个白名单内角色类 persona（char/anim/oc/hist）；
/// 3. 无匹配 → 默认 char-0001（夹具数据以此编写）。
///
/// 语义（不按发言量选择）:
/// - 白名单 kind 过滤（Char/Anim/Oc/Hist）天然排除"我方"（kind=user），
///   探针目标始终为"对方" persona；
/// - 多个对方 persona 时取列表第一个（稳定可复跑），不引入发言量排序。
async fn resolve_target_persona(app: &Arc<ramaria_app::App>, explicit: Option<&str>) -> String {
    match app.storage().list_personas().await {
        Ok(personas) => select_target_persona(&personas, explicit),
        Err(e) => {
            tracing::warn!(%e, "读取 persona 列表失败，使用默认 persona");
            DEFAULT_PERSONA.to_string()
        }
    }
}

/// 从 persona 列表中选择探针目标（纯函数，便于确定性测试）。
///
/// 优先级:
/// 1. 显式 `explicit` → 直接使用（不校验 kind，尊重用户指定）。
/// 2. 白名单 kind（Char/Anim/Oc/Hist）内第一个 persona —— 对方语义；
///    我方（kind=User）与助手（kind=Rama）不入选。
/// 3. 无匹配 → `DEFAULT_PERSONA`（char-0001，夹具数据以此编写）。
fn select_target_persona(
    personas: &[ramaria_core::types::Persona],
    explicit: Option<&str>,
) -> String {
    if let Some(uid) = explicit {
        return uid.to_string();
    }
    let whitelisted = [
        PersonaKind::Char,
        PersonaKind::Anim,
        PersonaKind::Oc,
        PersonaKind::Hist,
    ];
    for p in personas {
        if whitelisted.contains(&p.kind) {
            tracing::info!(persona_uid = %p.uid, "probe build 自动选择白名单 persona");
            return p.uid.clone();
        }
    }
    tracing::info!(
        persona_uid = DEFAULT_PERSONA,
        "probe build 使用默认 persona"
    );
    DEFAULT_PERSONA.to_string()
}

/// 收集语气模仿维度候选：persona 发言与其同会话前一条 user 消息配对。
///
/// 返回 `(question, reference)` 列表（question = 用户消息，reference = persona 原回复）。
/// 查询失败按会话跳过（记 warn），不中断整体构建。
async fn collect_tone_pairs(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
) -> Vec<(String, String)> {
    let sessions = match app.storage().list_sessions().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%e, "probe build 读取会话列表失败，语气模仿维度无候选");
            return Vec::new();
        }
    };

    let mut pairs = Vec::new();
    for session in &sessions {
        let messages = match app.storage().list_messages(session.id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(session_id = %session.id, %e, "probe build 读取会话消息失败，跳过该会话");
                continue;
            }
        };
        let mut last_user: Option<String> = None;
        for m in &messages {
            match m.role {
                MessageRole::User => {
                    last_user = Some(m.content.clone());
                }
                _ => {
                    // 目标 persona 的发言：与其前一条 user 消息配对
                    if m.persona_uid.as_deref() == Some(persona_uid)
                        && let Some(q) = last_user.take()
                    {
                        pairs.push((q, m.content.clone()));
                    }
                }
            }
        }
    }
    pairs
}

/// 收集情感表达维度候选：用户消息含情感线索 → persona 原回复配对。
///
/// 返回 `(question, reference, source_ref)`：
/// - `question` = 情绪化用户消息（情感线索命中）；
/// - `reference` = persona 原回复（golden 参考，供人工/judge 校准）；
/// - `source_ref` = 溯源标识（当前 None，保留扩展位）。
///
/// 数据来源: 复用语气模仿的"user → persona 回复"配对机制（`collect_tone_pairs`），
/// 再按用户消息的情感关键词（难过/生气/担心/开心等）筛出情绪化情境——
/// 情感维度评估"persona 面对情绪化用户消息时的回应恰当性"（rubric 0/0.5/1），
/// 而非事实召回。
async fn collect_emotion_pairs(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
) -> Vec<(String, String, Option<String>)> {
    let pairs = collect_tone_pairs(app, persona_uid).await;
    pairs
        .into_iter()
        .filter(|(q, _)| has_emotion_cue(q))
        .map(|(q, r)| (q, r, None))
        .collect()
}

/// 文本是否含情感线索（负面/正面情绪触发词）。
///
/// 情感维度候选筛选用：仅当用户消息带有明显情绪色彩时才属于
/// "需要情感回应"的情境。中性消息（普通询问/陈述）不入选。
fn has_emotion_cue(text: &str) -> bool {
    has_negative_cue(text) || has_positive_cue(text)
}

/// 文本是否含负面情感触发词（难过/生气/担心等）。
fn has_negative_cue(text: &str) -> bool {
    EMOTION_NEGATIVE_CUES.iter().any(|w| text.contains(w))
}

/// 文本是否含正面情感触发词（开心/高兴/成功等）。
fn has_positive_cue(text: &str) -> bool {
    EMOTION_POSITIVE_CUES.iter().any(|w| text.contains(w))
}

/// 负面情绪触发词（情境侧：用户消息）。
const EMOTION_NEGATIVE_CUES: [&str; 24] = [
    "难过",
    "伤心",
    "哭",
    "郁闷",
    "烦躁",
    "生气",
    "愤怒",
    "气死",
    "气得",
    "担心",
    "焦虑",
    "紧张",
    "害怕",
    "怕",
    "委屈",
    "失望",
    "崩溃",
    "累",
    "烦",
    "不开心",
    "痛苦",
    "压力",
    "孤独",
    "自责",
];

/// 正面情绪触发词（情境侧：用户消息）。
const EMOTION_POSITIVE_CUES: [&str; 10] = [
    "开心",
    "高兴",
    "太好了",
    "兴奋",
    "中奖",
    "升职",
    "成功了",
    "通过",
    "好消息",
    "惊喜",
];

/// 情感维 rubric 的"安慰/共情"标记（回复侧：对负面情境的恰当回应）。
const EMOTION_COMFORT_MARKERS: [&str; 18] = [
    "别难过",
    "别伤心",
    "抱抱",
    "理解",
    "我懂",
    "会好的",
    "别担心",
    "放心",
    "支持",
    "安慰",
    "加油",
    "没事",
    "慢慢来",
    "辛苦了",
    "陪你",
    "心疼",
    "别着急",
    "别想太多",
];

/// 情感维 rubric 的"分享喜悦/肯定"标记（回复侧：对正面情境的恰当回应）。
const EMOTION_JOY_MARKERS: [&str; 14] = [
    "太好了",
    "真棒",
    "恭喜",
    "开心",
    "高兴",
    "为你高兴",
    "厉害",
    "棒",
    "赞",
    "不错",
    "真不错",
    "好耶",
    "值得",
    "分享",
];

/// 收集事实记忆维度候选：L2 事件 → 模板化问题。
///
/// 返回 `(question, reference, title)`（reference = 事件摘要，title 用于溯源）。
/// 查询失败记 warn 后返回空（由上层夹具兜底）。
async fn collect_fact_events(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
) -> Vec<(String, String, String)> {
    let events = match app
        .storage()
        .list_events_by_persona(persona_uid, 0, 10_000)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%persona_uid, %e, "probe build 读取事件失败，事实记忆维度无候选");
            return Vec::new();
        }
    };
    events
        .into_iter()
        .filter(|e| !e.title.trim().is_empty())
        .map(|e| {
            (
                format!("还记得「{}」这件事吗？", e.title),
                e.summary.clone(),
                e.title.clone(),
            )
        })
        .collect()
}

/// 确定性抽样 + 夹具补齐：从真实候选池抽 `count` 条，不足部分用夹具补满。
///
/// 返回 `(抽取结果, 真实条数, 夹具补齐条数)`。
/// 真实候选为空时直接取夹具前 `count` 条（保证确定性）。
fn sample_with_fallback<T: Clone>(
    candidates: &[T],
    fixture: &[T],
    count: usize,
    seed: u64,
) -> (Vec<T>, usize) {
    if candidates.is_empty() {
        let taken = fixture.iter().take(count).cloned().collect::<Vec<_>>();
        return (taken, 0);
    }
    let mut rng = DeterministicRng::new(seed);
    let mut pool = candidates.to_vec();
    rng.shuffle(&mut pool);
    let mut out: Vec<T> = pool.into_iter().take(count).collect();
    let real_n = out.len();
    for item in fixture {
        if out.len() >= count {
            break;
        }
        out.push(item.clone());
    }
    (out, real_n)
}

// =========================================================
// probe run：档位批量实验
// =========================================================

/// 执行 `probe run`。
///
/// 流程:
/// 1. 读取数据集（probe build 产物），校验 schema 版本与维度。
/// 2. 加载生效配置（config.toml + DB 双写合并）作为档位基准。
/// 3. 逐档位：覆盖 utt 三参数 →（可选）按档位参数重建 utt 块 → 逐题跑对话管线。
/// 4. 单题/单档位失败均不中断其余（记 warn + 记录失败原因）。
// 参数为命令入口的完整输入集合（含输出模式与隐私透传），合并会降低可读性；
// 与 app_chat.rs 的 `build_system_prompt_with_context` 采用同一 allow 约定。
#[allow(clippy::too_many_arguments)]
async fn run_experiment(
    app: &Arc<ramaria_app::App>,
    dataset_path: PathBuf,
    variants_filter: Option<String>,
    limit: Option<usize>,
    rebuild_utt: bool,
    repeat: Option<usize>,
    output: Option<String>,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    // Step 1: 读取并校验数据集（文件缺失/解析失败 → 业务校验失败，exit code 4）
    let text = std::fs::read_to_string(&dataset_path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取数据集失败: {}（请先运行 `ramaria probe build` 生成）: {e}",
            dataset_path.display()
        )))
    })?;
    let dataset: ProbeDataset = serde_json::from_str(&text)
        .map_err(|e| RamariaError::validation(format!("数据集解析失败: {e}")))?;
    if dataset.items.is_empty() {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "数据集不含任何测试问题"
        )));
    }

    // Step 2-5: 构建档位实验结果（隐私确认/配置基准/逐档位批量；单题失败不中断）
    let experiment = build_experiment_with_repeat(
        app,
        &dataset,
        &dataset_path,
        variants_filter.as_deref(),
        limit,
        rebuild_utt,
        repeat.unwrap_or(1),
        yes,
    )
    .await?;

    let run_count: usize = experiment.variants.iter().map(|v| v.runs.len()).sum();
    tracing::info!(run_count, "probe run 完成");

    // Step 6: 输出
    if let Some(out) = output.as_deref() {
        write_experiment_file(out, &experiment)?;
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": experiment.persona_uid,
                "variants": experiment.variants.len(),
                "runs": run_count,
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "实验结果已写入 {}（{} 档位，{} 次运行）",
            out,
            experiment.variants.len(),
            run_count
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&experiment);
    }

    print_experiment_summary(&experiment);
    Ok(())
}

/// 构建档位实验结果（供命令输出与 v1.6 T2 自动评分复用）。
///
/// 流程:
/// 1. 校验数据集（schema 版本不匹配记 warn 继续）。
/// 2. 加载生效配置（config.toml + DB 双写合并）作为档位基准。
/// 3. 隐私确认（线上 provider 需确认；本地 LM Studio 直接通过）。
/// 4. 逐档位：覆盖 utt 三参数 →（可选）按档位参数重建 utt 块 → 逐题跑对话管线。
/// 5. 单题/单档位失败均不中断其余（记 warn + 记录失败原因）。
pub async fn build_experiment(
    app: &Arc<ramaria_app::App>,
    dataset: &ProbeDataset,
    dataset_path: &Path,
    variants_filter: Option<&str>,
    limit: Option<usize>,
    rebuild_utt: bool,
    yes: bool,
) -> anyhow::Result<ProbeExperiment> {
    if dataset.schema_version != DATASET_SCHEMA_VERSION {
        tracing::warn!(
            schema = dataset.schema_version,
            expected = DATASET_SCHEMA_VERSION,
            "数据集 schema 版本不匹配，继续尝试执行"
        );
    }

    // Step 2: 加载生效配置作为档位基准（失败降级为 App 默认配置）
    let base_config = load_effective_config(app).await;

    // Step 3: 隐私确认（线上 provider 需确认；本地 LM Studio 直接通过）
    crate::privacy::ensure_privacy(app, yes).await?;

    // Step 4: 过滤档位（--variants；无效 id 记 warn 跳过）
    let variants = filter_variants(&dataset.variants, variants_filter);

    tracing::info!(
        dataset = %dataset_path.display(),
        persona_uid = %dataset.persona_uid,
        items = dataset.items.len(),
        variants = variants.len(),
        rebuild_utt,
        "probe run 开始"
    );

    // Step 5: 逐档位实验（档位对齐：切分参数去重，同切分多档位复用已建块）
    let mut results = Vec::with_capacity(variants.len());
    // 已按切分参数（θ_gap/条数）重建过的档位集合；top_k 不参与切分，复用已建块。
    let mut rebuilt_cuts: std::collections::HashMap<(u32, u32), ()> =
        std::collections::HashMap::new();
    for variant in &variants {
        // 覆盖 utt 三参数（档位基准 + 单参数变化）
        let mut variant_config = base_config.clone();
        variant_config.utt.theta_gap_minutes = variant.theta_gap_minutes;
        variant_config.utt.max_msgs_per_block = variant.max_msgs_per_block;
        variant_config.utt.retrieve_top_k = variant.retrieve_top_k;

        // M5a 消融扩展：档位带 `ablation` 时，在 utt 覆盖后应用
        // 注入层闸门（B0/B1/F0/F1~F4/S_*）。`ablation=None`（M1 旧数据集）
        // 时零覆盖——行为与 M1 完全一致（回归红线 6/兼容性要求）。
        if let Some(profile_name) = variant.ablation.as_deref() {
            match AblationProfile::parse_name(profile_name) {
                Some(profile) => {
                    profile.apply_to(&mut variant_config);
                    tracing::info!(
                        variant_id = %variant.id,
                        ablation = profile.name(),
                        "probe run 档位应用消融 Profile"
                    );
                }
                None => {
                    tracing::warn!(
                        variant_id = %variant.id,
                        ablation = profile_name,
                        "未知消融档位名称，忽略该档位的层闸门（按完整体系运行）"
                    );
                }
            }
        }

        tracing::info!(
            variant_id = %variant.id,
            theta_gap_minutes = variant.theta_gap_minutes,
            max_msgs_per_block = variant.max_msgs_per_block,
            retrieve_top_k = variant.retrieve_top_k,
            ablation = ?variant.ablation,
            "probe run 档位开始"
        );

        // 档位对齐：仅当收到重建指令且该切分参数（θ_gap/条数）尚未在本轮重建过时，
        // 才重建 utt 块——同一切分下的多档位（仅 top_k 不同）复用已建块，embedding
        // 调用数不随 top_k 档位倍增；top_k 变化不影响 utt 块切分。
        // `rebuild_utt=false`（--no-rebuild-utt）时完全不重建，直接复用库中已建块。
        let cut_key = (variant.theta_gap_minutes, variant.max_msgs_per_block);
        if rebuild_utt
            && !rebuilt_cuts.contains_key(&cut_key)
            && let Err(e) = rebuild_utt_for_config(app, &variant_config).await
        {
            tracing::warn!(
                variant_id = %variant.id,
                %e,
                "档位 utt 块重建失败，本次档位可能未按目标参数生效"
            );
        }
        if rebuild_utt {
            rebuilt_cuts.insert(cut_key, ());
        }

        let mut runs = Vec::new();
        let mut failed = 0usize;
        let max_runs = limit
            .unwrap_or(dataset.items.len())
            .min(dataset.items.len());
        for item in dataset.items.iter().take(max_runs) {
            let result =
                run_single_question(app, &variant_config, &dataset.persona_uid, item).await;
            if result.error.is_some() {
                failed += 1;
                tracing::warn!(
                    variant_id = %variant.id,
                    item_id = %result.item_id,
                    error = result.error.as_deref().unwrap_or(""),
                    "probe run 单题失败（不中断批量）"
                );
            }
            runs.push(result);
        }

        tracing::info!(
            variant_id = %variant.id,
            runs = runs.len(),
            failed,
            "probe run 档位完成"
        );

        results.push(ProbeVariantResult {
            variant_id: variant.id.clone(),
            description: variant.description.clone(),
            params: VariantParams {
                theta_gap_minutes: variant.theta_gap_minutes,
                max_msgs_per_block: variant.max_msgs_per_block,
                retrieve_top_k: variant.retrieve_top_k,
                ablation: variant.ablation.clone(),
            },
            runs,
            failed_count: failed,
        });
    }

    Ok(ProbeExperiment {
        dataset_file: dataset_path.display().to_string(),
        dataset_seed: dataset.seed,
        persona_uid: dataset.persona_uid.clone(),
        rebuild_utt,
        variants: results,
        repeat: None,
        generated_at: now_iso8601(),
    })
}

/// 统计法多次运行（`probe run --repeat N >= 2`）。
///
/// 流程:
/// 1. `repeat == 1`（或未指定）时等价于 `build_experiment` 单次运行。
/// 2. `repeat >= 2` 时连续执行 N 次完整档位实验（每次独立调用 LLM，以 DeepSeek
///    无 seed 的自然波动为统计样本），每次的档位/题项集合一致。
/// 3. 按「档位 × item_id」跨 N 次配对，对 `reply_chars` / `elapsed_ms` 计算
///    均值 / 标准差 / 95% 置信区间（t 分布），写入 `repeat` 聚合块；同时在该档位
///    `repeat.per_variant[].rounds` 保留**每一轮**的完整结果明细（逐轮全量 reply），
///    供 evaluate/report 对每轮 reply 分别语义评分后聚合 fact_score 均值 ± CI
///    （缺口 A，M5-005 配对统计口径）。主 `variants` 仍保留最后一次运行明细，
///    供单次评定/兼容读取复用。
///
/// 说明:
/// - 统计法为 M1/M5 共享工具链（D-V17-001）：档位对比以「多次均值 ± 置信区间」
///   为口径，不期待单次命令逐字复现。
#[allow(clippy::too_many_arguments)] // 参数与 build_experiment 一致（另加 repeat 聚合数）
pub async fn build_experiment_with_repeat(
    app: &Arc<ramaria_app::App>,
    dataset: &ProbeDataset,
    dataset_path: &Path,
    variants_filter: Option<&str>,
    limit: Option<usize>,
    rebuild_utt: bool,
    repeat: usize,
    yes: bool,
) -> anyhow::Result<ProbeExperiment> {
    if repeat <= 1 {
        return build_experiment(
            app,
            dataset,
            dataset_path,
            variants_filter,
            limit,
            rebuild_utt,
            yes,
        )
        .await;
    }

    let mut rounds = Vec::with_capacity(repeat);
    for i in 0..repeat {
        tracing::info!(round = i + 1, total = repeat, "probe run 统计法重复轮开始");
        rounds.push(
            build_experiment(
                app,
                dataset,
                dataset_path,
                variants_filter,
                limit,
                rebuild_utt,
                yes,
            )
            .await?,
        );
    }

    let last = rounds.last().expect("repeat >= 2 必有最后一轮").clone();
    let per_variant = aggregate_repeat_stats(&rounds);

    Ok(ProbeExperiment {
        dataset_file: last.dataset_file.clone(),
        dataset_seed: last.dataset_seed,
        persona_uid: last.persona_uid.clone(),
        rebuild_utt: last.rebuild_utt,
        variants: last.variants.clone(),
        repeat: Some(ProbeRepeatMeta {
            count: repeat,
            per_variant,
        }),
        generated_at: now_iso8601(),
    })
}

/// 跨 N 次运行聚合档位逐题统计（按 variant_id + item_id 配对）。
///
/// 配对规则:
/// - 档位按 `variant_id` 对齐（数据集同档位过滤，各轮档位集合一致）。
/// - 题项按 `item_id` 对齐（同为该档位的前 `limit`/全部题）。
/// - 若某轮缺失某 item 的指标（正常不应发生），仅以实际出现的样本聚合，`n`
///   反映真实样本量（`n >= 1`；`n == 1` 时置信区间退化为该样本均值，stddev=0）。
///
/// 缺口 A（M1 报告 §6 登记项）：`rounds` 保留该档位**每一轮**的完整 `ProbeVariantResult`
/// （含逐轮全量 reply），供 evaluate/report 对每轮 reply 分别语义评分后聚合 fact_score
/// 的均值 ± 置信区间（M5-005 配对统计口径）。
fn aggregate_repeat_stats(rounds: &[ProbeExperiment]) -> Vec<VariantRepeatStats> {
    let mut out = Vec::new();
    // 以最后一轮的档位顺序为准（各轮一致）
    let last = rounds.last().expect("至少一轮");
    for vr in &last.variants {
        let mut per_item = Vec::with_capacity(vr.runs.len());
        // 收集该档位在各轮中的完整结果（逐轮全量 reply，供逐轮评分聚合）。
        let mut round_results: Vec<ProbeVariantResult> = rounds
            .iter()
            .filter_map(|round| {
                round
                    .variants
                    .iter()
                    .find(|v| v.variant_id == vr.variant_id)
                    .cloned()
            })
            .collect();
        for run in &vr.runs {
            // 跨轮收集该 item 的指标
            let mut chars = Vec::new();
            let mut ms = Vec::new();
            for round in rounds {
                if let Some(vr2) = round
                    .variants
                    .iter()
                    .find(|v| v.variant_id == vr.variant_id)
                    && let Some(r) = vr2.runs.iter().find(|r| r.item_id == run.item_id)
                {
                    chars.push(r.metrics.reply_chars as f64);
                    ms.push(r.metrics.elapsed_ms as f64);
                }
            }
            per_item.push(ItemRepeatStats {
                item_id: run.item_id.clone(),
                reply_chars: metric_stat(&chars),
                elapsed_ms: metric_stat(&ms),
            });
        }
        // rounds 与 per_item 仅与"该档位在各轮中实际出现"对齐，序列化按此保留；
        // 若某轮重复运行了同档位多次，round_results 会多于 per_item 行——按档位保留逐轮，
        // 逐轮评分聚合（evaluate/report）以 round_results 内容为准。
        out.push(VariantRepeatStats {
            variant_id: vr.variant_id.clone(),
            per_item,
            rounds: std::mem::take(&mut round_results),
        });
    }
    out
}

/// 计算一组 f64 样本的均值 / 样本标准差 / 95% 置信区间。
///
/// 说明:
/// - 空样本 → 全零（`n=0`）。
/// - `n == 1` → mean 为该样本值、stddev=0、CI 退化为 [sample, sample]。
/// - `n >= 2` → 学生氏 t 分布 95% 置信区间 `mean ± t_{n-1,0.975} * (std/sqrt(n))`，
///   内置临界值（n 3~12 表查，超出用近似 `t ≈ 2.0`）。
fn metric_stat(samples: &[f64]) -> MetricStat {
    let n = samples.len();
    if n == 0 {
        return MetricStat {
            mean: 0.0,
            stddev: 0.0,
            ci_low: 0.0,
            ci_high: 0.0,
            n: 0,
        };
    }
    let mean = samples.iter().sum::<f64>() / n as f64;
    if n == 1 {
        return MetricStat {
            mean,
            stddev: 0.0,
            ci_low: mean,
            ci_high: mean,
            n,
        };
    }
    let variance = samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / (n - 1) as f64;
    let stddev = variance.sqrt();
    let t = t_critical_975(n);
    let half = t * stddev / (n as f64).sqrt();
    MetricStat {
        mean,
        stddev,
        ci_low: mean - half,
        ci_high: mean + half,
        n,
    }
}

/// 学生氏 t 分布 95% 双尾临界值（自由度 n-1）。
///
/// 覆盖 n=2..=12（探针 N=2~5 常用）；更大 n 用近似 2.0。
fn t_critical_975(n: usize) -> f64 {
    // 下标 = n-2（自由度 1..=10）
    const T: [f64; 11] = [
        12.706, // n=2, df=1
        4.303,  // n=3, df=2
        3.182,  // n=4, df=3
        2.776,  // n=5, df=4
        2.571,  // n=6, df=5
        2.447,  // n=7, df=6
        2.365,  // n=8, df=7
        2.306,  // n=9, df=8
        2.262,  // n=10, df=9
        2.228,  // n=11, df=10
        2.201,  // n=12, df=11
    ];
    if n >= 2 && n - 2 < T.len() {
        T[n - 2]
    } else {
        2.0
    }
}

/// 加载生效配置（config.toml + DB 双写合并，与 `blocks rebuild` 一致）。
/// 加载失败记 warn 并降级为 App 默认配置（不阻塞探针）。
async fn load_effective_config(app: &Arc<ramaria_app::App>) -> RamariaConfig {
    let config_path = PathBuf::from(&app.config().paths.config_dir).join("config.toml");
    let sync = ramaria_app::ConfigSyncService::new(app.storage().clone(), config_path);
    match sync.load_config_only().await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(%e, "读取生效配置失败，档位基准使用 App 默认配置");
            app.config().clone()
        }
    }
}

/// 按档位参数重建 utt 块（与 `blocks rebuild --force` 语义一致）。
///
/// 说明:
/// - 清空全部 utt 块后按目标参数全量重切（增量语义不会按新参数重切旧块）。
/// - embedding 不可用时块照常入库（仅无向量，检索退化为关键词通道）。
/// - 失败返回 Err，由调用方记 warn 后继续（档位实验不中断）。
async fn rebuild_utt_for_config(
    app: &Arc<ramaria_app::App>,
    config: &RamariaConfig,
) -> anyhow::Result<()> {
    let sessions = app.storage().list_sessions().await?;
    for session in &sessions {
        app.storage()
            .delete_utt_blocks_by_session(session.id)
            .await?;
    }
    let builder = UttBuilder::from_config(&config.utt);
    let embedding = app.embedding_provider();
    let embedder: Option<&dyn ramaria_core::EmbeddingProvider> =
        embedding.as_ref().map(|arc| arc.as_ref());
    builder
        .rebuild_all(app.storage().as_ref(), embedder)
        .await?;
    app.rebuild_retriever().await?;
    Ok(())
}

/// 跑单题对话并收集输出与指标。
///
/// 降级策略:
/// - `send_message` 本身失败（状态/隐私/存储）→ 记录 error，指标置零。
/// - 流内 Error 事件 → 记录 error，reply 保留已收到的部分。
async fn run_single_question(
    app: &Arc<ramaria_app::App>,
    config: &RamariaConfig,
    persona_uid: &str,
    item: &DatasetItem,
) -> ProbeRunItem {
    let start = Instant::now();
    let mut reply = String::new();
    let mut total_chars = 0usize;
    let mut error: Option<String> = None;

    let stream = match app
        .send_message_with_config(&item.question, Some(persona_uid), None, config)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return ProbeRunItem {
                item_id: item.id.clone(),
                dimension: item.dimension.clone(),
                question: item.question.clone(),
                reply: String::new(),
                metrics: ProbeMetrics {
                    reply_chars: 0,
                    elapsed_ms: start.elapsed().as_millis(),
                },
                error: Some(e.to_string()),
            };
        }
    };

    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                ramaria_app::stream_event::StreamEvent::Delta { content, .. } => {
                    reply.push_str(&content);
                }
                ramaria_app::stream_event::StreamEvent::Done {
                    total_chars: tc, ..
                } => {
                    total_chars = tc;
                }
                ramaria_app::stream_event::StreamEvent::Error { error: e, .. }
                    if error.is_none() =>
                {
                    error = Some(e);
                }
                _ => {
                    // StreamEvent 为 #[non_exhaustive]，忽略未知事件类型
                }
            },
            Err(e) => {
                if error.is_none() {
                    error = Some(e.to_string());
                }
            }
        }
    }

    // 隐私红线：日志不记录完整问题与回复，仅记长度
    tracing::debug!(
        item_id = %item.id,
        reply_chars = reply.chars().count(),
        total_chars,
        has_error = error.is_some(),
        "probe run 单题完成"
    );

    let reply_chars = reply.chars().count();
    let reply = if total_chars > 0 {
        reply.chars().take(total_chars).collect()
    } else {
        reply
    };

    ProbeRunItem {
        item_id: item.id.clone(),
        dimension: item.dimension.clone(),
        question: item.question.clone(),
        reply,
        metrics: ProbeMetrics {
            reply_chars,
            elapsed_ms: start.elapsed().as_millis(),
        },
        error,
    }
}

/// 按 --variants 过滤档位（逗号分隔；无效 id 记 warn 跳过）。
fn filter_variants(variants: &[ProbeVariant], filter: Option<&str>) -> Vec<ProbeVariant> {
    let Some(filter) = filter else {
        return variants.to_vec();
    };
    let ids: Vec<&str> = filter
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut out = Vec::new();
    for id in ids {
        match variants.iter().find(|v| v.id == id) {
            Some(v) => out.push(v.clone()),
            None => {
                tracing::warn!(variant_id = id, "probe run 忽略未知档位 id");
            }
        }
    }
    if out.is_empty() {
        tracing::warn!("probe run 档位过滤结果为空，回退为全部档位");
        variants.to_vec()
    } else {
        out
    }
}

// =========================================================
// probe evaluate：自动评分（事实维 golden + 语气维 LLM-as-judge）
// =========================================================

/// 探针评分结果（`probe evaluate` 的输出）。
///
/// 格式:
/// - `variants`: 各档位的评分汇总（事实维 / 语气维均分 + 逐题明细）。
/// - `judge_used`: 语气维 LLM-as-judge 是否可用（不可用则 tone 分缺失并标注）。
/// - `embedding_used`: 事实维是否使用了 embedding 余弦（不可用则退化为关键词）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeEvaluation {
    pub results_file: String,
    pub persona_uid: String,
    pub dataset_seed: u64,
    pub judge_used: bool,
    pub embedding_used: bool,
    pub generated_at: String,
    pub variants: Vec<VariantEvaluation>,
}

/// 单档位评分汇总。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VariantEvaluation {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    /// 事实维均分（0.0~1.0；无 fact 题或全失败为 None）
    pub fact_score: Option<f64>,
    /// 语气维均分（1.0~5.0；judge 不可用或全失败为 None）
    pub tone_score: Option<f64>,
    /// 情感表达维均分（0.0~1.0 rubric；无 emotion 题或全失败为 None）。
    /// `#[serde(default)]`：旧评分数值文件（M5a 前无此维度）反序列化回退 None。
    #[serde(default)]
    pub emotion_score: Option<f64>,
    /// 统计法（`--repeat N`）逐轮评分聚合（M5a T-003）。
    ///
    /// 格式:
    /// - 每个维度一条聚合记录；观测单位 = "轮"——每轮先对该轮全部题取维度均分，
    ///   再跨 N 轮聚合 mean / std / 95% CI（t 分布），`n` = 有效轮数。
    /// - 主 `fact_score` / `tone_score` / `emotion_score` 仍为最后一轮快照
    ///   （`experiment.variants`），不参与聚合。
    /// - 无 `--repeat` 或 run 文件无逐轮明细（旧产物）时为 None（省略，兼容旧文件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimension_scores: Option<Vec<DimensionScoreAgg>>,
    pub failed_count: usize,
    pub items: Vec<ItemEvaluation>,
}

/// 单维度的跨轮评分聚合（mean ± 95% CI）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DimensionScoreAgg {
    /// 维度名（fact / tone / emotion）
    pub dimension: String,
    /// 跨轮均值
    pub mean: f64,
    /// 跨轮样本标准差（n=1 时为 0）
    pub std: f64,
    /// 95% 置信区间下界（t 分布；n=1 时退化该轮值）
    pub ci95_low: f64,
    /// 95% 置信区间上界
    pub ci95_high: f64,
    /// 有效轮数
    pub n: usize,
}

impl DimensionScoreAgg {
    /// 从 MetricStat（mean/stddev/CI）转换为维度聚合记录。
    fn from_metric(dimension: &str, stat: &MetricStat) -> Self {
        Self {
            dimension: dimension.to_string(),
            mean: stat.mean,
            std: stat.stddev,
            ci95_low: stat.ci_low,
            ci95_high: stat.ci_high,
            n: stat.n,
        }
    }
}

/// 单题评分明细。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemEvaluation {
    pub item_id: String,
    pub dimension: String,
    pub question: String,
    /// 参考回答（golden 摘要 / persona 原回复）
    pub reference: Option<String>,
    /// 模型回复（长文本截断为摘要，避免评分文件过大）
    pub reply_preview: String,
    /// 事实维子评分（仅 fact 维度有值）
    pub fact: Option<FactItemScore>,
    /// 语气维子评分（仅 tone 维度且 judge 可用时有值）
    pub tone: Option<ToneItemScore>,
    /// 情感表达维子评分（仅 emotion 维度有值；旧文件缺省为 None）
    #[serde(default)]
    pub emotion: Option<EmotionItemScore>,
    /// 单题失败原因（成功为 None）
    pub error: Option<String>,
}

/// 事实维单题评分（embedding 余弦 + 关键词命中加权）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FactItemScore {
    /// 余弦相似度（-1.0~1.0；embedding 不可用为 None）
    pub cosine: Option<f64>,
    /// 关键词命中率（0.0~1.0，参考文本 token 在回复中出现的比例）
    pub keyword_hit: f64,
    /// 综合分（0.0~1.0）
    pub score: f64,
}

/// 语气维单题评分（LLM-as-judge）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToneItemScore {
    /// judge 评分（1~5 整数）
    pub score: u32,
    /// judge 简短理由（脱敏，不含原文）
    pub reason: Option<String>,
}

/// 情感表达维单题评分（rubric 0/0.5/1：情感回应恰当性，非事实召回）。
///
/// rubric 语义（确定性规则，可测试）:
/// - `1.0`: 回复恰当回应了用户情绪——负面情境含充分安慰/共情，
///   正面情境含分享喜悦/肯定。
/// - `0.5`: 部分回应（仅有 1 个恰当标记，或回应不充分但方向正确）。
/// - `0.0`: 未恰当回应（冷漠/答非所问/无任何情感标记）。
///
/// 字段约定:
/// - `situation_negative` / `situation_positive`: 从用户消息检测到的情境极性
///   （两者皆 false = 中性，按是否含一般共情标记打分）。
/// - `marker_hit`: 回复命中的恰当标记数（安慰标记或喜悦标记）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmotionItemScore {
    /// rubric 分（0.0 / 0.5 / 1.0）
    pub score: f64,
    /// 用户消息是否含负面情感线索
    pub situation_negative: bool,
    /// 用户消息是否含正面情感线索
    pub situation_positive: bool,
    /// 回复命中的恰当标记数（安慰/共情 或 分享喜悦）
    pub marker_hit: usize,
}

/// 语气维 judge 的 rubric 常量（1~5 档语义锚定）。
const TONE_RUBRIC: &str = "\
请按 1~5 分评估「候选回复」在语气、风格上与「参考回复」的相似程度：
1 分：语气/风格完全不像参考（生硬、机器人腔、明显偏离角色）；
2 分：略有相似但有明显偏差；
3 分：基本相似，偶有偏差；
4 分：语气/风格较贴近参考，偏差少；
5 分：语气/风格高度贴近参考，几乎难辨。
只输出一个整数分数（1~5），不要输出任何其他文字。";

/// 语气维 judge 的示例锚定（few-shot，帮助 judge 稳定判分）。
const TONE_ANCHOR_EXAMPLES: &str = "\
【示例 1】
参考回复：别太往心里去，领导批评方案不代表否定你这个人。把意见一条条记下来，改完这版肯定能行。
候选回复：不要太难过，领导不是否定你。把建议记录下来，改好就行。
分数：4
【示例 2】
参考回复：周末我一般不安排太满。你想去哪里？公园散步或者找家安静的咖啡馆都行。
候选回复：周末有空，你说去哪。
分数：2
【示例 3】
参考回复：先观察一下是不是吃太快或者毛球。如果持续吐或者精神不好，尽快带去看医生比较稳妥。
候选回复：赶紧去医院，别等了。
分数：1";

/// 事实维综合分权重（cosine 0.6 / keyword 0.4）。
const FACT_COSINE_WEIGHT: f64 = 0.6;
const FACT_KEYWORD_WEIGHT: f64 = 0.4;

/// 事实维 cosine 未用时的纯关键词权重（embedding 不可用降级）。
const FACT_KEYWORD_ONLY_WEIGHT: f64 = 1.0;

// =========================================================
// 执行 `probe evaluate`
// =========================================================

/// 执行 `probe evaluate`。
///
/// 流程:
/// 1. 读取并校验实验结果文件（probe run 产物；缺失/解析失败 → exit code 4）。
/// 2. 可选读取数据集文件（probe build 产物）：提供时按 golden reference 精确评分（事实维）。
/// 3. 过滤档位（--variants）。
/// 4. 逐档位逐题评分：
///    - 事实维: golden 评分（embedding 余弦 + 关键词命中加权；embedding 不可用退化为纯关键词）。
///    - 语气维: LLM-as-judge（rubric 1~5、温度 0、示例锚定）；LLM 不可用 → 跳过并标注。
/// 5. 单题失败不中断批量（记 warn + error 字段）。
async fn run_evaluate(
    app: &Arc<ramaria_app::App>,
    results_path: &Path,
    dataset_path: Option<&Path>,
    variants_filter: Option<&str>,
    output: Option<&str>,
    no_tone_judge: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Step 1: 读取实验结果
    let experiment = read_experiment(results_path)?;
    if experiment.variants.is_empty() {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "实验结果不含任何档位数据"
        )));
    }

    // Step 2: 可选读取数据集（golden reference 索引：item_id → reference）
    let golden = match dataset_path {
        Some(p) => match load_golden_references(p) {
            Ok(g) => {
                tracing::info!(
                    references = g.len(),
                    "已加载 golden reference（事实维精确评分）"
                );
                Some(g)
            }
            Err(e) => {
                tracing::warn!(%e, "golden reference 加载失败，事实维退化为回复启发式评分");
                None
            }
        },
        None => None,
    };

    // Step 3: 过滤档位（复用 filter_variants 的语义：无效 id 记 warn 跳过）
    let selected: Vec<ProbeVariantResult> = filter_variant_results(&experiment, variants_filter);

    // Step 4: 初始化评分器（embedding / judge）
    let embedder = app.embedding_provider();
    let embedding_used = embedder.as_ref().map(|e| e.is_available()).unwrap_or(false);
    if !embedding_used {
        tracing::warn!("embedding 不可用，事实维退化为纯关键词评分");
    }

    let judge: Option<Arc<dyn LlmProvider>> = if no_tone_judge {
        tracing::info!("--no-tone-judge：跳过语气维 LLM-as-judge");
        None
    } else {
        let llm = app.llm_clone();
        // LM Studio 本地后端可直接用作 judge；线上后端（DeepSeek/OpenAI）为隐私考虑不自动判分
        let provider_name = llm.config().provider.as_str();
        if provider_name == "lm-studio" {
            Some(llm)
        } else {
            tracing::warn!(
                provider = %provider_name,
                "语气维 judge 仅支持本地 LM Studio（线上后端自动跳过并标注）"
            );
            None
        }
    };
    let judge_used = judge.is_some();

    tracing::info!(
        results = %results_path.display(),
        embedding_used,
        judge_used,
        variants = selected.len(),
        "probe evaluate 开始"
    );

    // Step 4: 逐档位评分
    let mut eval_variants = Vec::with_capacity(selected.len());
    for vr in &selected {
        let mut items = Vec::with_capacity(vr.runs.len());
        let mut fact_scores: Vec<f64> = Vec::new();
        let mut tone_scores: Vec<u32> = Vec::new();
        let mut emotion_scores: Vec<f64> = Vec::new();
        let mut failed = 0usize;

        for run in &vr.runs {
            let item_eval = evaluate_item(run, &embedder, judge.as_deref(), golden.as_ref()).await;
            if item_eval.error.is_some() {
                failed += 1;
            }
            // 汇总维度均分（仅成功题计入）
            match &item_eval.fact {
                Some(f) if item_eval.error.is_none() => fact_scores.push(f.score),
                _ => {}
            }
            match &item_eval.tone {
                Some(t) if item_eval.error.is_none() => tone_scores.push(t.score),
                _ => {}
            }
            match &item_eval.emotion {
                Some(e) if item_eval.error.is_none() => emotion_scores.push(e.score),
                _ => {}
            }
            items.push(item_eval);
        }

        let fact_score = if fact_scores.is_empty() {
            None
        } else {
            Some(fact_scores.iter().sum::<f64>() / fact_scores.len() as f64)
        };
        let tone_score = if tone_scores.is_empty() {
            None
        } else {
            Some(tone_scores.iter().sum::<u32>() as f64 / tone_scores.len() as f64)
        };
        let emotion_score = if emotion_scores.is_empty() {
            None
        } else {
            Some(emotion_scores.iter().sum::<f64>() / emotion_scores.len() as f64)
        };

        tracing::info!(
            variant_id = %vr.variant_id,
            fact_score,
            tone_score,
            emotion_score,
            failed,
            items = items.len(),
            "probe evaluate 档位完成"
        );

        // ---- M5a T-003：统计法（--repeat N）逐轮评分聚合 ----
        // run 文件 `repeat.per_variant[].rounds` 保留每一轮的完整 reply；
        // 若存在，则对每轮分别评分后按"轮均分"跨 N 轮聚合 mean ± 95% CI。
        // 主 variants（最后一轮快照）不参与聚合（保持单次快照语义）。
        let dimension_scores = match &experiment.repeat {
            Some(rep) => {
                let agg = match rep
                    .per_variant
                    .iter()
                    .find(|r| r.variant_id == vr.variant_id)
                {
                    Some(rv) => {
                        aggregate_round_dimension_scores(
                            &rv.rounds,
                            &embedder,
                            judge.as_deref(),
                            golden.as_ref(),
                        )
                        .await
                    }
                    // 该档位无逐轮明细（旧产物）→ 无聚合
                    None => Vec::new(),
                };
                if agg.is_empty() { None } else { Some(agg) }
            }
            None => None,
        };

        eval_variants.push(VariantEvaluation {
            variant_id: vr.variant_id.clone(),
            description: vr.description.clone(),
            params: vr.params.clone(),
            fact_score,
            tone_score,
            emotion_score,
            dimension_scores,
            failed_count: failed,
            items,
        });
    }

    let evaluation = ProbeEvaluation {
        results_file: results_path.display().to_string(),
        persona_uid: experiment.persona_uid.clone(),
        dataset_seed: experiment.dataset_seed,
        judge_used,
        embedding_used,
        generated_at: now_iso8601(),
        variants: eval_variants,
    };

    // Step 5: 输出
    if let Some(out) = output {
        write_evaluation_file(out, &evaluation)?;
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": evaluation.persona_uid,
                "variants": evaluation.variants.len(),
                "judge_used": evaluation.judge_used,
                "embedding_used": evaluation.embedding_used,
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "评分数值已写入 {}（{} 档位，judge={}，embedding={}）",
            out,
            evaluation.variants.len(),
            evaluation.judge_used,
            evaluation.embedding_used
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&evaluation);
    }

    print_evaluation_summary(&evaluation);
    Ok(())
}

/// 读取实验结果文件（probe run 产物），缺失/解析失败 → 业务校验失败。
fn read_experiment(path: &Path) -> anyhow::Result<ProbeExperiment> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取实验结果失败: {}（请先运行 `ramaria probe run` 生成）: {e}",
            path.display()
        )))
    })?;
    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!(RamariaError::validation(format!("实验结果解析失败: {e}"))))
}

/// 按 --variants 过滤实验结果档位（无效 id 记 warn 跳过；空回退全部）。
fn filter_variant_results(
    experiment: &ProbeExperiment,
    filter: Option<&str>,
) -> Vec<ProbeVariantResult> {
    let Some(filter) = filter else {
        return experiment.variants.clone();
    };
    let ids: Vec<&str> = filter
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut out = Vec::new();
    for id in ids {
        match experiment.variants.iter().find(|v| v.variant_id == id) {
            Some(v) => out.push(v.clone()),
            None => {
                tracing::warn!(variant_id = id, "probe evaluate 忽略未知档位 id");
            }
        }
    }
    if out.is_empty() {
        tracing::warn!("probe evaluate 档位过滤结果为空，回退为全部档位");
        experiment.variants.clone()
    } else {
        out
    }
}

/// 从数据集文件加载 golden reference 索引（item_id → reference）。
///
/// 说明:
/// - 仅收集 fact 维度的 reference（事件摘要），作为事实维精确评分的 golden 参照。
/// - reference 缺失的条目忽略（后续退化为问题文本近似）。
fn load_golden_references(
    dataset_path: &Path,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let text = std::fs::read_to_string(dataset_path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取数据集失败: {}: {e}",
            dataset_path.display()
        )))
    })?;
    let dataset: ProbeDataset = serde_json::from_str(&text)
        .map_err(|e| RamariaError::validation(format!("数据集解析失败: {e}")))?;

    let mut map = std::collections::HashMap::new();
    for item in &dataset.items {
        if item.dimension == "fact"
            && let Some(rev) = item.reference.clone().filter(|r| !r.trim().is_empty())
        {
            map.insert(item.id.clone(), rev);
        }
    }
    Ok(map)
}

/// 对单题运行结果评分。
///
/// 维度分派:
/// - `fact` 题 → 事实维 golden 评分（cosine + keyword 加权）。
/// - `tone` 题 → 语气维 LLM-as-judge（judge 不可用则 tone=None，不报错）。
/// - `emotion` 题 → 情感表达维 rubric 评分（0/0.5/1 回应恰当性，确定性规则）。
async fn evaluate_item(
    run: &ProbeRunItem,
    embedder: &Option<Arc<dyn EmbeddingProvider>>,
    judge: Option<&dyn LlmProvider>,
    golden: Option<&std::collections::HashMap<String, String>>,
) -> ItemEvaluation {
    // 单题运行失败 → 不评分（error 透传）
    if let Some(e) = &run.error {
        return ItemEvaluation {
            item_id: run.item_id.clone(),
            dimension: run.dimension.clone(),
            question: run.question.clone(),
            reference: None,
            reply_preview: crate::util::truncate(&run.reply, 200),
            fact: None,
            tone: None,
            emotion: None,
            error: Some(e.clone()),
        };
    }

    let reply_preview = crate::util::truncate(&run.reply, 200);

    // 按维度评分
    match run.dimension.as_str() {
        "fact" => {
            // golden reference：优先用数据集 reference（精确）；缺失时用问题文本（近似）
            let reference = golden
                .and_then(|g| g.get(&run.item_id).cloned())
                .unwrap_or_else(|| run.question.clone());
            let fact = score_fact_item(&run.reply, &reference, embedder.as_deref()).await;
            ItemEvaluation {
                item_id: run.item_id.clone(),
                dimension: run.dimension.clone(),
                question: run.question.clone(),
                reference: Some(reference),
                reply_preview,
                fact: Some(fact),
                tone: None,
                emotion: None,
                error: None,
            }
        }
        "tone" => {
            let tone = match judge {
                Some(j) => match score_tone_item(j, run).await {
                    Ok(t) => Some(t),
                    Err(e) => {
                        // judge 单题失败不阻塞批量（记 warn，tone=None）
                        tracing::warn!(
                            item_id = %run.item_id,
                            error = %e,
                            "probe evaluate 语气维 judge 单题失败（tone 分缺失）"
                        );
                        None
                    }
                },
                None => None,
            };
            ItemEvaluation {
                item_id: run.item_id.clone(),
                dimension: run.dimension.clone(),
                question: run.question.clone(),
                reference: None,
                reply_preview,
                fact: None,
                tone,
                emotion: None,
                error: None,
            }
        }
        "emotion" => {
            // 情感表达维：rubric 0/0.5/1（情感回应恰当性）。
            // 以用户消息（run.question，即情绪化情境）判定情境极性，
            // 再统计回复命中的恰当标记数打分；不使用 golden 事实召回。
            let emotion = Some(score_emotion_item(&run.reply, &run.question));
            ItemEvaluation {
                item_id: run.item_id.clone(),
                dimension: run.dimension.clone(),
                question: run.question.clone(),
                reference: None,
                reply_preview,
                fact: None,
                tone: None,
                emotion,
                error: None,
            }
        }
        other => {
            // 未知维度：不评分，记录 error（不中断批量）
            ItemEvaluation {
                item_id: run.item_id.clone(),
                dimension: other.to_string(),
                question: run.question.clone(),
                reference: None,
                reply_preview,
                fact: None,
                tone: None,
                emotion: None,
                error: Some(format!("未知维度 {other}，未评分")),
            }
        }
    }
}

/// 统计法逐轮评分聚合（M5a T-003）。
///
/// 对 `--repeat N` 保留的每一轮完整结果（`rounds`）分别评分，
/// 按"轮"为观测单位聚合：
/// - 每轮先对该轮全部题取各维度均分（与主流程单次快照口径一致）；
/// - 跨 N 轮对轮均分计算 mean / std / 95% CI（t 分布，复用 `metric_stat`），
///   `n` = 有该维评分的有效轮数。
///
/// 返回按 fact → tone → emotion 排序的聚合记录；无任何有效轮时返回空。
/// 单题失败跳过（该轮其余成功题仍计入），与主流程"单题失败不中断"一致。
async fn aggregate_round_dimension_scores(
    rounds: &[ProbeVariantResult],
    embedder: &Option<Arc<dyn EmbeddingProvider>>,
    judge: Option<&dyn LlmProvider>,
    golden: Option<&std::collections::HashMap<String, String>>,
) -> Vec<DimensionScoreAgg> {
    let mut fact_round_means: Vec<f64> = Vec::new();
    let mut tone_round_means: Vec<f64> = Vec::new();
    let mut emotion_round_means: Vec<f64> = Vec::new();

    for round in rounds {
        let mut fact_scores: Vec<f64> = Vec::new();
        let mut tone_scores: Vec<u32> = Vec::new();
        let mut emotion_scores: Vec<f64> = Vec::new();
        for run in &round.runs {
            let item_eval = evaluate_item(run, embedder, judge, golden).await;
            if item_eval.error.is_some() {
                continue;
            }
            if let Some(f) = &item_eval.fact {
                fact_scores.push(f.score);
            }
            if let Some(t) = &item_eval.tone {
                tone_scores.push(t.score);
            }
            if let Some(e) = &item_eval.emotion {
                emotion_scores.push(e.score);
            }
        }
        if !fact_scores.is_empty() {
            fact_round_means.push(fact_scores.iter().sum::<f64>() / fact_scores.len() as f64);
        }
        if !tone_scores.is_empty() {
            tone_round_means
                .push(tone_scores.iter().sum::<u32>() as f64 / tone_scores.len() as f64);
        }
        if !emotion_scores.is_empty() {
            emotion_round_means
                .push(emotion_scores.iter().sum::<f64>() / emotion_scores.len() as f64);
        }
    }

    let mut out = Vec::with_capacity(3);
    if !fact_round_means.is_empty() {
        out.push(DimensionScoreAgg::from_metric(
            "fact",
            &metric_stat(&fact_round_means),
        ));
    }
    if !tone_round_means.is_empty() {
        out.push(DimensionScoreAgg::from_metric(
            "tone",
            &metric_stat(&tone_round_means),
        ));
    }
    if !emotion_round_means.is_empty() {
        out.push(DimensionScoreAgg::from_metric(
            "emotion",
            &metric_stat(&emotion_round_means),
        ));
    }
    if out.is_empty() {
        tracing::debug!(
            rounds = rounds.len(),
            "统计法逐轮评分聚合无有效轮（全部失败或无维度评分）"
        );
    }
    out
}

// =========================================================
// 事实维评分（golden：embedding 余弦 + 关键词命中）
// =========================================================

/// 事实维单题评分。
///
/// 评分公式:
/// - embedding 可用: `综合分 = 0.6 × cosine(reply, reference) + 0.4 × 关键词命中率`。
/// - embedding 不可用: `综合分 = 关键词命中率`（纯关键词降级，标注 embedding 未用）。
///
/// 说明:
/// - `reference` 为事实维 golden 参考（事件摘要）；`reply` 为模型对探针问题的回复。
/// - 关键词命中率衡量 reply 是否涵盖 reference 中的关键信息（按 2-gram 字面重叠）。
/// - cosine 为 reply 与 reference 的语义相似度（embedding 向量余弦）。
async fn score_fact_item(
    reply: &str,
    reference: &str,
    embedder: Option<&dyn EmbeddingProvider>,
) -> FactItemScore {
    // 关键词命中率：reference 的关键 2-gram 在 reply 中的覆盖比例
    let keyword_hit = keyword_hit_score(reply, reference);

    // embedding 余弦：reply vs reference
    let cosine = match embedder {
        Some(e) => match embed_pair(e, reply, reference).await {
            Some(c) => Some(c),
            None => {
                tracing::warn!("embedding 余弦计算失败，该题 cosine 缺失");
                None
            }
        },
        None => None,
    };

    // 综合分：cosine 不可用时纯关键词
    let score = match cosine {
        Some(c) => FACT_COSINE_WEIGHT * c.max(0.0) + FACT_KEYWORD_WEIGHT * keyword_hit,
        None => FACT_KEYWORD_ONLY_WEIGHT * keyword_hit,
    }
    .clamp(0.0, 1.0);

    FactItemScore {
        cosine,
        keyword_hit,
        score,
    }
}

/// 计算回复对参考文本的关键词命中率（0.0~1.0）。
///
/// 算法:
/// - 对 `reference` 提取中文 2-gram（bigram）集合，统计其中出现在 `reply` 中的比例。
/// - 2-gram 字面重叠在中文短文本上能稳定反映"回复是否覆盖参考关键信息"。
/// - `reference` 过短（<2 字）时退化为按 reply 信息密度打分。
fn keyword_hit_score(reply: &str, reference: &str) -> f64 {
    let reply = reply.trim();
    if reply.is_empty() {
        return 0.0;
    }
    let ref_chars: Vec<char> = reference.trim().chars().collect();
    if ref_chars.len() < 2 {
        // reference 过短，退化为 reply 信息密度
        return density_score(reply);
    }

    // reference 的 2-gram 集合
    let mut ref_bigrams: std::collections::HashSet<(char, char)> = std::collections::HashSet::new();
    for w in ref_chars.windows(2) {
        ref_bigrams.insert((w[0], w[1]));
    }
    if ref_bigrams.is_empty() {
        return 0.0;
    }

    // 统计出现在 reply 中的 reference bigram
    let reply_chars: Vec<char> = reply.chars().collect();
    let mut hit = 0usize;
    for w in reply_chars.windows(2) {
        if ref_bigrams.contains(&(w[0], w[1])) {
            hit += 1;
        }
    }

    // 命中率 = 命中 bigram 数 / reference bigram 总数（上限 1.0）
    (hit as f64 / ref_bigrams.len() as f64).clamp(0.0, 1.0)
}

/// 回复信息密度打分（无参考可用时的近似，0.0~1.0）。
///
/// 说明: 回复越长、信息越充分，密度分越高；空/过短回复得分低。
fn density_score(reply: &str) -> f64 {
    let chars = reply.chars().count();
    if chars < 8 {
        return 0.2;
    }
    if chars >= 40 {
        1.0
    } else {
        0.5 + 0.5 * (chars as f64 - 8.0) / 32.0
    }
}

/// 计算两段文本的 embedding 余弦相似度（失败返回 None，不阻塞）。
///
/// 说明: 任一文本向量化失败或向量为空 → None（调用方处理缺失）。
async fn embed_pair(embedder: &dyn EmbeddingProvider, a: &str, b: &str) -> Option<f64> {
    let va = match embedder.embed(a).await {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "embedding 向量化失败（文本 A）");
            return None;
        }
    };
    let vb = match embedder.embed(b).await {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "embedding 向量化失败（文本 B）");
            return None;
        }
    };
    Some(cosine_f32(&va, &vb))
}

/// 两个 f32 向量的余弦相似度（归一化内积）。
///
/// 说明: 任一向量的 L2 范数为 0（空/零向量）→ 返回 0.0（无语义可比）。
fn cosine_f32(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..a.len() {
        dot += a[i] as f64 * b[i] as f64;
        na += (a[i] as f64) * (a[i] as f64);
        nb += (b[i] as f64) * (b[i] as f64);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0)
}

// =========================================================
// 语气维评分（LLM-as-judge）
// =========================================================

/// 语气维 LLM-as-judge 单题评分（返回 judge 分 + 理由）。
///
/// 说明:
/// - 构造 judge prompt：rubric + 示例锚定 + 参考回复 + 候选回复。
/// - 温度 0（确定性评分）、max_tokens 小（只需整数）。
/// - 解析 LLM 输出的整数分数（1~5）；解析失败 → 报错（调用方记 warn）。
async fn score_tone_item(
    judge: &dyn LlmProvider,
    run: &ProbeRunItem,
) -> anyhow::Result<ToneItemScore> {
    let request = ChatRequest {
        system_prompt: format!("{TONE_RUBRIC}\n\n{TONE_ANCHOR_EXAMPLES}"),
        memory_context: None,
        history: vec![],
        user_message: format!(
            "参考回复：{}\n候选回复：{}",
            run.question, // question 为 tone 题的用户输入；reference 为 persona 原回复未随结果携带
            run.reply
        ),
        temperature: 0.0,
        max_tokens: 16,
        request_id: Uuid::new_v4(),
        template_version: "probe-judge-v1".to_string(),
    };

    let raw = judge.chat(&request).await.map_err(|e| {
        tracing::warn!(item_id = %run.item_id, error = %e, "语气维 judge LLM 调用失败");
        anyhow::anyhow!(e)
    })?;

    // 解析整数分数（从输出中提取 1~5 的数字）
    let score = parse_judge_score(&raw).ok_or_else(|| {
        anyhow::anyhow!(
            "judge 输出无法解析为 1~5 整数: {}",
            crate::util::truncate(&raw, 40)
        )
    })?;

    tracing::debug!(item_id = %run.item_id, score, "probe evaluate 语气维 judge 完成");
    Ok(ToneItemScore {
        score,
        reason: None, // 隐私约定：不记录 judge 理由原文
    })
}

/// 从 judge 输出解析 1~5 整数分（首个 1~5 数字；忽略其余文本）。
fn parse_judge_score(raw: &str) -> Option<u32> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    for ch in digits.chars() {
        let n = ch.to_digit(10)?;
        if (1..=5).contains(&n) {
            return Some(n);
        }
    }
    // 中文数字兜底（一~五）
    match raw.trim() {
        "一" => Some(1),
        "二" | "两" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        _ => None,
    }
}

// =========================================================
// 情感表达维评分（rubric 0/0.5/1：回应恰当性，非事实召回）
// =========================================================

/// 情感表达维单题评分。
///
/// 评分思路:
/// - 以用户消息（run.question，情绪化情境）判定情境极性——
///   负面（难过/生气/担心等）需要安慰/共情；正面（开心/成功等）需要分享喜悦/肯定。
/// - 统计回复命中的"恰当标记"数量，映射到 rubric 0 / 0.5 / 1：
///   - ≥ 2 个恰当标记 → 1.0（充分恰当回应）；
///   - 1 个 → 0.5（部分回应，方向正确但单薄）；
///   - 0 个 → 0.0（未恰当回应：冷漠/答非所问/无情感标记）。
/// - 中性情境（无正负触发词）按两类标记合计弱判定。
///
/// 设计约束:
/// - 确定性规则（零 LLM 依赖），可直接单测；不比对 golden 原文字面重叠
///   （那是事实召回口径），评估的是"情感回应恰当性"。
/// - 空/过短回复 → 0.0。
fn score_emotion_item(reply: &str, question: &str) -> EmotionItemScore {
    let reply = reply.trim();
    let situation_negative = has_negative_cue(question);
    let situation_positive = has_positive_cue(question);

    if reply.is_empty() {
        return EmotionItemScore {
            score: 0.0,
            situation_negative,
            situation_positive,
            marker_hit: 0,
        };
    }

    // 统计回复命中的恰当标记（负面情境用安慰/共情词表，正面用喜悦/肯定词表）。
    let marker_hit = if situation_negative {
        count_marker_hits(reply, &EMOTION_COMFORT_MARKERS)
    } else if situation_positive {
        count_marker_hits(reply, &EMOTION_JOY_MARKERS)
    } else {
        count_marker_hits(reply, &EMOTION_COMFORT_MARKERS)
            + count_marker_hits(reply, &EMOTION_JOY_MARKERS)
    };

    // rubric 映射：≥2 → 1.0；==1 → 0.5；0 → 0.0
    let score = match marker_hit {
        0 => 0.0,
        1 => 0.5,
        _ => 1.0,
    };

    EmotionItemScore {
        score,
        situation_negative,
        situation_positive,
        marker_hit,
    }
}

/// 统计文本命中词表条目的数量（去重：每个词条最多计 1 次命中）。
fn count_marker_hits(text: &str, markers: &[&str]) -> usize {
    markers.iter().filter(|w| text.contains(**w)).count()
}

// =========================================================
// 输出辅助（evaluate）
// =========================================================

/// 写评分数值到文件（`-` 表示 stdout）。
fn write_evaluation_file(out: &str, evaluation: &ProbeEvaluation) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(evaluation).context("评分数值序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入评分数值失败: {out}"))?;
    }
    Ok(())
}

/// 文本模式打印评分摘要。
fn print_evaluation_summary(evaluation: &ProbeEvaluation) {
    println!(
        "probe 评分: persona={} | {} 档位 | judge={} | embedding={} | 数据集 seed={}",
        evaluation.persona_uid,
        evaluation.variants.len(),
        evaluation.judge_used,
        evaluation.embedding_used,
        evaluation.dataset_seed
    );
    for v in &evaluation.variants {
        let fact = v
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = v
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = v
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  档位 {:<14} 事实维={:<6} 语气维={:<6} 情感维={:<6} 失败={} — {}",
            v.variant_id, fact, tone, emotion, v.failed_count, v.description
        );
    }
    if !evaluation.judge_used {
        crate::ui::info("语气维 judge 不可用或已跳过（tone 分为空），可运行 --json 查看标注");
    }
    crate::ui::info(
        "运行 `ramaria probe report --results <文件> --evaluation <评分文件>` 生成报告",
    );
}

// =========================================================
// probe report：档位对比报告 + 定稿建议 + 校准 + 知识层质量评估
// =========================================================

/// 档位对比报告（`probe report` 的输出，markdown/JSON 双形态）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeReport {
    pub results_file: String,
    pub evaluation_file: Option<String>,
    pub persona_uid: String,
    pub dataset_seed: u64,
    pub judge_used: bool,
    pub embedding_used: bool,
    pub generated_at: String,
    /// 各档位评分汇总表
    pub variants: Vec<VariantReportRow>,
    /// 定稿建议（每维度的推荐档位 + 理由）
    pub recommendation: Recommendation,
    /// 人工抽检校准结果（未提供校准文件时为 None）
    pub calibration: Option<CalibrationResult>,
    /// 知识层抽取质量评估（基于 fact 题误报/漏报；可选）
    pub knowledge_quality: Option<KnowledgeQualityReport>,
    /// 消融对比报告（`probe report --ablation`；普通模式为 None）
    pub ablation: Option<AblationReport>,
}

/// 档位报告行（评分对比表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantReportRow {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    pub fact_score: Option<f64>,
    pub tone_score: Option<f64>,
    /// 情感表达维均分（0.0~1.0；无 emotion 题时为 None）
    pub emotion_score: Option<f64>,
    pub success_count: usize,
    pub total_count: usize,
    pub failed_count: usize,
}

/// 定稿建议。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Recommendation {
    /// 每维度的最佳档位 id 与理由
    pub per_dimension: Vec<DimensionRecommendation>,
    /// 综合建议（兼顾各维的平衡档位）
    pub overall: String,
}

/// 单维度定稿建议。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DimensionRecommendation {
    pub dimension: String,
    pub best_variant: Option<String>,
    pub best_score: Option<f64>,
    pub reason: String,
}

/// 人工抽检校准结果。
///
/// 说明:
/// - `consistency`: judge 与人工分数的一致性（同分占比 / 平均绝对差）。
/// - `bias`: judge 相对人工的系统性偏差（judge 均分 − 人工均分；>0 偏高、<0 偏低）。
/// - `calibrated_coefficient`: 校准系数（人工均分 / judge 均分，用于把 judge 分缩放到人工尺度）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrationResult {
    pub sample_count: usize,
    pub total_count: usize,
    pub sample_rate: f64,
    pub consistency_exact: f64,
    pub mean_abs_diff: f64,
    pub bias: f64,
    pub calibrated_coefficient: Option<f64>,
    /// 是否不一致（一致性低或偏差大，报告标注）
    pub inconsistent: bool,
    /// 标注说明
    pub annotation: String,
}

/// 知识层抽取质量评估报告。
///
/// 说明:
/// - 基于事实维探针题评估知识层抽取质量：以「回复是否包含事件事实」判定命中/漏报。
/// - `false_positive_rate`（误报）：判定器注入但回复未涵盖事实（答非所问）。
/// - `false_negative_rate`（漏报）：回复未包含应有的事实信息（目标 <10%）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeQualityReport {
    pub sample_count: usize,
    pub fact_hit_count: usize,
    pub false_positive_rate: f64,
    pub false_negative_rate: f64,
    /// 是否达到漏报 <10% 目标
    pub miss_target_met: bool,
    pub annotation: String,
}

// =========================================================
// 消融对比报告（M5a T-004：配对 Wilcoxon + Cohen's d + CI + FDR）
// =========================================================

/// 消融对比报告（`probe report --ablation`）。
///
/// 结构:
/// - `baseline_variant`: 消融基线档位 id（F 组为 F0，S 组为 B1）。
/// - `rows`: 消融 vs 基线的逐"消融档位 × 维度"统计判定行。
/// - `aux`: 参与对比各档位的辅助指标（回复长度/耗时/空回复率）。
///
/// 判定线（D-V17-009）: `p_fdr < 0.05 ∧ |cohens_d| ≥ 0.3 ∧ CI 不含 0`
/// → 显著；F 组 diff<0（移除后下降）或 S 组 diff>0（加入后上升）→ 该层有贡献。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationReport {
    /// 消融基线档位 id（F0 或 B1）
    pub baseline_variant: String,
    /// 逐消融档位 × 维度统计判定
    pub rows: Vec<AblationComparisonRow>,
    /// 参与对比档位的辅助指标（mean ± CI / 空回复率）
    pub aux: Vec<VariantAuxMetrics>,
}

/// 单条消融对比（某消融档位 × 某维度，按题目配对）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationComparisonRow {
    /// 消融档位 id（如 F1 / S_behavior）
    pub ablation_variant: String,
    /// 消融档位描述
    pub description: String,
    /// 维度（fact / tone / emotion）
    pub dimension: String,
    /// 配对题数
    pub n_pairs: usize,
    /// 基线均值（F0 或 B1）
    pub base_mean: f64,
    /// 消融后均值
    pub ablated_mean: f64,
    /// 均值差（消融 − 基线）
    pub mean_diff: f64,
    /// 配对 Wilcoxon 符号秩检验 p 值（双尾，正态近似）
    pub wilcoxon_p: f64,
    /// FDR 校正后 p 值（Benjamini–Hochberg）
    pub p_fdr: f64,
    /// Cohen's d（配对 d_z = mean(diff)/sd(diff)；sd=0 时 ±10 标记远超阈值）
    pub cohens_d: f64,
    /// 均值差 95% 置信区间（t 分布）
    pub ci95_low: f64,
    /// 均值差 95% 置信区间上界
    pub ci95_high: f64,
    /// 是否显著（p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0）
    pub significant: bool,
    /// 该层贡献方向结论（up = 消融后提升 / down = 消融后下降 / none）
    pub direction: String,
    /// 人类可读结论
    pub annotation: String,
}

/// 档位辅助指标（消融报告交叉验证用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantAuxMetrics {
    pub variant_id: String,
    pub description: String,
    /// 平均回复字符数
    pub reply_chars_mean: f64,
    /// 平均耗时（毫秒）
    pub elapsed_ms_mean: f64,
    /// 空回复率（0.0~1.0）
    pub empty_reply_rate: f64,
    /// 成功题数 / 总题数
    pub success_count: usize,
    pub total_count: usize,
}

// =========================================================
// 执行 `probe report`
// =========================================================

/// 执行 `probe report`。
///
/// 流程:
/// 1. 读取实验结果（probe run 产物）。
/// 2. 读取评分数值（probe evaluate 产物；缺失则仅汇总 run 指标；
///    `--ablation` 模式必须提供评分数值，否则业务校验失败）。
/// 3. 生成档位对比表 + 定稿建议（每维最佳档位）。
/// 4. 若提供校准文件 → 计算 judge/人工一致性、偏差、校准系数。
/// 5. 若提供评分数值 → 基于 fact 题评估知识层误报/漏报。
/// 6. `--ablation` 模式 → 自动识别 F0/B1 基线生成消融对比统计。
/// 7. 输出 markdown / JSON 双形态。
async fn run_report(
    _app: &Arc<ramaria_app::App>,
    results_path: &Path,
    evaluation_path: Option<&Path>,
    calibration_path: Option<&Path>,
    output: Option<&str>,
    ablation: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Step 1: 读取实验结果
    let experiment = read_experiment(results_path)?;

    // --ablation 模式前置校验：需要评分数值文件（含逐题明细）。
    if ablation && evaluation_path.is_none() {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "消融对比报告（--ablation）需要评分数值文件：请先运行 `ramaria probe evaluate --results <run> --dataset <ds> --output <eval>`"
        )));
    }

    // Step 2: 读取评分数值（可选）
    let evaluation: Option<ProbeEvaluation> = match evaluation_path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| {
                anyhow::anyhow!(RamariaError::validation(format!(
                    "读取评分数值失败: {}（请先运行 `ramaria probe evaluate` 生成）: {e}",
                    p.display()
                )))
            })?;
            match serde_json::from_str(&text) {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!(error = %e, "评分数值解析失败，报告仅含运行指标");
                    None
                }
            }
        }
        None => None,
    };

    // Step 3: 档位对比表
    let mut rows = Vec::with_capacity(experiment.variants.len());
    for vr in &experiment.variants {
        let ev = evaluation
            .as_ref()
            .and_then(|e| e.variants.iter().find(|v| v.variant_id == vr.variant_id));
        let success = vr.runs.len().saturating_sub(vr.failed_count);
        rows.push(VariantReportRow {
            variant_id: vr.variant_id.clone(),
            description: vr.description.clone(),
            params: vr.params.clone(),
            fact_score: ev.and_then(|v| v.fact_score),
            tone_score: ev.and_then(|v| v.tone_score),
            emotion_score: ev.and_then(|v| v.emotion_score),
            success_count: success,
            total_count: vr.runs.len(),
            failed_count: vr.failed_count,
        });
    }

    // Step 4: 定稿建议（基于评分，无评分时基于运行指标）
    let recommendation = build_recommendation(&rows);

    // Step 5: 人工抽检校准（可选）
    let calibration = match calibration_path {
        Some(p) => {
            let manual = read_manual_scores(p)?;
            Some(compute_calibration(&manual, evaluation.as_ref()))
        }
        None => None,
    };

    // Step 6: 知识层质量评估（基于评分数值 fact 题）
    let knowledge_quality = evaluation.as_ref().map(assess_knowledge_quality);

    // Step 6.5: 消融对比报告（--ablation 模式）
    // 评分数值解析失败时评估为 None → 消融段缺省（记 warn 已在上游输出）。
    let ablation_report = if ablation {
        evaluation
            .as_ref()
            .map(|eval| build_ablation_report(&experiment, eval))
    } else {
        None
    };

    let report = ProbeReport {
        results_file: results_path.display().to_string(),
        evaluation_file: evaluation_path.map(|p| p.display().to_string()),
        persona_uid: experiment.persona_uid.clone(),
        dataset_seed: experiment.dataset_seed,
        judge_used: evaluation.as_ref().map(|e| e.judge_used).unwrap_or(false),
        embedding_used: evaluation
            .as_ref()
            .map(|e| e.embedding_used)
            .unwrap_or(false),
        generated_at: now_iso8601(),
        variants: rows,
        recommendation,
        calibration,
        knowledge_quality,
        ablation: ablation_report,
    };

    // Step 7: 输出
    if let Some(out) = output {
        // 按扩展名判断输出形态：.json → JSON；.md → markdown；其他按 --json 决定
        let is_json_file = out.ends_with(".json");
        if is_json_file || (json && !out.ends_with(".md")) {
            write_report_json(out, &report)?;
        } else {
            write_report_markdown(out, &report)?;
        }
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": report.persona_uid,
                "variants": report.variants.len(),
                "calibration": report.calibration.is_some(),
                "knowledge_quality": report.knowledge_quality.is_some(),
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "探针报告已写入 {}（{} 档位对比，{}）",
            out,
            report.variants.len(),
            if report.ablation.is_some() {
                "含消融对比统计"
            } else if report.calibration.is_some() {
                "含人工抽检校准"
            } else {
                "未校准"
            }
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&report);
    }

    print_report_summary(&report);
    Ok(())
}

/// 构建定稿建议（每维最佳档位 + 综合建议）。
fn build_recommendation(rows: &[VariantReportRow]) -> Recommendation {
    let mut per_dimension = Vec::new();

    // 事实维：取 fact_score 最高档位
    let fact_best = rows
        .iter()
        .filter(|r| r.fact_score.is_some())
        .max_by(|a, b| {
            a.fact_score
                .partial_cmp(&b.fact_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "fact".to_string(),
        best_variant: fact_best.map(|r| r.variant_id.clone()),
        best_score: fact_best.and_then(|r| r.fact_score),
        reason: match fact_best {
            Some(r) => format!(
                "事实维最高分 {:.2}（档位 {}）；综合 embedding 余弦与关键词命中",
                r.fact_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => {
                "无有效事实维评分（embedding 不可用或全部失败），无法给出事实维建议".to_string()
            }
        },
    });

    // 语气维：取 tone_score 最高档位
    let tone_best = rows
        .iter()
        .filter(|r| r.tone_score.is_some())
        .max_by(|a, b| {
            a.tone_score
                .partial_cmp(&b.tone_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "tone".to_string(),
        best_variant: tone_best.map(|r| r.variant_id.clone()),
        best_score: tone_best.and_then(|r| r.tone_score),
        reason: match tone_best {
            Some(r) => format!(
                "语气维最高分 {:.2}（档位 {}）；judge rubric 1~5 评分",
                r.tone_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => "语气维 judge 不可用或已跳过，无法给出语气维建议".to_string(),
        },
    });

    // 情感表达维：取 emotion_score 最高档位（rubric 0/0.5/1 回应恰当性）
    let emotion_best = rows
        .iter()
        .filter(|r| r.emotion_score.is_some())
        .max_by(|a, b| {
            a.emotion_score
                .partial_cmp(&b.emotion_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "emotion".to_string(),
        best_variant: emotion_best.map(|r| r.variant_id.clone()),
        best_score: emotion_best.and_then(|r| r.emotion_score),
        reason: match emotion_best {
            Some(r) => format!(
                "情感表达维最高分 {:.2}（档位 {}）；rubric 0/0.5/1 回应恰当性",
                r.emotion_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => "无情感表达维评分（无 emotion 题或全部失败），无法给出情感维建议".to_string(),
        },
    });

    // 综合建议：若事实/语气/情感最佳档位一致 → 取该档位；否则提示需人工权衡
    let all_same = |best: Option<&VariantReportRow>, id: &str| {
        best.map(|r| r.variant_id == id).unwrap_or(false)
    };
    let overall = match fact_best {
        Some(f) if all_same(tone_best, &f.variant_id) && all_same(emotion_best, &f.variant_id) => {
            format!(
                "综合建议档位 {}（事实/语气/情感均最优）；需人工抽检校准后定稿",
                f.variant_id
            )
        }
        _ => "各维最佳档位不一致，需结合人工抽检与消融实验（M5）权衡取舍".to_string(),
    };

    Recommendation {
        per_dimension,
        overall,
    }
}

// =========================================================
// 消融对比报告实现（M5a T-004）
// =========================================================

/// 供消融配对的逐维度"item_id → 分数"索引。
type VariantDimScores = std::collections::HashMap<String, f64>;

/// 从评分数值档位提取某维度的逐题分数（仅成功题）。
///
/// 说明: tone 维 judge 分 1~5 直接作连续分使用；fact/emotion 维 0~1。
fn collect_variant_dim_scores(ev: &VariantEvaluation, dim: &str) -> VariantDimScores {
    let mut map = VariantDimScores::new();
    for item in &ev.items {
        if item.error.is_some() {
            continue;
        }
        let score = match dim {
            "fact" => item.fact.as_ref().map(|s| s.score),
            "tone" => item.tone.as_ref().map(|s| s.score as f64),
            "emotion" => item.emotion.as_ref().map(|s| s.score),
            _ => None,
        };
        if let Some(s) = score {
            map.insert(item.item_id.clone(), s);
        }
    }
    map
}

/// 按题目配对两个档位在某维度的差分样本。
///
/// 配对规则: 仅取两端都成功评分的 item_id（同一题目），
/// `diffs = ablated − base`；两端任一缺失的题不参与配对。
fn pair_dimension_diffs(
    ablated: &VariantDimScores,
    base: &VariantDimScores,
) -> (Vec<f64>, f64, f64) {
    let mut diffs = Vec::new();
    let mut base_sum = 0.0;
    let mut ablated_sum = 0.0;
    for (item_id, base_score) in base {
        if let Some(ablated_score) = ablated.get(item_id) {
            diffs.push(ablated_score - base_score);
            base_sum += base_score;
            ablated_sum += ablated_score;
        }
    }
    let n = diffs.len() as f64;
    if n == 0.0 {
        return (diffs, 0.0, 0.0);
    }
    (diffs, base_sum / n, ablated_sum / n)
}

/// 构建消融对比报告。
///
/// 基线识别:
/// - F 组: F0（完整体系）为基线，F1~F4 为逐层消融；
/// - S 组: B1（压缩摘要基座）为基线，S_behavior/S_knowledge/S_expression/
///   S_narrative 为单层注入。
///
/// 统计（按题目配对）:
/// - 配对 Wilcoxon 符号秩检验（双尾，正态近似）；
/// - Cohen's d（配对 d_z）；
/// - 均值差 95% CI（t 分布，复用 `metric_stat`）；
/// - 全部行 p 值经 Benjamini–Hochberg FDR 校正。
///
/// 判定线（D-V17-009）: `p_fdr < 0.05 ∧ |d| ≥ 0.3 ∧ CI 不含 0` → 显著。
fn build_ablation_report(
    experiment: &ProbeExperiment,
    evaluation: &ProbeEvaluation,
) -> AblationReport {
    // 索引评分数值档位（id → evaluation）
    let by_id: std::collections::HashMap<&str, &VariantEvaluation> = evaluation
        .variants
        .iter()
        .map(|v| (v.variant_id.as_str(), v))
        .collect();

    // 基线识别（F0 / B1）
    let find_baseline = |names: &[&str]| -> Option<&VariantEvaluation> {
        names
            .iter()
            .find_map(|n| by_id.get(*n).copied())
            .or_else(|| {
                // 兼容：id 非 F0/B1 但 params.ablation 标注了基线名的档位
                evaluation.variants.iter().find(|v| {
                    v.params
                        .ablation
                        .as_deref()
                        .map(|a| names.contains(&a))
                        .unwrap_or(false)
                })
            })
    };
    let f0 = find_baseline(&["F0"]);
    let b1 = find_baseline(&["B1"]);

    // 待比较组：F 组（F1~F4 vs F0）与 S 组（S_* vs B1），按数据集实际出现的档位驱动。
    let dims = ["fact", "tone", "emotion"];

    // 先收集全部"候选行"（含未校正 p 值），再统一 FDR 校正后补判定字段。
    struct RawRow<'a> {
        ablation: &'a VariantEvaluation,
        dimension: &'a str,
        diffs: Vec<f64>,
        base_mean: f64,
        ablated_mean: f64,
        wilcoxon_p: f64,
        cohens_d: f64,
        ci_low: f64,
        ci_high: f64,
    }

    let mut raw_rows: Vec<RawRow> = Vec::new();
    let mut compared_ids: Vec<String> = Vec::new();

    // F 组：F1~F4 逐层关闭 vs F0
    if let Some(base) = f0 {
        for name in ["F1", "F2", "F3", "F4"] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean) = pair_dimension_diffs(
                        &collect_variant_dim_scores(ablated, dim),
                        &collect_variant_dim_scores(base, dim),
                    );
                    if diffs.len() < 2 {
                        tracing::debug!(
                            ablation = name,
                            dimension = dim,
                            pairs = diffs.len(),
                            "消融对比配对样本不足，跳过该行"
                        );
                        continue;
                    }
                    raw_rows.push(RawRow {
                        ablation: ablated,
                        dimension: dim,
                        base_mean,
                        ablated_mean,
                        wilcoxon_p: wilcoxon_signed_rank_p(&diffs).unwrap_or(1.0),
                        cohens_d: cohens_d_paired(&diffs),
                        ci_low: metric_stat(&diffs).ci_low,
                        ci_high: metric_stat(&diffs).ci_high,
                        diffs,
                    });
                }
            }
        }
    } else {
        tracing::warn!("消融对比报告：未找到 F0 基线档位，F 组（F1~F4）无法对比");
    }

    // S 组：单层注入 vs B1
    if let Some(base) = b1 {
        for name in ["S_behavior", "S_knowledge", "S_expression", "S_narrative"] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean) = pair_dimension_diffs(
                        &collect_variant_dim_scores(ablated, dim),
                        &collect_variant_dim_scores(base, dim),
                    );
                    if diffs.len() < 2 {
                        tracing::debug!(
                            ablation = name,
                            dimension = dim,
                            pairs = diffs.len(),
                            "消融对比配对样本不足，跳过该行"
                        );
                        continue;
                    }
                    raw_rows.push(RawRow {
                        ablation: ablated,
                        dimension: dim,
                        base_mean,
                        ablated_mean,
                        wilcoxon_p: wilcoxon_signed_rank_p(&diffs).unwrap_or(1.0),
                        cohens_d: cohens_d_paired(&diffs),
                        ci_low: metric_stat(&diffs).ci_low,
                        ci_high: metric_stat(&diffs).ci_high,
                        diffs,
                    });
                }
            }
        }
    } else {
        tracing::warn!("消融对比报告：未找到 B1 基线档位，S 组（单层注入）无法对比");
    }

    // 多比较 FDR 校正（Benjamini–Hochberg，作用于全部候选行）。
    let p_raw: Vec<f64> = raw_rows.iter().map(|r| r.wilcoxon_p).collect();
    let p_fdr = bh_fdr_adjust(&p_raw);

    let is_f_group = |ablation_name: &str| matches!(ablation_name, "F1" | "F2" | "F3" | "F4");

    let mut rows = Vec::with_capacity(raw_rows.len());
    for (raw, p_fdr) in raw_rows.into_iter().zip(p_fdr) {
        let ablation_name = raw.ablation.variant_id.as_str();
        // 判定线：p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0
        let ci_excludes_zero = raw.ci_low > 0.0 || raw.ci_high < 0.0;
        let significant = p_fdr < 0.05 && raw.cohens_d.abs() >= 0.3 && ci_excludes_zero;
        // 方向语义：F 组关注"移除后下降"；S 组关注"加入后上升"。
        let mean_diff = raw.ablated_mean - raw.base_mean;
        let (direction, annotation) = if significant {
            if mean_diff < 0.0 {
                (
                    "down".to_string(),
                    if is_f_group(ablation_name) {
                        format!(
                            "移除该层后质量显著下降（{:.3}），该层对「{}」有贡献",
                            mean_diff, raw.dimension
                        )
                    } else {
                        format!(
                            "加入该层后质量显著下降（{:.3}），该层单独注入为负向",
                            mean_diff
                        )
                    },
                )
            } else {
                (
                    "up".to_string(),
                    if is_f_group(ablation_name) {
                        format!(
                            "移除该层后质量反升（{:.3}），该层在本维度疑似冗余/负作用",
                            mean_diff
                        )
                    } else {
                        format!("加入该层后质量显著提升（{:.3}），该层有正向贡献", mean_diff)
                    },
                )
            }
        } else {
            (
                "none".to_string(),
                format!("无显著差异（p_fdr={:.3}, |d|={:.2}）", p_fdr, raw.cohens_d),
            )
        };

        rows.push(AblationComparisonRow {
            ablation_variant: ablation_name.to_string(),
            description: raw.ablation.description.clone(),
            dimension: raw.dimension.to_string(),
            n_pairs: raw.diffs.len(),
            base_mean: raw.base_mean,
            ablated_mean: raw.ablated_mean,
            mean_diff: raw.ablated_mean - raw.base_mean,
            wilcoxon_p: raw.wilcoxon_p,
            p_fdr,
            cohens_d: raw.cohens_d,
            ci95_low: raw.ci_low,
            ci95_high: raw.ci_high,
            significant,
            direction,
            annotation,
        });
    }

    // 辅助指标：覆盖所有参与对比档位 + 基线档位（从 run 实验明细取回复指标）。
    let mut compared: Vec<String> = compared_ids;
    if let Some(b) = f0 {
        compared.push(b.variant_id.clone());
    }
    if let Some(b) = b1 {
        compared.push(b.variant_id.clone());
    }
    let mut aux = Vec::new();
    for vr in &experiment.variants {
        if !compared.contains(&vr.variant_id) {
            continue;
        }
        aux.push(variant_aux_metrics(vr));
    }

    AblationReport {
        baseline_variant: f0
            .map(|v| v.variant_id.clone())
            .or_else(|| b1.map(|v| v.variant_id.clone()))
            .unwrap_or_default(),
        rows,
        aux,
    }
}

/// 计算单档位辅助指标（平均回复长度 / 平均耗时 / 空回复率）。
fn variant_aux_metrics(vr: &ProbeVariantResult) -> VariantAuxMetrics {
    let total = vr.runs.len();
    let success = total.saturating_sub(vr.failed_count);
    let mut chars = 0usize;
    let mut ms: u128 = 0;
    let mut empty = 0usize;
    for run in &vr.runs {
        chars += run.metrics.reply_chars;
        ms += run.metrics.elapsed_ms;
        if run.reply.trim().is_empty() {
            empty += 1;
        }
    }
    VariantAuxMetrics {
        variant_id: vr.variant_id.clone(),
        description: vr.description.clone(),
        reply_chars_mean: if total > 0 {
            chars as f64 / total as f64
        } else {
            0.0
        },
        elapsed_ms_mean: if total > 0 {
            ms as f64 / total as f64
        } else {
            0.0
        },
        empty_reply_rate: if total > 0 {
            empty as f64 / total as f64
        } else {
            0.0
        },
        success_count: success,
        total_count: total,
    }
}

// =========================================================
// 配对非参检验与效应量（纯函数，可单测）
// =========================================================

/// 配对 Wilcoxon 符号秩检验双尾 p 值（正态近似，无零差分）。
///
/// 算法:
/// - 剔除零差分后取绝对值排序，相同绝对值取平均秩；
/// - W+ = 正差分秩和；W 均值/方差（不含结校正的近似）→ z → 双尾 p。
/// - 样本量过小（n<5）时近似偏保守/不可靠，返回 `None`（调用方按 p=1.0 处理）。
fn wilcoxon_signed_rank_p(diffs: &[f64]) -> Option<f64> {
    // 剔除零差分
    let mut abs_pairs: Vec<(f64, bool)> = diffs
        .iter()
        .filter(|d| d.abs() > 1e-12)
        .map(|d| (d.abs(), *d > 0.0))
        .collect();
    let n = abs_pairs.len();
    if n < 5 {
        return None; // 样本过小，正态近似不可靠
    }
    abs_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // 平均秩（处理相同绝对值）
    let mut w_plus = 0.0f64;
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        while j + 1 < n && (abs_pairs[j + 1].0 - abs_pairs[i].0).abs() < 1e-12 {
            j += 1;
        }
        let rank_avg = (i + j + 2) as f64 / 2.0; // 1-based 位置平均
        for pair in &abs_pairs[i..=j] {
            if pair.1 {
                w_plus += rank_avg;
            }
        }
        i = j + 1;
    }

    // 无结近似：mean = n(n+1)/4，var = n(n+1)(2n+1)/24
    let n_f = n as f64;
    let mean = n_f * (n_f + 1.0) / 4.0;
    let variance = n_f * (n_f + 1.0) * (2.0 * n_f + 1.0) / 24.0;
    if variance <= 0.0 {
        return None;
    }
    let z = (w_plus - mean) / variance.sqrt();
    Some(2.0 * (1.0 - normal_cdf(z.abs())))
}

/// 标准正态分布 CDF（erf 近似）。
fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf_approx(z / std::f64::consts::SQRT_2))
}

/// erf 近似（Abramowitz–Stegun 7.1.26，最大误差 ~1.5e-7）。
fn erf_approx(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    if x > 6.0 {
        return sign;
    }
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * (-x * x).exp())
}

/// 配对 Cohen's d（d_z = mean(diff) / sd(diff)）。
///
/// 说明: 差分为零（sd≈0）且均值非零时以 ±10 标记"远超效应量阈值"
/// （避免 inf 破坏判定与序列化）；均值亦为零 → 0.0。
fn cohens_d_paired(diffs: &[f64]) -> f64 {
    let n = diffs.len();
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f64;
    let mean = diffs.iter().sum::<f64>() / n_f;
    if n == 1 {
        return if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        };
    }
    let variance = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n_f - 1.0);
    let sd = variance.sqrt();
    if sd < 1e-12 {
        if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        }
    } else {
        mean / sd
    }
}

/// Benjamini–Hochberg FDR 校正。
///
/// 返回与输入等长的校正后 q 值；空输入返回空。
fn bh_fdr_adjust(p_values: &[f64]) -> Vec<f64> {
    let m = p_values.len();
    if m == 0 {
        return Vec::new();
    }
    // 索引排序（小 → 大）
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|a, b| {
        p_values[*a]
            .partial_cmp(&p_values[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut q = vec![1.0f64; m];
    // 从最大 p 反向累计取最小
    let mut running_min = f64::INFINITY;
    for (rank_idx, &orig_idx) in order.iter().enumerate().rev() {
        let raw = p_values[orig_idx];
        let adjusted = (raw * m as f64 / (rank_idx + 1) as f64).min(1.0);
        running_min = running_min.min(adjusted);
        q[orig_idx] = running_min;
    }
    q
}

// =========================================================
// 人工抽检校准（T-V16-4-004）
// =========================================================

/// 读取人工抽检校准文件。
///
/// 格式（JSON）:
/// ```json
/// { "scores": [ {"item_id": "tone-0001", "score": 4}, ... ] }
/// ```
/// 或简单数组 `[{"item_id": "...", "score": 4}]`。
fn read_manual_scores(path: &Path) -> anyhow::Result<Vec<ManualScore>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取校准文件失败: {}: {e}",
            path.display()
        )))
    })?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| RamariaError::validation(format!("校准文件解析失败: {e}")))?;

    let scores = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(obj) = value.get("scores").and_then(|s| s.as_array()) {
        obj.clone()
    } else {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "校准文件格式无效（应为 JSON 数组或 {scores:[...]}）"
        )));
    };

    let mut out = Vec::with_capacity(scores.len());
    for s in scores {
        let item_id = s
            .get("item_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!(RamariaError::validation("校准条目缺少 item_id 字段")))?
            .to_string();
        let score = s
            .get("score")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!(RamariaError::validation("校准条目缺少 score 字段")))?;
        out.push(ManualScore { item_id, score });
    }
    Ok(out)
}

/// 人工抽检单条分数。
#[derive(Debug, Clone)]
pub struct ManualScore {
    pub item_id: String,
    pub score: u64,
}

/// 计算人工抽检校准结果（一致性 / 偏差 / 校准系数）。
///
/// 说明:
/// - 只统计 judge 有分的条目（tone 题 judge 分）。
/// - `consistency_exact`: judge 与人工同分占比。
/// - `mean_abs_diff`: 平均绝对差。
/// - `bias`: judge 均分 − 人工均分。
/// - `calibrated_coefficient`: 人工均分 / judge 均分（judge 均分为 0 时为 None）。
/// - `inconsistent`: 同分占比 < 0.5 或 |bias| > 1.0（判定校准不一致）。
fn compute_calibration(
    manual: &[ManualScore],
    evaluation: Option<&ProbeEvaluation>,
) -> CalibrationResult {
    // 收集 judge 分（从 evaluation 的 tone 题逐题明细）
    let mut judge_by_item: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    if let Some(eval) = evaluation {
        for v in &eval.variants {
            for item in &v.items {
                if let Some(tone) = &item.tone {
                    judge_by_item.insert(item.item_id.clone(), tone.score);
                }
            }
        }
    }

    // 配对：manual 中能在 judge 中找到分且维度匹配 tone 的条目
    let mut pairs: Vec<(u32, u64)> = Vec::new();
    for m in manual {
        if let Some(j) = judge_by_item.get(&m.item_id) {
            pairs.push((*j, m.score));
        }
    }

    let sample_count = pairs.len();
    if sample_count == 0 {
        return CalibrationResult {
            sample_count: 0,
            total_count: manual.len(),
            sample_rate: 0.0,
            consistency_exact: 0.0,
            mean_abs_diff: 0.0,
            bias: 0.0,
            calibrated_coefficient: None,
            inconsistent: true,
            annotation: "未找到任何与 judge 分匹配的人工抽检条目，无法校准".to_string(),
        };
    }

    let total_count = manual.len();
    let sample_rate = sample_count as f64 / total_count.max(1) as f64;

    let exact = pairs.iter().filter(|(j, m)| *j as u64 == *m).count();
    let consistency_exact = exact as f64 / sample_count as f64;

    let mean_abs_diff = pairs
        .iter()
        .map(|(j, m)| (*j as f64 - *m as f64).abs())
        .sum::<f64>()
        / sample_count as f64;

    let judge_mean = pairs.iter().map(|(j, _)| *j as f64).sum::<f64>() / sample_count as f64;
    let manual_mean = pairs.iter().map(|(_, m)| *m as f64).sum::<f64>() / sample_count as f64;
    let bias = judge_mean - manual_mean;

    let calibrated_coefficient = if judge_mean.abs() < 1e-9 {
        None
    } else {
        Some(manual_mean / judge_mean)
    };

    let inconsistent = consistency_exact < 0.5 || bias.abs() > 1.0;
    let annotation = if inconsistent {
        format!(
            "一致性偏低（同分 {:.0}% / 均差 {:.2} / 偏差 {:.2}），judge 需人工复核或调参",
            consistency_exact * 100.0,
            mean_abs_diff,
            bias
        )
    } else {
        format!(
            "一致性可接受（同分 {:.0}% / 均差 {:.2} / 偏差 {:.2}）",
            consistency_exact * 100.0,
            mean_abs_diff,
            bias
        )
    };

    CalibrationResult {
        sample_count,
        total_count,
        sample_rate,
        consistency_exact,
        mean_abs_diff,
        bias,
        calibrated_coefficient,
        inconsistent,
        annotation,
    }
}

// =========================================================
// 知识层抽取质量评估（T-V16-4-005）
// =========================================================

/// 评估知识层抽取质量（误报 / 漏报率）。
///
/// 说明:
/// - 基于评分数值中的 fact 题：以「回复是否充分回应事实性问题」判定命中。
/// - 判定规则（无 reference 时的近似）:
///   - `fact_hit`: 事实维 score ≥ 0.5（回复具体、信息充分）。
///   - `false_positive`（误报）: score 低但判定器/知识注入本应提供事实（此处以 score < 0.3 计）。
///   - `false_negative`（漏报）: score 居中但信息不足（score < 0.4 视为未充分回答事实）。
/// - 漏报率目标 < 10%（D-V16-004）。
fn assess_knowledge_quality(evaluation: &ProbeEvaluation) -> KnowledgeQualityReport {
    let mut fact_items: Vec<&ItemEvaluation> = Vec::new();
    for v in &evaluation.variants {
        for item in &v.items {
            if item.dimension == "fact" && item.fact.is_some() {
                fact_items.push(item);
            }
        }
    }

    let sample_count = fact_items.len();
    if sample_count == 0 {
        return KnowledgeQualityReport {
            sample_count: 0,
            fact_hit_count: 0,
            false_positive_rate: 0.0,
            false_negative_rate: 0.0,
            miss_target_met: false,
            annotation: "无事实维样本，无法评估知识层质量".to_string(),
        };
    }

    let fact_hit_count = fact_items
        .iter()
        .filter(|i| i.fact.as_ref().map(|f| f.score >= 0.5).unwrap_or(false))
        .count();

    // 误报：判定注入但回复未覆盖事实（score 低）——近似为 score < 0.3
    let false_positive = fact_items
        .iter()
        .filter(|i| i.fact.as_ref().map(|f| f.score < 0.3).unwrap_or(false))
        .count();

    // 漏报：回复未充分回答事实问题（score < 0.4 视为信息不足）
    let false_negative = fact_items
        .iter()
        .filter(|i| i.fact.as_ref().map(|f| f.score < 0.4).unwrap_or(false))
        .count();

    let false_positive_rate = false_positive as f64 / sample_count as f64;
    let false_negative_rate = false_negative as f64 / sample_count as f64;
    let miss_target_met = false_negative_rate < 0.10;

    let annotation = if miss_target_met {
        format!(
            "漏报率 {:.1}% 达标（<10%）；误报率 {:.1}%",
            false_negative_rate * 100.0,
            false_positive_rate * 100.0
        )
    } else {
        format!(
            "漏报率 {:.1}% 未达标（目标 <10%）；误报率 {:.1}%；需优化知识抽取或检索召回",
            false_negative_rate * 100.0,
            false_positive_rate * 100.0
        )
    };

    KnowledgeQualityReport {
        sample_count,
        fact_hit_count,
        false_positive_rate,
        false_negative_rate,
        miss_target_met,
        annotation,
    }
}

// =========================================================
// 报告输出辅助
// =========================================================

/// 写 JSON 报告到文件。
fn write_report_json(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(report).context("报告 JSON 序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n")).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 写 markdown 报告到文件。
fn write_report_markdown(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let md = render_report_markdown(report);
    if out == "-" {
        print!("{md}");
    } else {
        std::fs::write(out, md).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 渲染 markdown 报告（档位对比表 + 定稿建议 + 校准 + 知识层质量）。
fn render_report_markdown(report: &ProbeReport) -> String {
    let mut md = String::new();
    md.push_str("# Ramaria 探针档位对比报告\n\n");
    md.push_str(&format!("- persona: `{}`\n", report.persona_uid));
    md.push_str(&format!("- 数据集 seed: {}\n", report.dataset_seed));
    md.push_str(&format!(
        "- 语气 judge: {} / 事实 embedding: {}\n",
        report.judge_used, report.embedding_used
    ));
    md.push_str(&format!("- 生成时间: {}\n\n", report.generated_at));

    // 档位对比表
    md.push_str("## 档位评分对比\n\n");
    md.push_str("| 档位 | 事实维 | 语气维 | 情感维 | 成功/总 | 失败 | 说明 |\n");
    md.push_str("|------|:---:|:---:|:---:|:---:|:---:|------|\n");
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}/{} | {} | {} |\n",
            r.variant_id,
            fact,
            tone,
            emotion,
            r.success_count,
            r.total_count,
            r.failed_count,
            r.description.replace('|', "\\|")
        ));
    }
    md.push('\n');

    // 定稿建议
    md.push_str("## 定稿建议\n\n");
    for d in &report.recommendation.per_dimension {
        md.push_str(&format!(
            "**{}**：{}（最佳档位 {}）\n\n",
            d.dimension,
            d.reason,
            d.best_variant.as_deref().unwrap_or("—")
        ));
    }
    md.push_str(&format!("**综合**：{}\n\n", report.recommendation.overall));

    // 人工抽检校准
    if let Some(c) = &report.calibration {
        md.push_str("## 人工抽检校准\n\n");
        md.push_str(&format!(
            "- 抽检样本：{}/{}（{:.0}%）\n",
            c.sample_count,
            c.total_count,
            c.sample_rate * 100.0
        ));
        md.push_str(&format!(
            "- 同分一致性：{:.0}%\n",
            c.consistency_exact * 100.0
        ));
        md.push_str(&format!("- 平均绝对差：{:.2}\n", c.mean_abs_diff));
        md.push_str(&format!("- 偏差（judge−人工）：{:.2}\n", c.bias));
        if let Some(coef) = c.calibrated_coefficient {
            md.push_str(&format!("- 校准系数：{:.3}\n", coef));
        }
        md.push_str(&format!("- 标注：{}\n\n", c.annotation));
    }

    // 知识层质量
    if let Some(kq) = &report.knowledge_quality {
        md.push_str("## 知识层抽取质量评估\n\n");
        md.push_str(&format!("- 样本数：{}（事实维）\n", kq.sample_count));
        md.push_str(&format!("- 事实命中：{}\n", kq.fact_hit_count));
        md.push_str(&format!(
            "- 误报率：{:.1}%\n",
            kq.false_positive_rate * 100.0
        ));
        md.push_str(&format!(
            "- 漏报率：{:.1}%（目标 <10% → {})\n",
            kq.false_negative_rate * 100.0,
            if kq.miss_target_met {
                "达标"
            } else {
                "未达标"
            }
        ));
        md.push_str(&format!("- 结论：{}\n\n", kq.annotation));
    }

    // 消融对比统计
    if let Some(ab) = &report.ablation {
        md.push_str("## 消融对比统计\n\n");
        md.push_str(&format!("- 基线档位：`{}`\n", ab.baseline_variant));
        md.push_str(
            "- 方法：按题目配对 Wilcoxon 符号秩检验 + Cohen's d + 95% CI；\
             多比较经 Benjamini–Hochberg FDR 校正\n",
        );
        md.push_str("- 判定线：p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0\n\n");
        md.push_str("| 消融档位 | 维度 | 基线 | 消融后 | Δ | p_fdr | d | 95%CI | 判定 |\n");
        md.push_str("|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|------|\n");
        for r in &ab.rows {
            md.push_str(&format!(
                "| {} | {} | {:.3} | {:.3} | {:.3} | {:.4} | {:.2} | [{:.3}, {:.3}] | {}(n={}) |\n",
                r.ablation_variant,
                r.dimension,
                r.base_mean,
                r.ablated_mean,
                r.mean_diff,
                r.p_fdr,
                r.cohens_d,
                r.ci95_low,
                r.ci95_high,
                match (r.significant, r.direction.as_str()) {
                    (true, "down") => "↓ 移除后下降（层有贡献）",
                    (true, "up") => "↑ 差异显著",
                    _ => "→ 无差异",
                },
                r.n_pairs
            ));
        }
        md.push('\n');
        md.push_str("### 辅助指标\n\n");
        md.push_str("| 档位 | 平均回复(字符) | 平均耗时(ms) | 空回复率 | 成功/总 |\n");
        md.push_str("|------|:---:|:---:|:---:|:---:|\n");
        for a in &ab.aux {
            md.push_str(&format!(
                "| {} | {:.1} | {:.1} | {:.1}% | {}/{} |\n",
                a.variant_id,
                a.reply_chars_mean,
                a.elapsed_ms_mean,
                a.empty_reply_rate * 100.0,
                a.success_count,
                a.total_count
            ));
        }
        md.push('\n');
    }

    md.push_str("---\n*由 `ramaria probe report` 自动生成，供 M5 定稿实验参考。*\n");
    md
}

/// 文本模式打印报告摘要（stdout 只输出数据）。
fn print_report_summary(report: &ProbeReport) {
    println!(
        "探针报告: persona={} | {} 档位对比 | judge={} | embedding={}",
        report.persona_uid,
        report.variants.len(),
        report.judge_used,
        report.embedding_used
    );
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  档位 {:<14} 事实={:<6} 语气={:<6} 情感={:<6} 成功={}/{} — {}",
            r.variant_id, fact, tone, emotion, r.success_count, r.total_count, r.description
        );
    }
    println!("定稿建议: {}", report.recommendation.overall);
    if let Some(c) = &report.calibration {
        println!(
            "校准: 样本 {}/{} 同分 {:.0}% 偏差 {:.2} {}",
            c.sample_count,
            c.total_count,
            c.consistency_exact * 100.0,
            c.bias,
            if c.inconsistent {
                "⚠ 不一致"
            } else {
                "✓ 一致"
            }
        );
    }
    if let Some(kq) = &report.knowledge_quality {
        println!(
            "知识层: 误报 {:.1}% 漏报 {:.1}% {}",
            kq.false_positive_rate * 100.0,
            kq.false_negative_rate * 100.0,
            if kq.miss_target_met {
                "（达标）"
            } else {
                "（未达标）"
            }
        );
    }
    if let Some(ab) = &report.ablation {
        let sig = ab.rows.iter().filter(|r| r.significant).count();
        println!(
            "消融对比: {} 行对比（基线 {}），显著 {} 行",
            ab.rows.len(),
            ab.baseline_variant,
            sig
        );
    }
    crate::ui::info("用 --output 生成 markdown/JSON 报告文件");
}

// =========================================================
// 输出辅助
// =========================================================

/// 写数据集到文件（`-` 表示 stdout，输出原始数据集 JSON）。
fn write_dataset_file(out: &str, dataset: &ProbeDataset) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(dataset).context("数据集序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入数据集失败: {out}"))?;
    }
    Ok(())
}

/// 写实验结果到文件（`-` 表示 stdout，输出原始结果 JSON）。
fn write_experiment_file(out: &str, experiment: &ProbeExperiment) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(experiment).context("实验结果序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入实验结果失败: {out}"))?;
    }
    Ok(())
}

/// 文本模式打印数据集摘要（stdout 仅输出数据，提示走 stderr）。
fn print_dataset_summary(dataset: &ProbeDataset) {
    let dim_count = |dim: &str| dataset.items.iter().filter(|i| i.dimension == dim).count();
    let tone = dim_count("tone");
    let fact = dim_count("fact");
    let emotion = dim_count("emotion");
    let real = dataset
        .items
        .iter()
        .filter(|i| i.source != "fixture")
        .count();
    println!(
        "probe 测试集: persona={} | 维度=tone({})/fact({})/emotion({}) | 档位={} | 真实数据 {} 题 / 夹具 {} 题 | source={}",
        dataset.persona_uid,
        tone,
        fact,
        emotion,
        dataset.variants.len(),
        real,
        dataset.items.len() - real,
        dataset.source
    );
    println!("seed={}（相同 seed 可复跑相同测试集）", dataset.seed);
    for v in &dataset.variants {
        let ablation = v
            .ablation
            .as_deref()
            .map(|a| format!(" [消融:{a}]"))
            .unwrap_or_default();
        println!(
            "  档位 {:<14} θ_gap={:<3} 条数={:<3} top_k={}{}  — {}",
            v.id,
            v.theta_gap_minutes,
            v.max_msgs_per_block,
            v.retrieve_top_k,
            ablation,
            v.description
        );
    }
    crate::ui::info("运行 `ramaria probe run --dataset <文件>` 执行档位实验");
}

/// 文本模式打印实验结果摘要。
fn print_experiment_summary(experiment: &ProbeExperiment) {
    println!(
        "probe 实验结果: persona={} | {} 档位 | 数据集 seed={}",
        experiment.persona_uid,
        experiment.variants.len(),
        experiment.dataset_seed
    );
    for v in &experiment.variants {
        let success = v.runs.len() - v.failed_count;
        let avg_ms: u128 = if v.runs.is_empty() {
            0
        } else {
            v.runs.iter().map(|r| r.metrics.elapsed_ms).sum::<u128>() / v.runs.len() as u128
        };
        let avg_chars: usize = if v.runs.is_empty() {
            0
        } else {
            v.runs.iter().map(|r| r.metrics.reply_chars).sum::<usize>() / v.runs.len()
        };
        println!(
            "  档位 {:<14} 成功={}/{:<3} 平均回复 {} 字符 / {} ms  — {}",
            v.variant_id,
            success,
            v.runs.len(),
            avg_chars,
            avg_ms,
            v.description
        );
    }
    // 统计法（--repeat N）：展示各档位跨 N 次的均值（细目与置信区间见 --json/文件）
    if let Some(rep) = &experiment.repeat {
        println!(
            "  统计法: {} 次运行，各档位平均（细目/95% 置信区间见 --json 或 --output）",
            rep.count
        );
        for vs in &rep.per_variant {
            let n = vs.per_item.len();
            if n == 0 {
                continue;
            }
            let chars_mean: f64 =
                vs.per_item.iter().map(|s| s.reply_chars.mean).sum::<f64>() / n as f64;
            let ms_mean: f64 =
                vs.per_item.iter().map(|s| s.elapsed_ms.mean).sum::<f64>() / n as f64;
            println!(
                "    档位 {:<14} 平均回复 {:.1} 字符 / {:.1} ms（N={}）",
                vs.variant_id, chars_mean, ms_mean, rep.count
            );
        }
    }
    crate::ui::info("完整结果含每题 reply/metrics，可用 --json 或 --output 获取");
}

/// 当前时间（ISO-8601 UTC，M1 约定）。
fn now_iso8601() -> String {
    crate::util::format_timestamp_iso(now_ms())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

// =========================================================
// 确定性伪随机（xorshift64*，无外部依赖，seed 固定可复跑）
// =========================================================

/// 确定性伪随机数生成器。
///
/// 职责:
/// - 为测试集抽样提供可复现的随机序列（同 seed → 同测试集）。
/// - 使用 xorshift64*（无外部 crate 依赖，实现短、可单测）。
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    /// 创建生成器（seed=0 时使用固定非零种子，避免全零状态退化）。
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// 下一个 u64 随机数（xorshift64*）。
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 返回 `[0, bound)` 内的随机数。
    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// Fisher-Yates 洗牌（同 seed 同顺序，保证抽样可复跑）。
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.next_usize(i + 1);
            items.swap(i, j);
        }
    }
}

// =========================================================
// 内置测试夹具数据（构建失败时的兜底）
// =========================================================

/// 数据源文件输入格式（probe build --source）。
#[derive(Debug, serde::Deserialize)]
struct ProbeSourceFile {
    persona_uid: Option<String>,
    #[serde(default)]
    messages: Vec<SourceMessage>,
    #[serde(default)]
    events: Vec<SourceEvent>,
}

#[derive(Debug, serde::Deserialize)]
struct SourceMessage {
    question: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    source_ref: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SourceEvent {
    title: String,
    #[serde(default)]
    summary: String,
}

/// 内置语气模仿夹具（(用户问题, persona 原回复) 配对）。
///
/// 内容说明: 示例角色 persona（char-0001）的典型回应风格——
/// 工作吐槽安抚、生活建议、情绪陪伴，覆盖 v1.5「行为驱动」的目标情境。
fn fixture_tone_pairs() -> Vec<(String, String)> {
    vec![
        (
            "今天上班被领导批评了，说我方案写得不行".to_string(),
            "别太往心里去，领导批评方案不代表否定你这个人。把意见一条条记下来，改完这版肯定能行。"
                .to_string(),
        ),
        (
            "周末想约你出来玩，你有空吗".to_string(),
            "周末我一般不安排太满。你想去哪里？公园散步或者找家安静的咖啡馆都行。".to_string(),
        ),
        (
            "我养的猫今天吐了，好担心".to_string(),
            "先观察一下是不是吃太快或者毛球。如果持续吐或者精神不好，尽快带去看医生比较稳妥。"
                .to_string(),
        ),
        (
            "项目又要延期了，客户催得紧".to_string(),
            "延期不全是坏事，说明需求一直在变。跟客户沟通好新的时间点，把预期管理做到位更重要。"
                .to_string(),
        ),
        (
            "晚上总是失眠，脑子里全是工作的事".to_string(),
            "躺下前把明天要做的事写在纸上，清空脑袋再睡。工作的事明天再想也不迟。".to_string(),
        ),
        (
            "今天终于把健身卡办了，坚持了一个月".to_string(),
            "坚持一个月已经很厉害了，最难的起步阶段你已经过了。接下来保持节奏就行，别贪快。"
                .to_string(),
        ),
        (
            "想换个工作，又怕不稳定".to_string(),
            "有换工作的念头说明你在成长。先想清楚你最在意什么——工资、发展还是氛围，排个序再决定。"
                .to_string(),
        ),
        (
            "手机丢了，里面有很多照片".to_string(),
            "照片丢了确实心疼。以后重要的照片记得备份到云端，这次就当买个教训吧。".to_string(),
        ),
        (
            "跟室友吵架了，不知道怎么办".to_string(),
            "先冷静一晚，明天再谈。吵架时说的话都当不得真，等情绪过去再沟通才是正事。".to_string(),
        ),
        (
            "今天加班到十点，累死了".to_string(),
            "辛苦了，早点回去休息。工作是做不完的，身体才是自己的。".to_string(),
        ),
        (
            "第一次做饭，把厨房搞得一团糟".to_string(),
            "第一次做饭都这样，谁都是从炸厨房开始的。能吃就行，下次一定会更好。".to_string(),
        ),
        (
            "准备考研，但一直静不下心".to_string(),
            "学习最难的是开始那半小时。先把手机放远，定个 25 分钟的小目标，进入状态就好了。"
                .to_string(),
        ),
    ]
}

/// 内置事实记忆夹具（(问题, 事件摘要, 事件标题)）。
fn fixture_fact_events() -> Vec<(String, String, String)> {
    let raw = [
        (
            "东京旅行",
            "2024 年 3 月和朋友去了东京，看了樱花，去了浅草寺和秋叶原，非常开心。",
        ),
        (
            "养猫",
            "去年收养了一只三花猫，取名「团子」，现在一岁半，性格粘人。",
        ),
        (
            "跳槽到新公司",
            "2025 年初从上一家公司跳槽，现在做后端开发，团队氛围不错。",
        ),
        (
            "跑步习惯",
            "从今年春天开始每周跑三次五公里，配速从 8 分提高到 6 分半。",
        ),
        (
            "学吉他",
            "去年开始学吉他，已经会弹三首完整的曲子，最喜欢《晴天》。",
        ),
        (
            "搬家",
            "去年秋天搬到了离公司更近的小区，通勤时间从一小时缩短到二十分钟。",
        ),
        (
            "第一次马拉松",
            "上个月完成了人生第一个半程马拉松，用时 2 小时 15 分。",
        ),
        (
            "考驾照",
            "今年六月拿到了驾照，科目二补考了一次，科目三一次过。",
        ),
        (
            "近视手术",
            "前年做了近视手术，现在视力恢复到 1.0，彻底告别眼镜。",
        ),
        (
            "养多肉",
            "办公桌上养了一排多肉，最喜欢那棵熊童子，已经养了两年。",
        ),
        (
            "换手机",
            "今年换了新手机，主要是为了拍照，拍风景和猫都很满意。",
        ),
        ("学游泳", "去年夏天学会了蛙泳，现在每周去一次游泳馆。"),
    ];
    raw.iter()
        .map(|(title, summary)| {
            (
                format!("还记得「{}」这件事吗？", title),
                summary.to_string(),
                title.to_string(),
            )
        })
        .collect()
}

/// 内置情感表达夹具（(情绪化用户消息, persona 原回复)）。
///
/// 覆盖四类情境：难过/委屈（需安慰）、生气（需安抚）、担心/焦虑（需宽慰）、
/// 开心/成功（需分享喜悦）。每个 question 必须命中情感线索（`has_emotion_cue`），
/// 否则不会被 emotion 维收集/评估语义判定为情绪化情境。
fn fixture_emotion_pairs() -> Vec<(String, String)> {
    vec![
        (
            "今天上班被领导当着全组批评，太难过了".to_string(),
            "被当众批评确实不好受。先深呼吸，别急着自我否定——领导针对的是方案不是人，我们把问题一条条改掉就好。"
                .to_string(),
        ),
        (
            "和最好的朋友吵架了，很伤心".to_string(),
            "吵架不代表友谊结束。等彼此冷静下来，你可以先发条消息说声在乎这段关系，很多时候一个小台阶就能和好。"
                .to_string(),
        ),
        (
            "养了五年的猫生病住院，好担心".to_string(),
            "别太担心，现代兽医能处理很多问题。先配合医生把检查做完，你陪着它，它会更有安全感。".to_string(),
        ),
        (
            "项目上线前出 bug，被气得想摔电脑".to_string(),
            "先离开电脑五分钟，气头上做决定容易后悔。bug 总归能修，身体和气坏了不划算。".to_string(),
        ),
        (
            "准备了三个月的考试差两分没过，很崩溃".to_string(),
            "差两分说明你已经在门口了，这很可惜但也说明方向没错。休息两天，复盘下错题，下次一定行。".to_string(),
        ),
        (
            "我好像把事情搞砸了，很自责".to_string(),
            "别把一次失误放大成自我否定。搞砸了就一起想办法补救，你愿意承担已经很了不起了。".to_string(),
        ),
        (
            "深夜一个人待着，觉得很孤独".to_string(),
            "夜深人静的时候情绪容易放大。你不是一个人，随时可以来找我聊聊，明天太阳照常升起。".to_string(),
        ),
        (
            "最近工作压力好大，有点撑不住".to_string(),
            "压力大的时候先允许自己歇一歇。把任务拆小，一天只做好一件，你会发现没有想象中那么难。".to_string(),
        ),
        (
            "我升职了！太开心了".to_string(),
            "恭喜！这是你应得的。记得好好庆祝一下，也别忘了感谢一路支持你的人。".to_string(),
        ),
        (
            "终于跑完人生第一个半马，太兴奋了".to_string(),
            "太棒了！半马可不是随便谁都能完成的，为自己骄傲一下，好好犒劳自己。".to_string(),
        ),
        (
            "收到理想公司的 offer 了，好高兴".to_string(),
            "真替你高兴！这是实力加运气的证明。入职前好好放松几天，新旅程会很好的。".to_string(),
        ),
        (
            "我种的向日葵开花了，很开心".to_string(),
            "亲手养大的花开出来最有成就感了。拍张照留个纪念，这份喜悦值得好好记住。".to_string(),
        ),
    ]
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DeterministicRng ----

    #[test]
    fn rng_same_seed_same_sequence() {
        let mut a = DeterministicRng::new(42);
        let mut b = DeterministicRng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64(), "同 seed 序列必须一致");
        }
    }

    #[test]
    fn rng_different_seed_different_sequence() {
        let mut a = DeterministicRng::new(1);
        let mut b = DeterministicRng::new(2);
        let mut same = 0;
        for _ in 0..10 {
            if a.next_u64() == b.next_u64() {
                same += 1;
            }
        }
        assert!(same <= 1, "不同 seed 的序列应几乎完全不同（同次数={same}）");
    }

    #[test]
    fn rng_shuffle_is_permutation() {
        let mut rng = DeterministicRng::new(7);
        let mut items = vec![1, 2, 3, 4, 5];
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3, 4, 5], "洗牌必须是排列（不增不减）");
    }

    // ---- sample_with_fallback ----

    #[test]
    fn sample_fallback_uses_fixture_when_no_candidates() {
        let (items, real) = sample_with_fallback::<i32>(&[], &[10, 20, 30], 2, 99);
        assert_eq!(items, vec![10, 20]);
        assert_eq!(real, 0, "无真实候选时 real=0");
    }

    #[test]
    fn sample_fallback_deterministic_same_seed() {
        let cands = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let (a, _) = sample_with_fallback(&cands, &[0], 4, 123);
        let (b, _) = sample_with_fallback(&cands, &[0], 4, 123);
        assert_eq!(a, b, "同 seed 抽样结果必须一致（可复跑）");
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn sample_fallback_pads_with_fixture_when_short() {
        let cands = vec![1, 2];
        let fixture = vec![100, 200, 300];
        let (items, real) = sample_with_fallback(&cands, &fixture, 4, 5);
        assert_eq!(real, 2);
        assert_eq!(items.len(), 4, "不足部分必须用夹具补满");
        // 真实数据在前（洗牌后顺序不定，按集合比较）
        let mut head = items[..2].to_vec();
        head.sort_unstable();
        assert_eq!(head, vec![1, 2], "真实数据应排在前面");
        assert_eq!(&items[2..], &[100, 200], "夹具补齐排在真实数据之后");
    }

    // ---- 档位 ----

    #[test]
    fn default_variants_are_representative_pairs() {
        let variants = default_variants();
        assert_eq!(variants.len(), 4);
        // baseline 即对照基准值
        let base = &variants[0];
        assert_eq!(base.id, "baseline");
        assert_eq!(base.theta_gap_minutes, 10);
        assert_eq!(base.max_msgs_per_block, 80);
        assert_eq!(base.retrieve_top_k, 3);
        // 每个档位只动一个参数（相对定稿基准 baseline）
        for v in &variants[1..] {
            let changed = [
                v.theta_gap_minutes != base.theta_gap_minutes,
                v.max_msgs_per_block != base.max_msgs_per_block,
                v.retrieve_top_k != base.retrieve_top_k,
            ]
            .iter()
            .filter(|b| **b)
            .count();
            assert_eq!(changed, 1, "档位 {} 应只变化一个参数", v.id);
        }
    }

    // ---- read_experiment（实验结果文件缺失/非法 → 业务校验失败）----

    #[test]
    fn read_experiment_missing_file_is_validation_error() {
        let err =
            read_experiment(Path::new("/nonexistent/run.json")).expect_err("文件缺失必须报错");
        let ramaria_err = err.downcast_ref::<RamariaError>();
        assert!(
            matches!(ramaria_err, Some(RamariaError::Validation { .. })),
            "文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
        );
    }

    #[test]
    fn read_experiment_malformed_json_is_validation_error() {
        let path = std::env::temp_dir().join("ramaria_probe_bad_experiment.json");
        std::fs::write(&path, "{ not json").expect("写入临时文件失败");
        let err = read_experiment(&path).expect_err("非法 JSON 必须报错");
        let ramaria_err = err.downcast_ref::<RamariaError>();
        assert!(matches!(ramaria_err, Some(RamariaError::Validation { .. })));
        let _ = std::fs::remove_file(&path);
    }

    // ---- 统计法（--repeat）----

    /// metric_stat：单样本退化为该值，stddev=0，CI=该值。
    #[test]
    fn metric_stat_single_sample_degenerates() {
        let s = metric_stat(&[42.0]);
        assert_eq!(s.n, 1);
        assert_eq!(s.mean, 42.0);
        assert_eq!(s.stddev, 0.0);
        assert_eq!(s.ci_low, 42.0);
        assert_eq!(s.ci_high, 42.0);
    }

    /// metric_stat：空样本 → 全零。
    #[test]
    fn metric_stat_empty_is_zero() {
        let s = metric_stat(&[]);
        assert_eq!(s.n, 0);
        assert_eq!(s.mean, 0.0);
        assert_eq!(s.stddev, 0.0);
        assert_eq!(s.ci_low, 0.0);
        assert_eq!(s.ci_high, 0.0);
    }

    /// metric_stat：多样本 → 均值正确、stddev 为样本标准差、CI 对称且随 n 增大收窄。
    #[test]
    fn metric_stat_multiple_mean_stddev_ci() {
        let samples = [10.0, 12.0, 11.0]; // mean=11
        let s = metric_stat(&samples);
        assert_eq!(s.n, 3);
        assert!((s.mean - 11.0).abs() < 1e-9, "均值应为 11, 实际 {}", s.mean);
        // 样本标准差 = sqrt(((1)^2+(-1)^2+0)/2) = sqrt(1)=1
        assert!(
            (s.stddev - 1.0).abs() < 1e-9,
            "stddev 应为 1, 实际 {}",
            s.stddev
        );
        // t(2,0.975)=4.303, half = 4.303*1/sqrt(3)
        let half = 4.303 / 3.0f64.sqrt();
        assert!((s.ci_low - (11.0 - half)).abs() < 1e-6);
        assert!((s.ci_high - (11.0 + half)).abs() < 1e-6);
        assert!(s.ci_low < s.mean && s.mean < s.ci_high);
    }

    /// metric_stat：n 增大 → 置信区间收窄（同一分布更稳）。
    #[test]
    fn metric_stat_more_samples_narrower_ci() {
        let small = metric_stat(&[10.0, 12.0, 11.0, 10.5, 11.2]);
        let bigger = metric_stat(&[
            10.0, 12.0, 11.0, 10.5, 11.2, 10.8, 11.4, 10.9, 11.1, 10.7, 11.3, 10.6, 11.0, 11.2,
            10.9, 11.1, 10.8, 11.0, 10.9, 11.1, 11.0, 11.0, 11.0, 11.0,
        ]);
        let w_small = small.ci_high - small.ci_low;
        let w_bigger = bigger.ci_high - bigger.ci_low;
        assert!(
            w_bigger < w_small,
            "样本量增大后 CI 应收窄, 小 {w_small} vs 大 {w_bigger}"
        );
    }

    /// t_critical_975：边界值正确且单调递减趋近于 2。
    #[test]
    fn t_critical_975_table_and_approximation() {
        assert!((t_critical_975(2) - 12.706).abs() < 1e-6);
        assert!((t_critical_975(5) - 2.776).abs() < 1e-6);
        // 超表项 → 近似 2.0
        assert_eq!(t_critical_975(100), 2.0);
        // 单调递减（自由度越高，临界值越小）
        assert!(t_critical_975(3) < t_critical_975(2));
        assert!(t_critical_975(8) < t_critical_975(5));
    }

    /// aggregate_repeat_stats：按档位+item 配对，缺轮样本以实际计数。
    #[test]
    fn aggregate_repeat_stats_pairs_by_variant_and_item() {
        // 构造两个 round 的 ProbeExperiment
        fn round(item_chars: &[(usize, usize)]) -> ProbeExperiment {
            let vr = ProbeVariantResult {
                variant_id: "v1".to_string(),
                description: "档位".to_string(),
                params: VariantParams {
                    theta_gap_minutes: 30,
                    max_msgs_per_block: 40,
                    retrieve_top_k: 3,
                    ablation: None,
                },
                runs: item_chars
                    .iter()
                    .map(|(id, chars)| ProbeRunItem {
                        item_id: format!("fact-{id:04}"),
                        dimension: "fact".to_string(),
                        question: "q".to_string(),
                        reply: String::new(),
                        metrics: ProbeMetrics {
                            reply_chars: *chars,
                            elapsed_ms: 100,
                        },
                        error: None,
                    })
                    .collect(),
                failed_count: 0,
            };
            ProbeExperiment {
                dataset_file: "d".to_string(),
                dataset_seed: 1,
                persona_uid: "p".to_string(),
                rebuild_utt: true,
                variants: vec![vr],
                repeat: None,
                generated_at: "t".to_string(),
            }
        }
        let r1 = round(&[(1, 10), (2, 20)]);
        let r2 = round(&[(1, 14), (2, 24)]);
        let stats = aggregate_repeat_stats(&[r1, r2]);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].per_item.len(), 2);
        // item1: chars=[10,14] mean=12
        let it1 = &stats[0].per_item[0];
        assert_eq!(it1.item_id, "fact-0001");
        assert!((it1.reply_chars.mean - 12.0).abs() < 1e-6);
        assert_eq!(it1.reply_chars.n, 2);
        // item2: chars=[20,24] mean=22
        let it2 = &stats[0].per_item[1];
        assert!((it2.reply_chars.mean - 22.0).abs() < 1e-6);
        // 缺口 A：rounds 保留该档位每一轮的完整结果明细（逐轮全量 reply）
        assert_eq!(stats[0].rounds.len(), 2, "应保留两轮的完整结果");
        // round1 item chars=10 / round2 item chars=14
        assert_eq!(stats[0].rounds[0].runs[0].metrics.reply_chars, 10);
        assert_eq!(stats[0].rounds[1].runs[0].metrics.reply_chars, 14);
        assert_eq!(stats[0].rounds[0].runs.len(), 2);
        assert_eq!(stats[0].rounds[1].runs.len(), 2);
    }

    /// 缺口 A 向后兼容：旧 repeat 聚合 JSON 无 `rounds` 字段时反序列化为空，
    /// 序列化时空 `rounds` 被省略（不破坏旧文件读/写与契约）。
    #[test]
    fn repeat_rounds_serde_roundtrip_and_backcompat() {
        // 新格式：rounds 非空，序列化应保留逐轮明细。
        let with_rounds = VariantRepeatStats {
            variant_id: "v1".to_string(),
            per_item: vec![],
            rounds: vec![ProbeVariantResult {
                variant_id: "v1".to_string(),
                description: "d".to_string(),
                params: VariantParams {
                    theta_gap_minutes: 30,
                    max_msgs_per_block: 40,
                    retrieve_top_k: 3,
                    ablation: None,
                },
                runs: vec![],
                failed_count: 0,
            }],
        };
        let roundtrip: VariantRepeatStats =
            serde_json::from_str(&serde_json::to_string(&with_rounds).unwrap()).unwrap();
        assert_eq!(roundtrip.rounds.len(), 1);

        // 旧格式：JSON 无 rounds 字段 → 反序列化 rounds 为空（serde default）。
        let old = r#"{"variant_id":"v1","per_item":[]}"#;
        let parsed: VariantRepeatStats = serde_json::from_str(old).unwrap();
        assert!(parsed.rounds.is_empty());

        // 空的 rounds 序列化时应省略该键（skip_serializing_if），保持与旧文件最小差异。
        let s = serde_json::to_string(&parsed).unwrap();
        assert!(!s.contains("rounds"), "空 rounds 应省略，实际: {s}");
    }

    // ---- 输入文件缺失统一归业务校验失败（--results / --evaluation / --calibration / --dataset / --source）----

    #[test]
    fn read_manual_scores_missing_file_is_validation_error() {
        let err = read_manual_scores(Path::new("/nonexistent/calib.json"))
            .expect_err("校准文件缺失必须报错");
        let ramaria_err = err.downcast_ref::<RamariaError>();
        assert!(
            matches!(ramaria_err, Some(RamariaError::Validation { .. })),
            "校准文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
        );
    }

    #[test]
    fn load_golden_references_missing_file_is_validation_error() {
        let err = load_golden_references(Path::new("/nonexistent/dataset.json"))
            .expect_err("数据集缺失必须报错");
        let ramaria_err = err.downcast_ref::<RamariaError>();
        assert!(matches!(ramaria_err, Some(RamariaError::Validation { .. })));
    }

    // ---- fixture ----

    #[test]
    fn fixture_data_covers_default_scale() {
        assert!(fixture_tone_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
        assert!(fixture_fact_events().len() >= DEFAULT_QUESTIONS_PER_DIM);
        assert!(fixture_emotion_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
        // emotion 夹具的 question 必须命中情感线索（否则不会被收集/评分语义判定）
        for (q, _) in fixture_emotion_pairs() {
            assert!(has_emotion_cue(&q), "emotion 夹具问题应含情感线索: {q}");
        }
    }

    // ---- select_target_persona（不按发言量，白名单过滤对方）----

    /// 构造测试 persona。
    fn test_persona(uid: &str, kind: PersonaKind) -> ramaria_core::types::Persona {
        ramaria_core::types::Persona::new(
            uid.to_string(),
            uid.to_string(),
            kind,
            1,
            "test".to_string(),
        )
    }

    #[test]
    fn select_persona_excludes_user_kind() {
        // 我方（kind=user）不得入选探针目标
        let personas = vec![
            test_persona("user-0001", PersonaKind::User),
            test_persona("char-0001", PersonaKind::Char),
        ];
        assert_eq!(select_target_persona(&personas, None), "char-0001");
    }

    #[test]
    fn select_persona_first_whitelisted() {
        // 多个对方 persona：取第一个白名单，不引入发言量排序
        let personas = vec![
            test_persona("user-0001", PersonaKind::User),
            test_persona("anim-0001", PersonaKind::Anim),
            test_persona("char-0001", PersonaKind::Char),
            test_persona("hist-0001", PersonaKind::Hist),
        ];
        assert_eq!(select_target_persona(&personas, None), "anim-0001");
    }

    #[test]
    fn select_persona_explicit_wins() {
        // 显式 --persona 优先（不校验 kind，尊重用户指定）
        let personas = vec![
            test_persona("user-0001", PersonaKind::User),
            test_persona("char-0001", PersonaKind::Char),
        ];
        assert_eq!(
            select_target_persona(&personas, Some("rama-0001")),
            "rama-0001"
        );
    }

    #[test]
    fn select_persona_all_user_role_falls_back() {
        // 全 user-role 退化场景：无白名单 persona → 默认 char-0001（夹具兜底）
        let personas = vec![
            test_persona("user-0001", PersonaKind::User),
            test_persona("user-0002", PersonaKind::User),
        ];
        assert_eq!(select_target_persona(&personas, None), DEFAULT_PERSONA);
    }

    #[test]
    fn select_persona_empty_falls_back() {
        assert_eq!(select_target_persona(&[], None), DEFAULT_PERSONA);
    }

    // ---- build_from_fixture ----

    #[test]
    fn build_from_fixture_shape() {
        let ds = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
        assert_eq!(ds.schema_version, DATASET_SCHEMA_VERSION);
        assert_eq!(ds.persona_uid, DEFAULT_PERSONA);
        assert_eq!(ds.source, "fixture");
        assert_eq!(ds.dimensions, vec!["tone", "fact", "emotion"]);
        assert_eq!(ds.items.len(), DEFAULT_QUESTIONS_PER_DIM * 3);
        assert_eq!(ds.variants.len(), 4);
        // 全部来自夹具
        assert!(ds.items.iter().all(|i| i.source == "fixture"));
        // 每维恰好 qpd 题
        for dim in ["tone", "fact", "emotion"] {
            assert_eq!(
                ds.items.iter().filter(|i| i.dimension == dim).count(),
                DEFAULT_QUESTIONS_PER_DIM,
                "维度 {dim} 应有 qpd 题"
            );
        }
        // 每题都有 reference 与 id（前缀含 emotion-）
        for item in &ds.items {
            assert!(item.reference.is_some(), "{} 应有参考回答", item.id);
            assert!(
                item.id.starts_with("tone-")
                    || item.id.starts_with("fact-")
                    || item.id.starts_with("emotion-")
            );
        }
        // seed 固定 → 复跑一致
        let again = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
        let qs: Vec<&str> = ds.items.iter().map(|i| i.question.as_str()).collect();
        let qs2: Vec<&str> = again.items.iter().map(|i| i.question.as_str()).collect();
        assert_eq!(qs, qs2, "同 seed 复跑必须产生相同测试集");
    }

    // ---- 数据集序列化 roundtrip ----

    #[test]
    fn dataset_roundtrip_json() {
        let ds = build_from_fixture(DEFAULT_PERSONA, 3, 42);
        let json = serde_json::to_string(&ds).expect("序列化失败");
        let back: ProbeDataset = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(back.items.len(), ds.items.len());
        assert_eq!(back.items[0].question, ds.items[0].question);
        assert_eq!(back.variants.len(), ds.variants.len());
    }

    // ---- 问题模板 ----

    #[test]
    fn fact_question_template() {
        let (q, _ref, title) = fixture_fact_events()[0].clone();
        assert!(q.contains(&title), "事实记忆问题应包含事件标题");
        assert!(q.contains("还记得"), "事实记忆问题应使用回忆问法");
    }

    // ---- filter_variants ----

    #[test]
    fn filter_variants_selects_and_ignores_unknown() {
        let variants = default_variants();
        let filtered = filter_variants(&variants, Some("baseline,top_k_1,nonexistent"));
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, "baseline");
        assert_eq!(filtered[1].id, "top_k_1");
    }

    #[test]
    fn filter_variants_empty_falls_back_to_all() {
        let variants = default_variants();
        let filtered = filter_variants(&variants, Some("bad,bad2"));
        assert_eq!(filtered.len(), 4, "过滤为空时应回退全部档位");
    }

    // ---- 数据源文件解析 ----

    #[test]
    fn build_from_file_parses_source_json() {
        let tmp = std::env::temp_dir().join(format!("ramaria_probe_src_{}", std::process::id()));
        let path = tmp.join("source.json");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            &path,
            r#"{
                "persona_uid": "char-0009",
                "messages": [
                    {"question": "今天好累", "reply": "早点休息"},
                    {"question": "周末去哪", "reply": "去公园"}
                ],
                "events": [
                    {"title": "学钢琴", "summary": "今年开始学钢琴，会弹《致爱丽丝》"}
                ]
            }"#,
        )
        .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let ds = rt
            .block_on(build_from_file(&path, DEFAULT_PERSONA, 3, 7))
            .expect("文件构建应成功");
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(ds.persona_uid, "char-0009", "文件中的 persona_uid 应优先");
        assert_eq!(ds.source, "file");
        assert_eq!(ds.items.len(), 9, "3 维 × 3 题");
        // 真实数据在前：tone 2 条 + fact 1 条 + emotion 1 条
        // （"今天好累"含情感线索"累" → 同时进入 emotion 候选；"周末去哪"不含）。
        assert_eq!(ds.items.iter().filter(|i| i.source == "file").count(), 4);
        assert_eq!(ds.items.iter().filter(|i| i.source == "fixture").count(), 5);
        assert_eq!(
            ds.items.iter().filter(|i| i.dimension == "emotion").count(),
            3,
            "emotion 维应补齐 3 题"
        );
        assert_eq!(ds.dimensions, vec!["tone", "fact", "emotion"]);
    }

    #[test]
    fn build_from_file_missing_is_err() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let missing = std::env::temp_dir().join("ramaria_probe_nonexistent.json");
        let result = rt.block_on(build_from_file(&missing, DEFAULT_PERSONA, 3, 7));
        assert!(result.is_err(), "文件不存在应返回 Err（由上层夹具兜底）");
    }

    // =========================================================
    // M5a 消融档位 Profile（D-V17-015 / 技术报告 §16.3）
    // =========================================================

    /// 全部 11 个名称可解析且往返一致；未知名称返回 None。
    #[test]
    fn ablation_profile_parse_roundtrip_all_names() {
        assert_eq!(ABLATION_PROFILE_NAMES.len(), 11);
        for name in ABLATION_PROFILE_NAMES {
            let p =
                AblationProfile::parse_name(name).unwrap_or_else(|| panic!("名称 {name} 应可解析"));
            assert_eq!(p.name(), name, "解析后名称往返一致");
        }
        assert!(AblationProfile::parse_name("unknown").is_none());
        assert!(AblationProfile::parse_name("").is_none());
        assert!(AblationProfile::parse_name("f0").is_none(), "大小写敏感");
    }

    /// ablation_variants：11 档、id=Profile 名、utt 取定稿基准、ablation 回显。
    #[test]
    fn ablation_variants_shape_and_baseline_utt() {
        let variants = ablation_variants();
        assert_eq!(variants.len(), 11);
        for v in &variants {
            assert_eq!(
                v.ablation.as_deref(),
                Some(v.id.as_str()),
                "ablation 与 id 一致"
            );
            assert_eq!(v.theta_gap_minutes, 10, "消融档位 utt 取定稿基准");
            assert_eq!(v.max_msgs_per_block, 80);
            assert_eq!(v.retrieve_top_k, 3);
            assert!(v.description.contains("[消融]"));
        }
        // 覆盖 B0/B1/F0/F1~F4/S_* 全集
        let ids: Vec<&str> = variants.iter().map(|v| v.id.as_str()).collect();
        for name in ABLATION_PROFILE_NAMES {
            assert!(ids.contains(&name), "缺少档位 {name}");
        }
    }

    /// B0 无记忆注入：闸门全关；B1 压缩摘要基座：仅 memory_rag 开。
    #[test]
    fn ablation_profile_b0_b1_gates() {
        let mut cfg = ramaria_core::config::RamariaConfig::default();
        AblationProfile::B0.apply_to(&mut cfg);
        assert!(!cfg.injection.behavior);
        assert!(!cfg.injection.knowledge);
        assert!(!cfg.injection.speaking_style);
        assert!(!cfg.injection.examples);
        assert!(!cfg.injection.utt);
        assert!(!cfg.injection.narrative);
        assert!(!cfg.injection.bridge);
        assert!(!cfg.injection.memory_rag, "B0 关闭 RAG 相关记忆");

        let mut cfg = ramaria_core::config::RamariaConfig::default();
        AblationProfile::B1.apply_to(&mut cfg);
        assert!(cfg.injection.memory_rag, "B1 保留 RAG 摘要基座");
        assert!(!cfg.injection.behavior);
        assert!(!cfg.injection.knowledge);
        assert!(!cfg.injection.speaking_style);
        assert!(!cfg.injection.examples);
        assert!(!cfg.injection.utt);
        assert!(!cfg.injection.narrative);
        assert!(!cfg.injection.bridge);
    }

    /// F0 全开（与 None 等同）；F1~F4 在全开基础上只关对应层。
    #[test]
    fn ablation_profile_f0_to_f4_gates() {
        let mut cfg = ramaria_core::config::RamariaConfig::default();
        cfg.injection = ramaria_core::config::InjectionGate::all_off();
        AblationProfile::F0.apply_to(&mut cfg);
        assert!(cfg.injection.behavior && cfg.injection.memory_rag && cfg.injection.utt);
        assert!(cfg.injection.narrative && cfg.injection.bridge);

        let mut f1 = ramaria_core::config::RamariaConfig::default();
        AblationProfile::F1.apply_to(&mut f1);
        assert!(!f1.injection.behavior, "F1 关行为层");
        assert!(f1.injection.knowledge && f1.injection.memory_rag && f1.injection.utt);
        assert!(f1.injection.narrative && f1.injection.bridge && f1.injection.examples);

        let mut f2 = ramaria_core::config::RamariaConfig::default();
        AblationProfile::F2.apply_to(&mut f2);
        assert!(!f2.injection.knowledge, "F2 关知识层");
        assert!(f2.injection.behavior && f2.injection.memory_rag);

        let mut f3 = ramaria_core::config::RamariaConfig::default();
        AblationProfile::F3.apply_to(&mut f3);
        assert!(!f3.injection.speaking_style, "F3 关表达层（风格）");
        assert!(!f3.injection.examples, "F3 关表达层（示例）");
        assert!(!f3.injection.utt, "F3 关表达层（原文样例）");
        assert!(f3.injection.behavior && f3.injection.knowledge);
        assert!(f3.injection.narrative && f3.injection.bridge && f3.injection.memory_rag);

        let mut f4 = ramaria_core::config::RamariaConfig::default();
        AblationProfile::F4.apply_to(&mut f4);
        assert!(!f4.injection.narrative, "F4 关脉络（近期脉络）");
        assert!(!f4.injection.bridge, "F4 关脉络（桥接）");
        assert!(f4.injection.utt && f4.injection.behavior && f4.injection.knowledge);
        assert!(f4.injection.speaking_style && f4.injection.examples && f4.injection.memory_rag);
    }

    /// S_* 前置单层：B1 基座（memory_rag）之上只开目标层。
    #[test]
    fn ablation_profile_s_group_gates() {
        let mut sb = ramaria_core::config::RamariaConfig::default();
        AblationProfile::SBehavior.apply_to(&mut sb);
        assert!(sb.injection.memory_rag && sb.injection.behavior);
        assert!(!sb.injection.knowledge && !sb.injection.speaking_style);
        assert!(!sb.injection.examples && !sb.injection.utt);
        assert!(!sb.injection.narrative && !sb.injection.bridge);

        let mut sk = ramaria_core::config::RamariaConfig::default();
        AblationProfile::SKnowledge.apply_to(&mut sk);
        assert!(sk.injection.memory_rag && sk.injection.knowledge);
        assert!(!sk.injection.behavior);

        let mut se = ramaria_core::config::RamariaConfig::default();
        AblationProfile::SExpression.apply_to(&mut se);
        assert!(se.injection.memory_rag);
        assert!(se.injection.speaking_style && se.injection.examples && se.injection.utt);
        assert!(!se.injection.behavior && !se.injection.knowledge);
        assert!(!se.injection.narrative && !se.injection.bridge);

        let mut sn = ramaria_core::config::RamariaConfig::default();
        AblationProfile::SNarrative.apply_to(&mut sn);
        assert!(sn.injection.memory_rag);
        assert!(sn.injection.narrative && sn.injection.bridge);
        assert!(!sn.injection.utt && !sn.injection.behavior && !sn.injection.knowledge);
        assert!(!sn.injection.speaking_style && !sn.injection.examples);
    }

    /// ProbeVariant serde 向后兼容：旧数据集（无 ablation 字段）→ None；
    /// 带 ablation 的档位 roundtrip 保留该字段。
    #[test]
    fn probe_variant_ablation_serde_backcompat() {
        // 旧格式：无 ablation 字段 → 反序列化为 None。
        let old = r#"{"id":"baseline","description":"对照基准","theta_gap_minutes":10,"max_msgs_per_block":80,"retrieve_top_k":3}"#;
        let parsed: ProbeVariant = serde_json::from_str(old).unwrap();
        assert!(parsed.ablation.is_none(), "旧数据集 ablation 应为 None");

        // 新格式：ablation 存在则保留。
        let v = ProbeVariant {
            id: "F1".to_string(),
            description: "d".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: Some("F1".to_string()),
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            json.contains("\"ablation\":\"F1\""),
            "ablation 应序列化: {json}"
        );
        let back: ProbeVariant = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ablation.as_deref(), Some("F1"));

        // ablation=None 序列化时省略该键（保持 M1 旧产物最小差异）。
        let plain = ProbeVariant {
            ablation: None,
            ..v
        };
        let plain_json = serde_json::to_string(&plain).unwrap();
        assert!(
            !plain_json.contains("ablation"),
            "None ablation 应省略: {plain_json}"
        );
    }

    /// F0 档位（ablation="F0"）注入闸门全开——与 ablation=None 行为一致。
    #[test]
    fn ablation_f0_equivalent_to_none() {
        let mut with_f0 = ramaria_core::config::RamariaConfig::default();
        AblationProfile::F0.apply_to(&mut with_f0);
        let default_cfg = ramaria_core::config::RamariaConfig::default();
        assert!(
            with_f0.injection.behavior == default_cfg.injection.behavior
                && with_f0.injection.memory_rag == default_cfg.injection.memory_rag,
            "F0 闸门应与默认全开一致"
        );
    }

    // =========================================================
    // M5a emotion 第三维（T-V17-5a-002）
    // =========================================================

    /// 情感线索判定：负面/正面触发词命中；中性消息不命中。
    #[test]
    fn emotion_cue_detection_cases() {
        assert!(has_emotion_cue("今天被领导骂了，很难过"));
        assert!(has_emotion_cue("我好生气，想投诉"));
        assert!(has_emotion_cue("收到 offer 了，太开心了"));
        assert!(!has_emotion_cue("周末一起去爬山吗"));
        assert!(!has_emotion_cue("请问这个功能怎么用"));
        assert!(has_negative_cue("很担心") && !has_positive_cue("很担心"));
        assert!(!has_negative_cue("太开心了") && has_positive_cue("太开心了"));
    }

    /// rubric：负面情境 + 充分安慰 → 1.0；1 个标记 → 0.5；无标记 → 0.0。
    #[test]
    fn emotion_rubric_negative_situation() {
        let q = "今天被领导当众批评，好难过";
        // 充分安慰：命中多个安慰/共情标记
        let full = score_emotion_item("别难过，我理解你，会好的，先深呼吸", q);
        assert_eq!(full.score, 1.0);
        assert!(full.situation_negative && !full.situation_positive);
        // 单标记：部分回应
        let partial = score_emotion_item("别担心，睡一觉就好了", q);
        assert_eq!(partial.score, 0.5);
        assert_eq!(partial.marker_hit, 1);
        // 无标记：冷漠回应
        let cold = score_emotion_item("这个方案本身就有问题，明天重写吧", q);
        assert_eq!(cold.score, 0.0);
        // 空回复
        let empty = score_emotion_item("", q);
        assert_eq!(empty.score, 0.0);
    }

    /// rubric：正面情境 + 分享喜悦 → 1.0；单标记 → 0.5；无 → 0.0。
    #[test]
    fn emotion_rubric_positive_situation() {
        let q = "我升职了，太开心了";
        let full = score_emotion_item("太好了，真棒，恭喜你！这是你应得的", q);
        assert_eq!(full.score, 1.0);
        assert!(full.situation_positive && !full.situation_negative);
        let partial = score_emotion_item("嗯，不错", q);
        assert_eq!(partial.score, 0.5);
        let cold = score_emotion_item("下次注意保持", q);
        assert_eq!(cold.score, 0.0);
    }

    /// 中性情境：两类标记合计弱判定。
    #[test]
    fn emotion_rubric_neutral_situation() {
        let q = "帮我看看这段代码";
        let score = score_emotion_item("别担心，我帮你看看，一起加油", q);
        assert_eq!(score.score, 1.0, "中性情境按共情标记合计");
        assert!(!score.situation_negative && !score.situation_positive);
    }

    // =========================================================
    // M5a --repeat 逐轮评分聚合（T-V17-5a-003）
    // =========================================================

    /// 构造一个含单条 fact 题的轮次结果。
    fn fact_round(reply: &str) -> ProbeVariantResult {
        ProbeVariantResult {
            variant_id: "v1".to_string(),
            description: "d".to_string(),
            params: VariantParams {
                theta_gap_minutes: 10,
                max_msgs_per_block: 80,
                retrieve_top_k: 3,
                ablation: None,
            },
            runs: vec![ProbeRunItem {
                item_id: "fact-0001".to_string(),
                dimension: "fact".to_string(),
                question: "还记得「团子」吗？".to_string(),
                reply: reply.to_string(),
                metrics: ProbeMetrics {
                    reply_chars: reply.chars().count(),
                    elapsed_ms: 1,
                },
                error: None,
            }],
            failed_count: 0,
        }
    }

    /// 空 rounds → 无聚合记录。
    #[tokio::test]
    async fn aggregate_round_scores_empty_returns_none() {
        let agg = aggregate_round_dimension_scores(&[], &None, None, None).await;
        assert!(agg.is_empty());
    }

    /// 三轮回复与 golden 完全一致 → fact 轮均分恒 1.0，n=3、std=0、CI 退化。
    #[tokio::test]
    async fn aggregate_round_scores_pools_round_means() {
        let reference = "用户去年收养了一只猫，取名团子";
        let mut golden = std::collections::HashMap::new();
        golden.insert("fact-0001".to_string(), reference.to_string());
        let rounds: Vec<ProbeVariantResult> = vec![
            fact_round(reference),
            fact_round(reference),
            fact_round(reference),
        ];
        let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden)).await;
        assert_eq!(agg.len(), 1, "只有 fact 维聚合");
        assert_eq!(agg[0].dimension, "fact");
        assert_eq!(agg[0].n, 3, "有效轮数 = 3");
        assert!((agg[0].mean - 1.0).abs() < 1e-9, "满分均值应为 1.0");
        assert_eq!(agg[0].std, 0.0);
        assert!((agg[0].ci95_low - 1.0).abs() < 1e-9);
        assert!((agg[0].ci95_high - 1.0).abs() < 1e-9);
    }

    /// 三轮回复质量不同 → 轮均分存在波动，mean 介于 (0,1)，std > 0，CI 有效。
    #[tokio::test]
    async fn aggregate_round_scores_captures_variation() {
        let reference = "用户去年收养了一只猫，取名团子";
        let mut golden = std::collections::HashMap::new();
        golden.insert("fact-0001".to_string(), reference.to_string());
        let rounds: Vec<ProbeVariantResult> = vec![
            fact_round(reference),
            fact_round("不太记得了"),
            fact_round(reference),
        ];
        let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden)).await;
        assert_eq!(agg[0].n, 3);
        assert!(
            agg[0].mean > 0.0 && agg[0].mean < 1.0,
            "波动后均值应介于 0..1"
        );
        assert!(agg[0].std > 0.0, "质量波动应产生正 std");
        assert!(agg[0].ci95_low < agg[0].mean && agg[0].mean < agg[0].ci95_high);
    }

    /// 旧评分数值文件（无 dimension_scores/emotion 字段）反序列化兼容。
    #[test]
    fn evaluation_variant_serde_backcompat_new_fields() {
        let old = r#"{
            "variant_id":"v1",
            "description":"d",
            "params":{"theta_gap_minutes":10,"max_msgs_per_block":80,"retrieve_top_k":3},
            "fact_score":0.5,"tone_score":null,"failed_count":0,"items":[]
        }"#;
        let parsed: VariantEvaluation = serde_json::from_str(old).unwrap();
        assert!(parsed.dimension_scores.is_none());
        assert!(parsed.emotion_score.is_none());
        // 空聚合序列化时省略 dimension_scores（保持最小差异）
        let s = serde_json::to_string(&parsed).unwrap();
        assert!(!s.contains("dimension_scores"), "None 聚合应省略: {s}");
    }

    // =========================================================
    // M5a 消融对比报告统计（T-V17-5a-004）
    // =========================================================

    /// erf / 正态 CDF 关键值：cdf(0)=0.5，cdf(1.96)≈0.975。
    #[test]
    fn normal_cdf_key_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-9);
        assert!((normal_cdf(1.96) - 0.975).abs() < 0.005);
        assert!((normal_cdf(-1.96) - 0.025).abs() < 0.005);
        assert!((erf_approx(0.0)).abs() < 1e-9);
    }

    /// Wilcoxon：单向强效应 → p 小；符号混合 → p 大（接近 1 侧）。
    #[test]
    fn wilcoxon_signed_rank_directionality() {
        // 8 个全正差分（不同绝对值避免全结）→ 秩和显著偏离零
        let diffs: Vec<f64> = (1..=8).map(|i| i as f64 * 0.1).collect();
        let p_strong = wilcoxon_signed_rank_p(&diffs).expect("n≥5 应可检验");
        assert!(p_strong < 0.05, "单向效应 p 应小，实际 {p_strong}");
        // 正负各半抵消 → p 大
        let mixed = vec![0.2, -0.3, 0.4, -0.5, 0.6, -0.7];
        let p_mixed = wilcoxon_signed_rank_p(&mixed).expect("n≥5 应可检验");
        assert!(p_mixed > 0.1, "符号混合 p 应大，实际 {p_mixed}");
        // 样本过小（n<5）→ None
        assert!(wilcoxon_signed_rank_p(&[0.1, 0.2, 0.3]).is_none());
    }

    /// Cohen's d：零方差非零均值 → ±10 标记；零均值 → 0。
    #[test]
    fn cohens_d_edge_cases() {
        assert_eq!(cohens_d_paired(&[1.0, 1.0, 1.0, 1.0]), 10.0);
        assert_eq!(cohens_d_paired(&[-0.5, -0.5]), -10.0);
        assert_eq!(cohens_d_paired(&[1.0, -1.0]), 0.0);
        assert!((cohens_d_paired(&[1.0, 2.0]) - 2.121).abs() < 0.01);
        assert_eq!(cohens_d_paired(&[]), 0.0);
    }

    /// BH FDR：单调校正且首尾正确。
    #[test]
    fn bh_fdr_adjust_monotonic() {
        let p = vec![0.01, 0.04, 0.2];
        let q = bh_fdr_adjust(&p);
        // 预期: [0.03, 0.06, 0.2]
        assert!((q[0] - 0.03).abs() < 1e-12);
        assert!((q[1] - 0.06).abs() < 1e-12);
        assert!((q[2] - 0.2).abs() < 1e-12);
        // 空输入
        assert!(bh_fdr_adjust(&[]).is_empty());
    }

    /// 构造一个合成评分数值档位（纯 fact 维度，给定逐题分数）。
    fn eval_variant_scores(id: &str, scores: &[f64]) -> VariantEvaluation {
        let items = scores
            .iter()
            .enumerate()
            .map(|(i, s)| ItemEvaluation {
                item_id: format!("fact-{:04}", i + 1),
                dimension: "fact".to_string(),
                question: String::new(),
                reference: None,
                reply_preview: String::new(),
                fact: Some(FactItemScore {
                    cosine: Some(*s),
                    keyword_hit: *s,
                    score: *s,
                }),
                tone: None,
                emotion: None,
                error: None,
            })
            .collect();
        VariantEvaluation {
            variant_id: id.to_string(),
            description: format!("{id} 档位"),
            params: VariantParams {
                theta_gap_minutes: 10,
                max_msgs_per_block: 80,
                retrieve_top_k: 3,
                ablation: Some(id.to_string()),
            },
            fact_score: None,
            tone_score: None,
            emotion_score: None,
            dimension_scores: None,
            failed_count: 0,
            items,
        }
    }

    /// 集成：F0（高分）vs F1（同题低分）→ F1/fact 行显著且方向 down。
    #[test]
    fn build_ablation_report_marks_removal_effect() {
        let eval = ProbeEvaluation {
            results_file: String::new(),
            persona_uid: "char-0001".into(),
            dataset_seed: 1,
            judge_used: false,
            embedding_used: false,
            generated_at: "t".into(),
            variants: vec![
                eval_variant_scores("F0", &[0.9, 0.9, 0.9, 0.9, 0.9]),
                eval_variant_scores("F1", &[0.5, 0.5, 0.5, 0.5, 0.5]),
            ],
        };
        let exp = ProbeExperiment {
            dataset_file: String::new(),
            dataset_seed: 1,
            persona_uid: "char-0001".into(),
            rebuild_utt: false,
            variants: vec![],
            repeat: None,
            generated_at: "t".into(),
        };
        let report = build_ablation_report(&exp, &eval);
        assert_eq!(report.baseline_variant, "F0");
        let row = report
            .rows
            .iter()
            .find(|r| r.ablation_variant == "F1" && r.dimension == "fact")
            .expect("应有 F1/fact 行");
        assert_eq!(row.n_pairs, 5);
        assert!(row.significant, "F1 移除行为层后应显著下降");
        assert_eq!(row.direction, "down");
        assert!(row.mean_diff < 0.0);
        assert!(row.p_fdr < 0.05);
        assert!(row.ci95_high < 0.0, "CI 不含 0");
    }

    /// S 组：B1（低分基座）vs S_behavior（高分单层）→ up 方向。
    #[test]
    fn build_ablation_report_s_group_positive() {
        let eval = ProbeEvaluation {
            results_file: String::new(),
            persona_uid: "char-0001".into(),
            dataset_seed: 1,
            judge_used: false,
            embedding_used: false,
            generated_at: "t".into(),
            variants: vec![
                eval_variant_scores("B1", &[0.4, 0.4, 0.4, 0.4, 0.4]),
                eval_variant_scores("S_behavior", &[0.8, 0.8, 0.8, 0.8, 0.8]),
            ],
        };
        let exp = ProbeExperiment {
            dataset_file: String::new(),
            dataset_seed: 1,
            persona_uid: "char-0001".into(),
            rebuild_utt: false,
            variants: vec![],
            repeat: None,
            generated_at: "t".into(),
        };
        let report = build_ablation_report(&exp, &eval);
        assert_eq!(report.baseline_variant, "B1");
        let row = report
            .rows
            .iter()
            .find(|r| r.ablation_variant == "S_behavior" && r.dimension == "fact")
            .expect("应有 S_behavior/fact 行");
        assert!(row.significant, "S_behavior 单层注入应显著正向");
        assert_eq!(row.direction, "up");
        assert!(row.mean_diff > 0.0);
    }
}
