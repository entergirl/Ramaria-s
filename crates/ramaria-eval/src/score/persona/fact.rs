//! crates/ramaria-eval/src/score/persona/fact.rs - Ramaria 评测人格表达事实维
//!
//! 设计特点:
//! - 综合分只认旧 2-gram 覆盖口径：0.6×语义余弦 + 0.4×关键词命中，历史结论可比
//! - 内容 2-gram 先剔除含标点与两侧皆功能字的组合，避免虚词撑起覆盖率
//! - 长度归一命中率与子句级事实点作为并列判据一并记录，不参与综合分
//! - 嵌入缺失或调用失败即降级为纯关键词，实际口径写进结果供条件级汇总

use ramaria_core::traits::EmbeddingProvider;
use serde::{Deserialize, Serialize};

/// 事实维综合分的余弦权重。
pub const FACT_COSINE_WEIGHT: f64 = 0.6;

/// 事实维综合分的关键词权重。
pub const FACT_KEYWORD_WEIGHT: f64 = 0.4;

/// 无可用余弦时的纯关键词权重。
pub const FACT_KEYWORD_ONLY_WEIGHT: f64 = 1.0;

/// 中文与常用英文标点（内容 2-gram 过滤与子句切分用）。
pub const FACT_PUNCT_CHARS: &[char] = &[
    '，', '。', '；', '、', '！', '？', '…', '—', '～', '·', '「', '」', '『', '』', '（', '）',
    '(', ')', '《', '》', '〈', '〉', '“', '”', '"', '\'', '‘', '’', '：', ':', ',', '.', '!', '?',
    ';', '\n', '\r', '\t', ' ',
];

/// 高频功能字（两侧皆为功能字的 2-gram 不计入内容）。
pub const FACT_STOP_CHARS: &[char] = &[
    '的', '了', '是', '在', '有', '和', '与', '我', '你', '他', '她', '它', '们', '这', '那', '个',
    '就', '都', '也', '还', '很', '太', '不', '没', '中', '上', '下', '之', '而', '及', '等', '把',
    '被', '让', '给', '对', '从', '到', '为', '以', '于', '会', '能', '要', '说', '法', '做', '去',
    '来', '后', '前',
];

// =========================================================
// 单题结果
// =========================================================

/// 事实维单题评分。
///
/// 字段约定:
/// - `score`: 综合分（0.0~1.0），口径见 `variant`；
/// - `cosine`: 回复与参考的语义余弦（-1.0~1.0）；嵌入不可用为 `None`；
/// - `keyword_hit`: 旧 2-gram 覆盖率，分母为参考 2-gram 总数，短回复会被机械压低；
/// - `keyword_hit_norm`: 长度归一命中率，分母为 `min(参考, 回复)` 内容 2-gram 数；
/// - `fact_point`: 参考子句中被回复命中至少一个内容 2-gram 的子句占比；
/// - `variant`: 实际口径，`cosine+keyword` 或 `keyword`。
///
/// 说明: 三个关键词判据并列记录，只有 `keyword_hit` 进综合分。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactScore {
    pub score: f64,
    pub cosine: Option<f64>,
    pub keyword_hit: f64,
    pub keyword_hit_norm: f64,
    pub fact_point: f64,
    pub variant: &'static str,
}

/// 事实维结果的序列化形态（口径标签用字符串，读回时再收敛为静态常量）。
#[derive(Deserialize)]
struct FactScoreWire {
    score: f64,
    cosine: Option<f64>,
    keyword_hit: f64,
    keyword_hit_norm: f64,
    fact_point: f64,
    variant: String,
}

/// 读回时把口径标签收敛为静态常量，未知标签报错。
///
/// 说明: `variant` 是 `&'static str`，序列化与反序列化不同形，手写实现避免
/// derive 给整个结构体加上 `'de: 'static` 约束。
impl<'de> Deserialize<'de> for FactScore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = FactScoreWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<FactScoreWire> for FactScore {
    type Error = String;

    fn try_from(wire: FactScoreWire) -> Result<Self, Self::Error> {
        let variant = match wire.variant.as_str() {
            FACT_VARIANT_COSINE => FACT_VARIANT_COSINE,
            FACT_VARIANT_KEYWORD => FACT_VARIANT_KEYWORD,
            other => return Err(format!("未知的事实维口径: {other}")),
        };
        Ok(Self {
            score: wire.score,
            cosine: wire.cosine,
            keyword_hit: wire.keyword_hit,
            keyword_hit_norm: wire.keyword_hit_norm,
            fact_point: wire.fact_point,
            variant,
        })
    }
}

