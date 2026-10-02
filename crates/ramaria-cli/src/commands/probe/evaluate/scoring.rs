//! crates/ramaria-cli/src/commands/probe/evaluate/scoring.rs - 探针 三维判据与统计聚合
//!
//! 设计特点:
//! - 事实维：embedding 余弦 + 关键词命中（2-gram / 长度归一 / 事实点）
//! - 语气维：LLM-as-judge 提示与分数解析；本地后端判定
//! - 情感维：确定性 rubric 与标记词命中
//! - 跨轮维度分聚合（均值 / 标准差 / 95% CI）

use super::super::dataset::has_negative_cue;
use super::super::dataset::has_positive_cue;
use super::super::run::metric_stat;
use super::super::types::ProbeRunItem;
use super::super::types::ProbeVariantResult;
use super::model::{
    DimensionScoreAgg, EMOTION_COMFORT_MARKERS, EMOTION_JOY_MARKERS, EmotionItemScore,
    FACT_COSINE_WEIGHT, FACT_KEYWORD_ONLY_WEIGHT, FACT_KEYWORD_WEIGHT, FACT_PUNCT_CHARS,
    FACT_STOP_CHARS, FactItemScore, TONE_ANCHOR_EXAMPLES, TONE_RUBRIC, ToneItemScore,
};
use super::pipeline::evaluate_item;
use ramaria_core::traits::ChatRequest;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::traits::LlmProvider;
use std::sync::Arc;
use uuid::Uuid;

/// 语气维 judge 的 system prompt（rubric + few-shot 示例）。
///
/// 说明: 统一出口便于单测锁定"长度中性"口径不回退。
pub(crate) fn tone_judge_system_prompt() -> String {
    format!("{TONE_RUBRIC}\n\n{TONE_ANCHOR_EXAMPLES}")
}

/// 判断后端配置是否可作语气维本地 judge。
///
/// 本地判据（隐私口径：仅本地 judge）:
/// - provider 非线上（LM Studio / 未来本地 Ollama 均为非线上）；
/// - base_url host 指向本机（localhost / 127.0.0.1 / ::1），兼容 LM Studio（:1234）
///   与本地 Ollama（:11434）的 OpenAI-compatible 服务。
///
/// 线上后端（DeepSeek/OpenAI）与远程 host 一律返回 false（自动跳过并标注）。
pub(crate) fn is_local_backend(provider: ramaria_core::types::LlmProvider, base_url: &str) -> bool {
    if provider.is_online() {
        return false;
    }
    let lower = base_url.to_ascii_lowercase();
    lower.contains("://localhost") || lower.contains("://127.0.0.1") || lower.contains("://[::1]")
}

// =========================================================
// 执行 `probe evaluate`
// =========================================================

