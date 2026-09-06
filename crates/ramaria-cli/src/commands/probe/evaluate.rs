//! crates/ramaria-cli/src/commands/probe/evaluate.rs - 探针 evaluate 自动评分
//!
//! 设计特点:
//! - `run_evaluate`：读取 probe run 实验结果，按档位逐题评分并输出评分数值文件 / JSON / 文本摘要。
//! - 事实维：embedding 余弦 + 关键词 2-gram 命中加权；embedding 不可用退化为纯关键词。
//! - 语气维：LLM-as-judge（rubric 1~5、温度 0、few-shot 锚定）；仅本地后端，线上自动跳过。
//! - 情感维：确定性 rubric（0/0.5/1 回应恰当性），安慰 / 喜悦标记词表驱动，零 LLM 依赖。
//! - 统计法（--repeat N）：逐轮评分按"轮均分"跨 N 轮聚合 mean ± 95% CI（复用 run::metric_stat）。
//! - 单题失败不中断批量；judge / embedding 缺失静默降级并标注；输出不含完整原文。

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::{ChatRequest, EmbeddingProvider, LlmProvider};
use uuid::Uuid;

use super::dataset::{has_negative_cue, has_positive_cue};
use super::run::metric_stat;
use super::types::{
    MetricStat, ProbeDataset, ProbeExperiment, ProbeRunItem, ProbeVariantResult, VariantParams,
};

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
pub(super) struct ProbeEvaluation {
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
pub(super) struct VariantEvaluation {
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
pub(super) struct DimensionScoreAgg {
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
pub(super) struct ItemEvaluation {
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
pub(super) struct FactItemScore {
    /// 余弦相似度（-1.0~1.0；embedding 不可用为 None）
    pub cosine: Option<f64>,
    /// 关键词命中率（0.0~1.0，参考文本 token 在回复中出现的比例）
    pub keyword_hit: f64,
    /// 综合分（0.0~1.0）
    pub score: f64,
}

/// 语气维单题评分（LLM-as-judge）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct ToneItemScore {
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
pub(super) struct EmotionItemScore {
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
pub(super) async fn run_evaluate(
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
        generated_at: super::now_iso8601(),
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
pub(super) fn read_experiment(path: &Path) -> anyhow::Result<ProbeExperiment> {
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
pub(super) fn load_golden_references(
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
pub(super) async fn aggregate_round_dimension_scores(
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
pub(super) fn score_emotion_item(reply: &str, question: &str) -> EmotionItemScore {
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