/// 有可用余弦时的口径标签。
const FACT_VARIANT_COSINE: &str = "cosine+keyword";

/// 纯关键词降级时的口径标签。
const FACT_VARIANT_KEYWORD: &str = "keyword";

// =========================================================
// 单题评分
// =========================================================

/// 事实维单题评分。
///
/// 参数:
/// - `reply`: 待评回复；
/// - `reference`: 参照文本（本题取 gold_answer）；
/// - `embedder`: 嵌入 provider；`None` 表示向量通道不可用。
///
/// 返回:
/// - 余弦可用时综合分为 `0.6×cos(≥0) + 0.4×keyword_hit`；
/// - 余弦不可用（未配置 / 任一文本向量化失败）时降级为 `keyword_hit`。
///
/// 说明: 余弦失败不阻塞评分，只记 warn 后按纯关键词口径出分。
pub async fn score_fact_item(
    reply: &str,
    reference: &str,
    embedder: Option<&dyn EmbeddingProvider>,
) -> FactScore {
    let keyword_hit = keyword_hit_score(reply, reference);
    let keyword_hit_norm = keyword_hit_norm_score(reply, reference);
    let fact_point = fact_point_score(reply, reference);

    let cosine = match embedder {
        Some(embedder) => {
            let value = embed_pair(embedder, reply, reference).await;
            if value.is_none() {
                tracing::warn!("embedding 余弦计算失败，事实维按纯关键词口径出分");
            }
            value
        }
        None => None,
    };

    let (score, variant) = match cosine {
        Some(cosine) => (
            FACT_COSINE_WEIGHT * cosine.max(0.0) + FACT_KEYWORD_WEIGHT * keyword_hit,
            FACT_VARIANT_COSINE,
        ),
        None => (FACT_KEYWORD_ONLY_WEIGHT * keyword_hit, FACT_VARIANT_KEYWORD),
    };

    FactScore {
        score: score.clamp(0.0, 1.0),
        cosine,
        keyword_hit,
        keyword_hit_norm,
        fact_point,
        variant,
    }
}

// =========================================================
// 关键词判据
// =========================================================

/// 回复对参考文本的旧口径 2-gram 覆盖率（0.0~1.0）。
///
/// 算法:
/// - 对 `reference` 提取全部相邻 2-gram 集合，统计其中出现在 `reply` 的比例；
/// - 分母固定为参考 2-gram 总数，回复越短分子越小，短回复会被机械压低；
/// - `reference` 不足 2 字时退化为按回复信息密度打分。
pub fn keyword_hit_score(reply: &str, reference: &str) -> f64 {
    let reply = reply.trim();
    if reply.is_empty() {
        return 0.0;
    }
    let ref_chars: Vec<char> = reference.trim().chars().collect();
    if ref_chars.len() < 2 {
        return density_score(reply);
    }

    let mut ref_bigrams: std::collections::HashSet<(char, char)> = std::collections::HashSet::new();
    for window in ref_chars.windows(2) {
        ref_bigrams.insert((window[0], window[1]));
    }
    if ref_bigrams.is_empty() {
        return 0.0;
    }

    let reply_chars: Vec<char> = reply.chars().collect();
    let mut hit = 0usize;
    for window in reply_chars.windows(2) {
        if ref_bigrams.contains(&(window[0], window[1])) {
            hit += 1;
        }
    }
    (hit as f64 / ref_bigrams.len() as f64).clamp(0.0, 1.0)
}

/// 提取内容 2-gram：剔除含标点的组合与两侧皆为功能字的组合。
///
/// 说明: 只剔除两侧都是功能字的组合，单侧为功能字时仍保留（"团子是"的"子是"有意义）。
pub fn content_bigrams(s: &str) -> Vec<(char, char)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    for window in chars.windows(2) {
        if FACT_PUNCT_CHARS.contains(&window[0]) || FACT_PUNCT_CHARS.contains(&window[1]) {
            continue;
        }
        if FACT_STOP_CHARS.contains(&window[0]) && FACT_STOP_CHARS.contains(&window[1]) {
            continue;
        }
        out.push((window[0], window[1]));
    }
    out
}