/// 统计法逐轮评分聚合。
///
/// 对 `--repeat N` 保留的每一轮完整结果（`rounds`）分别评分，
/// 按"轮"为观测单位聚合：
/// - 每轮先对该轮全部题取各维度均分（与主流程单次快照口径一致）；
/// - 跨 N 轮对轮均分计算 mean / std / 95% CI（t 分布，复用 `metric_stat`），
///   `n` = 有该维评分的有效轮数。
///
/// 事实维按三口径分别聚合（`fact` = 旧 2-gram 覆盖口径，`fact_norm` = 长度归一口径，
/// `fact_point` = 子句级事实点口径），三者的轮均分各自独立跨轮统计，互不合并。
///
/// 返回按 fact → fact_norm → fact_point → tone → emotion 排序的聚合记录；
/// 无任何有效轮时返回空。
/// 单题失败跳过（该轮其余成功题仍计入），与主流程"单题失败不中断"一致。
///
/// 节流:
/// - `judge_delay_ms` 为 judge 请求间最小间隔（毫秒）；judge 可用（`Some`）时
///   在每次 `evaluate_item` 后等待该间隔，避免本地 judge 过载（同主流程口径）。
/// - `judge` 为 `None`（纯事实维 evaluate）时不等待。
pub(crate) async fn aggregate_round_dimension_scores(
    rounds: &[ProbeVariantResult],
    embedder: &Option<Arc<dyn EmbeddingProvider>>,
    judge: Option<&dyn LlmProvider>,
    golden: Option<&std::collections::HashMap<String, String>>,
    judge_delay_ms: u64,
) -> Vec<DimensionScoreAgg> {
    let mut fact_round_means: Vec<f64> = Vec::new();
    let mut fact_norm_round_means: Vec<f64> = Vec::new();
    let mut fact_point_round_means: Vec<f64> = Vec::new();
    let mut tone_round_means: Vec<f64> = Vec::new();
    let mut emotion_round_means: Vec<f64> = Vec::new();

    for round in rounds {
        let mut fact_scores: Vec<f64> = Vec::new();
        let mut fact_norm_scores: Vec<f64> = Vec::new();
        let mut fact_point_scores: Vec<f64> = Vec::new();
        let mut tone_scores: Vec<u32> = Vec::new();
        let mut emotion_scores: Vec<f64> = Vec::new();
        for run in &round.runs {
            let item_eval = evaluate_item(run, embedder, judge, golden).await;
            // LLM-as-judge 请求间节流（同主流程口径：judge 可用时每题后等待；
            // judge 为 None 时不等待；delay=0 时 inter_llm_delay 内部跳过）。
            if judge.is_some() {
                ramaria_memory::llm_gate::inter_llm_delay(
                    judge_delay_ms,
                    "probe evaluate judge 请求间隔",
                )
                .await;
            }
            if item_eval.error.is_some() {
                continue;
            }
            if let Some(f) = &item_eval.fact {
                fact_scores.push(f.score);
                if let Some(s) = f.score_norm {
                    fact_norm_scores.push(s);
                }
                if let Some(s) = f.score_point {
                    fact_point_scores.push(s);
                }
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
        if !fact_norm_scores.is_empty() {
            fact_norm_round_means
                .push(fact_norm_scores.iter().sum::<f64>() / fact_norm_scores.len() as f64);
        }
        if !fact_point_scores.is_empty() {
            fact_point_round_means
                .push(fact_point_scores.iter().sum::<f64>() / fact_point_scores.len() as f64);
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

    let mut out = Vec::with_capacity(5);
    if !fact_round_means.is_empty() {
        out.push(DimensionScoreAgg::from_metric(
            "fact",
            &metric_stat(&fact_round_means),
        ));
    }
    if !fact_norm_round_means.is_empty() {
        out.push(DimensionScoreAgg::from_metric(
            "fact_norm",
            &metric_stat(&fact_norm_round_means),
        ));
    }
    if !fact_point_round_means.is_empty() {
        out.push(DimensionScoreAgg::from_metric(
            "fact_point",
            &metric_stat(&fact_point_round_means),
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
// 事实维评分（golden：embedding 余弦 + 多判据关键词项）
// =========================================================

/// 事实维单题评分。
///
/// 评分公式（三套口径，权重相同，仅关键词项不同）:
/// - embedding 可用: `0.6 × cosine(reply, reference) + 0.4 × 关键词项`。
/// - embedding 不可用: `关键词项`（纯关键词降级，标注 embedding 未用）。
/// - 关键词项: 旧 2-gram 覆盖率（`score`）/ 长度归一命中率（`score_norm`）/
///   子句级事实点召回（`score_point`）。
///
/// 说明:
/// - `reference` 为事实维 golden 参考（事件摘要）；`reply` 为模型对探针问题的回复。
/// - 旧口径 `score` 冻结不变：既保证历史产物可比，也支持"旧 2-gram 覆盖 vs
///   长度中性口径"的双口径对照（短回复在旧口径下被机械压低）。
/// - cosine 为 reply 与 reference 的语义相似度（embedding 向量余弦）；不可用
///   （无 embedder 或调用失败）时三套口径一致降级为纯关键词项。
pub(crate) async fn score_fact_item(
    reply: &str,
    reference: &str,
    embedder: Option<&dyn EmbeddingProvider>,
) -> FactItemScore {
    // 关键词命中率：reference 的关键 2-gram 在 reply 中的覆盖比例（旧口径，分母固定）
    let keyword_hit = keyword_hit_score(reply, reference);
    // 长度中性口径：分母取 min(参考, 回复) 内容 2-gram 数
    let keyword_hit_norm = keyword_hit_norm_score(reply, reference);
    // 子句级口径：参考事实点被回复覆盖的比例
    let fact_point = fact_point_score(reply, reference);

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

    // 长度归一 / 事实点综合分：与旧综合分同权重，仅替换关键词项；
    // cosine 不可用时与旧口径一致，降级为纯关键词项。
    let score_norm = Some(
        match cosine {
            Some(c) => FACT_COSINE_WEIGHT * c.max(0.0) + FACT_KEYWORD_WEIGHT * keyword_hit_norm,
            None => FACT_KEYWORD_ONLY_WEIGHT * keyword_hit_norm,
        }
        .clamp(0.0, 1.0),
    );
    let score_point = Some(
        match cosine {
            Some(c) => FACT_COSINE_WEIGHT * c.max(0.0) + FACT_KEYWORD_WEIGHT * fact_point,
            None => FACT_KEYWORD_ONLY_WEIGHT * fact_point,
        }
        .clamp(0.0, 1.0),
    );

    FactItemScore {
        cosine,
        keyword_hit,
        score,
        keyword_hit_norm: Some(keyword_hit_norm),
        fact_point: Some(fact_point),
        score_norm,
        score_point,
    }
}

/// 计算回复对参考文本的关键词命中率（0.0~1.0）。
///
/// 算法:
/// - 对 `reference` 提取中文 2-gram（bigram）集合，统计其中出现在 `reply` 中的比例。
/// - 2-gram 字面重叠在中文短文本上能稳定反映"回复是否覆盖参考关键信息"。
/// - `reference` 过短（<2 字）时退化为按 reply 信息密度打分。
pub(crate) fn keyword_hit_score(reply: &str, reference: &str) -> f64 {
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

/// 提取"内容 2-gram"：剔除含标点的 2-gram，以及两侧皆为功能字的 2-gram。
pub(crate) fn content_bigrams(s: &str) -> Vec<(char, char)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    for w in chars.windows(2) {
        if FACT_PUNCT_CHARS.contains(&w[0]) || FACT_PUNCT_CHARS.contains(&w[1]) {
            continue;
        }
        if FACT_STOP_CHARS.contains(&w[0]) && FACT_STOP_CHARS.contains(&w[1]) {
            continue;
        }
        out.push((w[0], w[1]));
    }
    out
}

/// 参考切分为事实子句：按标点切分，保留字数 ≥2 的子句。
pub(crate) fn reference_clauses(reference: &str) -> Vec<String> {
    reference
        .split(|c: char| FACT_PUNCT_CHARS.contains(&c))
        .map(|p| p.trim())
        .filter(|p| p.chars().count() >= 2)
        .map(|p| p.to_string())
        .collect()
}

/// 长度归一关键词命中率（0.0~1.0）。
///
/// 公式: `命中内容 2-gram 数 / min(参考内容 2-gram 数, 回复内容 2-gram 数)`。
/// 旧口径以参考 2-gram 总数为分母，回复越短分子越小，短回复被机械压低；
/// 改用 `min(参考, 回复)` 后该分不再随回复长度单调衰减。
/// 空回复 / 参考过短 / 任一侧无内容 2-gram → 0.0。
pub(crate) fn keyword_hit_norm_score(reply: &str, reference: &str) -> f64 {
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
    let hit = reply_bg.iter().filter(|b| ref_set.contains(b)).count();
    (hit as f64 / ref_set.len().min(reply_bg.len()) as f64).clamp(0.0, 1.0)
}

/// 事实点召回（0.0~1.0）：参考子句中被回复至少一个内容 2-gram 命中的子句占比。
///
/// 参考为多事实长摘要，简短社交回复通常只体现其中部分事实点；
/// 该分衡量"回复是否体现了至少一个事实点"，与整段覆盖率互补。
pub(crate) fn fact_point_score(reply: &str, reference: &str) -> f64 {
    let units = reference_clauses(reference);
    if units.is_empty() {
        return 0.0;
    }
    let reply_bg: std::collections::HashSet<(char, char)> =
        content_bigrams(reply).into_iter().collect();
    let hit = units
        .iter()
        .filter(|u| {
            let ub = content_bigrams(u);
            !ub.is_empty() && ub.iter().any(|b| reply_bg.contains(b))
        })
        .count();
    hit as f64 / units.len() as f64
}

/// 回复信息密度打分（无参考可用时的近似，0.0~1.0）。
///
/// 说明: 回复越长、信息越充分，密度分越高；空/过短回复得分低。
pub(crate) fn density_score(reply: &str) -> f64 {
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
pub(crate) async fn embed_pair(embedder: &dyn EmbeddingProvider, a: &str, b: &str) -> Option<f64> {
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
pub(crate) fn cosine_f32(a: &[f32], b: &[f32]) -> f64 {
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
pub(crate) async fn score_tone_item(
    judge: &dyn LlmProvider,
    run: &ProbeRunItem,
    reference: &str,
) -> anyhow::Result<ToneItemScore> {
    let request = ChatRequest {
        system_prompt: tone_judge_system_prompt(),
        memory_context: None,
        history: vec![],
        user_message: format!(
            "参考回复：{}\n候选回复：{}",
            reference, // tone 题参考 = persona 原回复（数据集 reference）
            run.reply
        ),
        temperature: 0.0,
        max_tokens: 16,
        request_id: Uuid::new_v4(),
        // 模板版本参与 LLM 响应缓存键：口径修订即 bump，避免复用旧偏置评分缓存。
        template_version: "probe-judge-v2".to_string(),
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
pub(crate) fn parse_judge_score(raw: &str) -> Option<u32> {
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
pub(crate) fn score_emotion_item(reply: &str, question: &str) -> EmotionItemScore {
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
pub(crate) fn count_marker_hits(text: &str, markers: &[&str]) -> usize {
    markers.iter().filter(|w| text.contains(**w)).count()
}

// =========================================================
// 输出辅助（evaluate）
// =========================================================
