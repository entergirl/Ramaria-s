//! crates/ramaria-cli/src/commands/probe/evaluate/model.rs - 探针 评分数据模型与判据常量
//!
//! 设计特点:
//! - 评分结果容器（档位 / 题项 / 三维明细）
//! - 语气 rubric、few-shot 锚定与事实维权重/字符表
//! - 情感维标记词表

use super::super::types::MetricStat;
use super::super::types::VariantParams;

/// 情感维 rubric 的"安慰/共情"标记（回复侧：对负面情境的恰当回应）。
pub(crate) const EMOTION_COMFORT_MARKERS: [&str; 18] = [
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
pub(crate) const EMOTION_JOY_MARKERS: [&str; 14] = [
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
pub(crate) struct ProbeEvaluation {
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
pub(crate) struct VariantEvaluation {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    /// 事实维均分（0.0~1.0；无 fact 题或全失败为 None）
    pub fact_score: Option<f64>,
    /// 事实维长度归一均分（0.0~1.0；旧产物无此字段 → None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact_score_norm: Option<f64>,
    /// 事实维事实点均分（0.0~1.0；旧产物无此字段 → None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact_score_point: Option<f64>,
    /// 语气维均分（1.0~5.0；judge 不可用或全失败为 None）
    pub tone_score: Option<f64>,
    /// 情感表达维均分（0.0~1.0 rubric；无 emotion 题或全失败为 None）。
    /// `#[serde(default)]`：旧评分数值文件无此字段时反序列化回退 None。
    #[serde(default)]
    pub emotion_score: Option<f64>,
    /// 统计法（`--repeat N`）逐轮评分聚合。
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
pub(crate) struct DimensionScoreAgg {
    /// 维度名（fact / fact_norm / fact_point / tone / emotion）
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
    pub(crate) fn from_metric(dimension: &str, stat: &MetricStat) -> Self {
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
pub(crate) struct ItemEvaluation {
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

/// 事实维单题评分（embedding 余弦 + 多判据关键词加权）。
///
/// 判据:
/// - `cosine`: 回复与参考的语义相似度（embedding 不可用为 None）。
/// - `keyword_hit`: 旧 2-gram 覆盖率，分母为**参考** 2-gram 总数——不随回复长度变化，
///   短回复的分子天然偏小，会被机械压低；保留该口径仅用于双口径对照。
/// - `keyword_hit_norm`: 长度归一命中率，分母为 `min(参考, 回复)` 内容 2-gram 数，
///   不再随回复长度单调衰减。
/// - `fact_point`: 参考子句中被回复命中至少一个内容 2-gram 的比例，衡量"回复体现了
///   至少一个事实点"，与整段覆盖率互补（长摘要参考下短社交回复通常只覆盖部分事实点）。
///
/// 综合分:
/// - `score`: 旧口径综合分（`0.6×cosine + 0.4×keyword_hit`），冻结不变以支持口径对照。
/// - `score_norm` / `score_point`: 同权重，仅把关键词项替换为 `keyword_hit_norm` /
///   `fact_point`；cosine 不可用时与旧口径一致，降级为纯关键词项。
///
/// 字段约定:
/// - 四个新增字段均带 `#[serde(default)]`：旧评分数值文件无这些字段 → None。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct FactItemScore {
    /// 余弦相似度（-1.0~1.0；embedding 不可用为 None）
    pub cosine: Option<f64>,
    /// 旧口径关键词命中率（0.0~1.0，参考 2-gram 在回复中出现的比例）
    pub keyword_hit: f64,
    /// 旧口径综合分（0.0~1.0）
    pub score: f64,
    /// 长度归一关键词命中率（0.0~1.0；旧产物无此字段 → None）
    #[serde(default)]
    pub keyword_hit_norm: Option<f64>,
    /// 事实点召回（0.0~1.0；旧产物无此字段 → None）
    #[serde(default)]
    pub fact_point: Option<f64>,
    /// 长度归一综合分（0.6×cosine + 0.4×keyword_hit_norm；cosine 不可用时降级为纯关键词）
    #[serde(default)]
    pub score_norm: Option<f64>,
    /// 事实点综合分（0.6×cosine + 0.4×fact_point；cosine 不可用时降级为纯关键词）
    #[serde(default)]
    pub score_point: Option<f64>,
}

/// 语气维单题评分（LLM-as-judge）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ToneItemScore {
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
pub(crate) struct EmotionItemScore {
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
///
/// 长度中性约束:
/// - 高情感社交语料的 persona 原回复普遍很短（实测均值约 15 字），若 rubric 与示例锚点
///   均为多句书面回复，judge 易形成"回复越长分越高"的长度偏置：实测各档位内
///   Pearson(reply_len, tone) ≈ 0.58~0.78，且 |reply_len - ref_len| 与得分**正**相关
///   （0.53）——即回复越偏离参考长度反而分越高，惩罚与参考同样简短的回复。
/// - 该偏置会把"表达层把回复压到 persona 真实短句长度"误判为语气变差，故在 rubric 中
///   显式加入长度中性指令。
pub(crate) const TONE_RUBRIC: &str = "\
请按 1~5 分评估「候选回复」在语气、风格上与「参考回复」的相似程度（只比语气风格，不比内容多少、不比长短）：
1 分：语气/风格完全不像参考（生硬、机器人口吻、书面助手腔）；
2 分：略有相似但有明显偏差；
3 分：基本相似，偶有偏差；
4 分：语气/风格较贴近参考，偏差少；
5 分：语气/风格高度贴近参考，几乎难辨。
重要：不要按回复长短判分。日常社交聊天里回复常常很短，参考回复很短时，
同样简短的候选回复完全可能是 5 分；写得冗长、像在解释或总结的回复反而应扣分。
只输出一个整数分数（1~5），不要输出任何其他文字。";

/// 语气维 judge 的示例锚定（few-shot，帮助 judge 稳定判分）。
///
/// 示例取自"短句社交聊天"分布（与高情感语料同域）：参考与候选长度相当，分数的差异
/// 只由语气风格决定；避免示例本身把"长=好"当作锚点（多句书面参考、候选短的给 1~2 分，
/// 是长度偏置的来源之一）。
pub(crate) const TONE_ANCHOR_EXAMPLES: &str = "\
【示例 1】（参考很短、候选同样简短且风格一致 → 高分）
参考回复：对啊对啊
候选回复：对对对
分数：5
【示例 2】（候选写成书面助手腔、比参考啰嗦很多 → 低分）
参考回复：我找一下
候选回复：好的，我这就去数据库里帮您查询相关记录，还请您稍等片刻。
分数：1
【示例 3】（候选长度接近但语气仍有偏差 → 中间分）
参考回复：哦哦
候选回复：原来是这样啊，我明白了。
分数：3
【示例 4】（候选与参考长度不同但语气风格贴近 → 高分）
参考回复：快九点吧
候选回复：九点左右到
分数：4";

/// 事实维综合分权重（cosine 0.6 / keyword 0.4）。
pub(crate) const FACT_COSINE_WEIGHT: f64 = 0.6;

pub(crate) const FACT_KEYWORD_WEIGHT: f64 = 0.4;

/// 事实维 cosine 未用时的纯关键词权重（embedding 不可用降级）。
pub(crate) const FACT_KEYWORD_ONLY_WEIGHT: f64 = 1.0;

/// 中文标点集合（内容 2-gram 过滤与子句切分用）。
pub(crate) const FACT_PUNCT_CHARS: &[char] = &[
    '，', '。', '；', '、', '！', '？', '…', '—', '～', '·', '「', '」', '『', '』', '（', '）',
    '(', ')', '《', '》', '〈', '〉', '“', '”', '"', '\'', '‘', '’', '：', ':', ',', '.', '!', '?',
    ';', '\n', '\r', '\t', ' ',
];

/// 高频功能字（内容 2-gram 过滤：两侧皆为功能字的 2-gram 不计入内容）。
pub(crate) const FACT_STOP_CHARS: &[char] = &[
    '的', '了', '是', '在', '有', '和', '与', '我', '你', '他', '她', '它', '们', '这', '那', '个',
    '就', '都', '也', '还', '很', '太', '不', '没', '中', '上', '下', '之', '而', '及', '等', '把',
    '被', '让', '给', '对', '从', '到', '为', '以', '于', '会', '能', '要', '说', '法', '做', '去',
    '来', '后', '前',
];