/// 把参考切成事实子句：按标点切分，保留字数 ≥2 的片段。
pub fn reference_clauses(reference: &str) -> Vec<String> {
    reference
        .split(|c: char| FACT_PUNCT_CHARS.contains(&c))
        .map(|part| part.trim())
        .filter(|part| part.chars().count() >= 2)
        .map(|part| part.to_string())
        .collect()
}

/// 长度归一关键词命中率（0.0~1.0）。
///
/// 公式: `命中内容 2-gram 数 / min(参考内容 2-gram 数, 回复内容 2-gram 数)`。
///
/// 说明: 旧口径以参考 2-gram 总数为分母，分数随回复变短单调衰减；改用两侧较小值后
/// 不再惩罚简短回复。空回复、参考过短或任一侧无内容 2-gram 时为 0.0。
pub fn keyword_hit_norm_score(reply: &str, reference: &str) -> f64 {
    let reply = reply.trim();
    if reply.is_empty() {
        return 0.0;
    }
    let ref_bg = content_bigrams(reference);
    let reply_bg = content_bigrams(reply);
    if ref_bg.is_empty() || reply_bg.is_empty() {
        return 0.0;
    }
    let ref_set: std::collections::HashSet<(char, char)> = ref_bg.into_iter().collect();
    let hit = reply_bg
        .iter()
        .filter(|bigram| ref_set.contains(bigram))
        .count();
    (hit as f64 / ref_set.len().min(reply_bg.len()) as f64).clamp(0.0, 1.0)
}

/// 事实点召回（0.0~1.0）：参考子句中被回复命中至少一个内容 2-gram 的子句占比。
///
/// 说明: 长摘要参考下简短社交回复通常只覆盖部分子句，该分衡量"是否体现了事实点"，
/// 与整段覆盖率互补。
pub fn fact_point_score(reply: &str, reference: &str) -> f64 {
    let units = reference_clauses(reference);
    if units.is_empty() {
        return 0.0;
    }
    let reply_bg: std::collections::HashSet<(char, char)> =
        content_bigrams(reply).into_iter().collect();
    let hit = units
        .iter()
        .filter(|unit| {
            let unit_bg = content_bigrams(unit);
            !unit_bg.is_empty() && unit_bg.iter().any(|bigram| reply_bg.contains(bigram))
        })
        .count();
    hit as f64 / units.len() as f64
}

/// 回复信息密度打分（0.0~1.0）：参考不足 2 字时的近似。
///
/// 说明: 8 字以下 0.2，40 字以上 1.0，中间线性过渡。
pub fn density_score(reply: &str) -> f64 {
    let chars = reply.chars().count();
    if chars < 8 {
        return 0.2;
    }
    if chars >= 40 {
        return 1.0;
    }
    0.5 + 0.5 * (chars as f64 - 8.0) / 32.0
}

// =========================================================
// 嵌入
// =========================================================

/// 两段文本的嵌入余弦（任一失败或空向量返回 `None`）。
pub async fn embed_pair(embedder: &dyn EmbeddingProvider, left: &str, right: &str) -> Option<f64> {
    let left_vector = match embedder.embed(left).await {
        Ok(vector) if !vector.is_empty() => vector,
        Ok(_) => return None,
        Err(error) => {
            tracing::warn!(error = %error, "embedding 向量化失败（待评回复）");
            return None;
        }
    };
    let right_vector = match embedder.embed(right).await {
        Ok(vector) if !vector.is_empty() => vector,
        Ok(_) => return None,
        Err(error) => {
            tracing::warn!(error = %error, "embedding 向量化失败（参考文本）");
            return None;
        }
    };
    Some(cosine_f32(&left_vector, &right_vector))
}

/// 两个 f32 向量的余弦相似度（归一化内积）。
///
/// 说明: 长度不一致或任一范数为 0 时返回 0.0（无语义可比）。
pub fn cosine_f32(left: &[f32], right: &[f32]) -> f64 {
    if left.len() != right.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut left_norm = 0.0f64;
    let mut right_norm = 0.0f64;
    for (a, b) in left.iter().zip(right.iter()) {
        let (a, b) = (f64::from(*a), f64::from(*b));
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0)
}
