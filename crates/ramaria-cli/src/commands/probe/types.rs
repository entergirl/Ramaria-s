//! crates/ramaria-cli/src/commands/probe/types.rs - 探针 CLI 常量与纯数据类型
//!
//! 设计特点:
//! - 收敛探针（probe build / run / evaluate / report）共享的常量与数据集/结果类型。
//! - 类型均可 serde 序列化/反序列化，供 `--json` 输出、落盘与跨命令读取。
//! - `AblationProfile` 把消融档位映射为注入层闸门覆盖集，`None`（无消融）行为等同 F0。
//! - `ProbeCmd` 集中承载四个子命令的参数形态（由 main.rs 解析 clap 后构造）。
//! - 本文件只含常量与类型定义（含纯类型构造工具），不含 I/O / 数据库 / LLM 业务逻辑。

use std::path::PathBuf;

// =========================================================
// 常量与档位定义
// =========================================================

/// 数据集 schema 版本（结构变更时递增，run 侧校验兼容性）。
pub(super) const DATASET_SCHEMA_VERSION: u32 = 1;

/// 默认探针 seed（固定值，保证 `probe build` 默认输出可复跑）。
pub const DEFAULT_SEED: u64 = 2026_0810;

/// 默认每维题数（3 维 × 10 题预跑）。
pub const DEFAULT_QUESTIONS_PER_DIM: usize = 10;

/// 默认目标 persona（无白名单 persona 时的兜底；fixture 数据即以此 persona 编写）。
pub(super) const DEFAULT_PERSONA: &str = "char-0001";

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

// =========================================================
// 消融对照语义（真增量档设计，M0 冻结）
// =========================================================
// 消融档位回答三类不同的问题，必须区分口径:
//
// 1) 替代对照（既有 `S_*`，保留）:
//    - 定义: B1 基座中"去掉 RAG 压缩摘要"，只保留目标专属层单独注入。
//    - 闸门: memory_rag=false，仅目标专属层闸门为 true。
//    - 回答问题: "目标层能否独立替代 RAG 摘要基座"，测的是单层替代能力。
//    - 局限: J 消融证明该口径测不出"在 RAG 之上叠加一层的净增量"。
//
// 2) 净增量对照（`I_behavior` / `I_knowledge` / `I_expression` / `I_narrative`，
//    后续评估里程碑实装，属未来档位，不在当前 Profile 集合内）:
//    - 定义: B1 压缩摘要基座 + 仅叠加一个目标专属层，其余专属层全部关闭。
//    - 闸门: memory_rag=true（保留 B1 基座），仅目标专属层闸门为 true，
//      其余专属层（behavior/knowledge/speaking_style+examples+utt/narrative+bridge）为 false。
//    - 回答问题: "在 RAG 摘要基座之上加一层的净增量"，与 B1 配对做统计检验。
//
// 3) 移除对照（`F1`~`F4`）与基线:
//    - F1~F4: 全开（F0）基础上关闭对应专属层 → 回答"去掉某一层的边际损失"。
//    - B0/B1/F0 为两个基线与完整体系锚点；F0 与 ablation=None 完全一致。
//
// 专属层 → 闸门集合映射（行为/knowledge/speaking_style/examples/utt/narrative/bridge）:
// - 行为层: behavior
// - 知识层: knowledge
// - 表达层: speaking_style + examples + utt
// - 脉络层: narrative + bridge
// - RAG 摘要基座: memory_rag
//
// 关系表与统计检验要点见 `docs/dev-2.0/ablation-profile-mapping.md`。

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
