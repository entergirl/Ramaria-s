//! crates/ramaria-cli/src/commands/probe/evaluate/pipeline.rs - 探针 评分流水线
//!
//! 设计特点:
//! - read_experiment / filter_variant_results 与 golden reference 加载
//! - 逐题评分编排与跨轮维度聚合接线
//! - 评分数值写出与文本摘要

use super::super::now_iso8601;
use super::super::types::ProbeDataset;
use super::super::types::ProbeExperiment;
use super::super::types::ProbeRunItem;
use super::super::types::ProbeVariantResult;
use super::model::{ItemEvaluation, ProbeEvaluation, VariantEvaluation};
use super::scoring::{
    aggregate_round_dimension_scores, is_local_backend, score_emotion_item, score_fact_item,
    score_tone_item,
};
use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::traits::LlmProvider;
use ramaria_service::Engine;
use std::path::Path;
use std::sync::Arc;

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
pub(crate) async fn run_evaluate(
    engine: &Arc<Engine>,
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
    let embedder = engine.embedding();
    let embedding_used = embedder.as_ref().map(|e| e.is_available()).unwrap_or(false);
    if !embedding_used {
        tracing::warn!("embedding 不可用，事实维退化为纯关键词评分");
    }

    let judge: Option<Arc<dyn LlmProvider>> = if no_tone_judge {
        tracing::info!("--no-tone-judge：跳过语气维 LLM-as-judge");
        None
    } else {
        let llm = engine.llm();
        // 语气维 judge 仅限本地后端（隐私口径）：本地 LM Studio / Ollama
        // 可直接用作 judge；线上后端（DeepSeek/OpenAI）自动跳过并标注。
        // 本地判据为"provider 非线上 ∧ base_url 指向本机"——兼容 LM Studio（:1234）
        // 与本地 Ollama（:11434），不依赖 provider 名字符串（名字形态与 as_str() 返回值
        // 不一致，字符串比对会误判为不匹配、致本地后端永不启用）。
        let cfg = llm.config();
        if is_local_backend(cfg.provider, &cfg.base_url) {
            Some(llm)
        } else {
            tracing::warn!(
                provider = %cfg.provider.as_str(),
                "语气维 judge 仅支持本地 LM Studio / Ollama（线上后端自动跳过并标注）"
            );
            None
        }
    };
    let judge_used = judge.is_some();

    // judge 请求间最小间隔（毫秒）：复用 [thresholds].cluster_delay_ms（语义
    // "批量 LLM 请求间最小间隔"，默认 800；本地 judge 批量较大时可调大至 ~1500）。
    // 仅当 judge 可用时生效——纯事实维 evaluate（--no-tone-judge / 线上后端跳过）
    // 不等待，避免无谓拖慢；delay=0 时 `llm_gate::inter_llm_delay` 内部直接跳过。
    let judge_delay_ms = engine.config().thresholds.cluster_delay_ms;

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
        let mut fact_scores_norm: Vec<f64> = Vec::new();
        let mut fact_scores_point: Vec<f64> = Vec::new();
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
                Some(f) if item_eval.error.is_none() => {
                    fact_scores.push(f.score);
                    if let Some(s) = f.score_norm {
                        fact_scores_norm.push(s);
                    }
                    if let Some(s) = f.score_point {
                        fact_scores_point.push(s);
                    }
                }
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

            // LLM-as-judge 请求间节流（本地 LM Studio / Ollama 需留间隔避免过载）：
            // 在每次 judge 请求（evaluate_item 内部 tone 评分）完成后等待；
            // judge 不可用（--no-tone-judge / 线上后端）时不等待。
            if judge.is_some() {
                ramaria_memory::llm_gate::inter_llm_delay(
                    judge_delay_ms,
                    "probe evaluate judge 请求间隔",
                )
                .await;
            }
        }

        let fact_score = if fact_scores.is_empty() {
            None
        } else {
            Some(fact_scores.iter().sum::<f64>() / fact_scores.len() as f64)
        };
        let fact_score_norm = if fact_scores_norm.is_empty() {
            None
        } else {
            Some(fact_scores_norm.iter().sum::<f64>() / fact_scores_norm.len() as f64)
        };
        let fact_score_point = if fact_scores_point.is_empty() {
            None
        } else {
            Some(fact_scores_point.iter().sum::<f64>() / fact_scores_point.len() as f64)
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
            fact_score_norm,
            fact_score_point,
            tone_score,
            emotion_score,
            failed,
            items = items.len(),
            "probe evaluate 档位完成"
        );

        // ---- 统计法（--repeat N）逐轮评分聚合 ----
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
                            judge_delay_ms,
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
            fact_score_norm,
            fact_score_point,
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
        // `-` + --json：stdout 只出一行信封，原始评分放 data.raw（避免两段 JSON）
        if out == "-" && json {
            let data = serde_json::json!({
                "file": "-",
                "persona_uid": evaluation.persona_uid,
                "variants": evaluation.variants.len(),
                "judge_used": evaluation.judge_used,
                "embedding_used": evaluation.embedding_used,
                "raw": &evaluation,
            });
            return crate::json::emit_ok(&data);
        }
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
pub(crate) fn read_experiment(path: &Path) -> anyhow::Result<ProbeExperiment> {
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
pub(crate) fn filter_variant_results(
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
/// - 收集 fact 维度的 reference（事件摘要）作为事实维精确评分的 golden 参照；
///   同时收集 tone 维度的 reference（persona 原回复）作为语气维 judge 的"参考回复"
///   （评分要求：候选回复与 persona 原回复在语气/风格上的相似度，而非与提问文本）。
/// - reference 缺失的条目忽略（fact 题后续退化为问题文本近似；tone 题则标注缺参考）。
pub(crate) fn load_golden_references(
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
        if matches!(item.dimension.as_str(), "fact" | "tone")
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
pub(crate) async fn evaluate_item(
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
            // 语气维 judge 参考回复 = persona 原回复（数据集 tone 题 reference），
            // 而非提问文本（run.question）——评分比较对象是"候选回复 vs 原回复"。
            let reference = golden.and_then(|g| g.get(&run.item_id).cloned());
            let tone = match (judge, &reference) {
                (Some(j), Some(rev)) => match score_tone_item(j, run, rev).await {
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
                // judge 不可用或数据集未提供该题 reference → 无参考可比（不评，非错误）
                (Some(_), None) => {
                    tracing::debug!(
                        item_id = %run.item_id,
                        "probe evaluate tone 题缺 persona 原回复参考（需 --dataset），tone 分缺失"
                    );
                    None
                }
                (None, _) => None,
            };
            ItemEvaluation {
                item_id: run.item_id.clone(),
                dimension: run.dimension.clone(),
                question: run.question.clone(),
                reference,
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

/// 写评分数值到文件（`-` 表示 stdout）。
///
/// 说明: `-` 直出 stdout（含库内原文，口径见模块头说明）。
pub(crate) fn write_evaluation_file(out: &str, evaluation: &ProbeEvaluation) -> anyhow::Result<()> {
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
pub(crate) fn print_evaluation_summary(evaluation: &ProbeEvaluation) {
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
        // 统计法（--repeat N）逐轮评分聚合：展示各维 mean ± 95% CI（n=轮数）。
        // 上表各行仍是最后一轮快照；这里给出跨轮聚合（可复算）。
        if let Some(scores) = &v.dimension_scores {
            for d in scores {
                println!(
                    "    ↳ {} 聚合: mean={:.3} ±95%CI [{:.3}, {:.3}] (std={:.3}, n={})",
                    d.dimension, d.mean, d.ci95_low, d.ci95_high, d.std, d.n
                );
            }
        }
    }
    if !evaluation.judge_used {
        crate::ui::info("语气维 judge 不可用或已跳过（tone 分为空），可运行 --json 查看标注");
    }
    crate::ui::info(
        "运行 `ramaria probe report --results <文件> --evaluation <评分文件>` 生成报告",
    );
}
