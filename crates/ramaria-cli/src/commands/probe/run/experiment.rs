//! crates/ramaria-cli/src/commands/probe/run/experiment.rs - 探针 档位实验装配与统计聚合
//!
//! 设计特点:
//! - 档位实验构建（基线 + 单参数档）与 repeat 多轮装配
//! - 跨轮聚合（均值 / 标准差 / 95% CI）与 t 临界值
//! - 档位过滤与结果写出 / 文本摘要

use super::super::now_iso8601;
use super::super::types::AblationProfile;
use super::super::types::DATASET_SCHEMA_VERSION;
use super::super::types::ItemRepeatStats;
use super::super::types::MetricStat;
use super::super::types::ProbeDataset;
use super::super::types::ProbeExperiment;
use super::super::types::ProbeRepeatMeta;
use super::super::types::ProbeVariant;
use super::super::types::ProbeVariantResult;
use super::super::types::VariantParams;
use super::super::types::VariantRepeatStats;
use super::session::{
    collect_run_diagnostics, ensure_privacy_with_engine, load_effective_config,
    rebuild_utt_for_config, run_single_question,
};
use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_service::Engine;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

/// 执行 `probe run`。
///
/// 流程:
/// 1. 读取数据集（probe build 产物），校验 schema 版本与维度。
/// 2. 加载生效配置（config.toml + DB 双写合并）作为档位基准。
/// 3. 逐档位：覆盖 utt 三参数 →（可选）按档位参数重建 utt 块 → 逐题跑对话管线。
/// 4. 单题/单档位失败均不中断其余（记 warn + 记录失败原因）。
// 参数为命令入口的完整输入集合（含输出模式与隐私透传），合并会降低可读性。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_experiment(
    engine: &Arc<Engine>,
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
        engine,
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
        // `-` + --json：stdout 只出一行信封，原始结果放 data.raw（避免两段 JSON）
        if out == "-" && json {
            let data = serde_json::json!({
                "file": "-",
                "persona_uid": experiment.persona_uid,
                "variants": experiment.variants.len(),
                "runs": run_count,
                "raw": &experiment,
            });
            return crate::json::emit_ok(&data);
        }
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

/// 构建档位实验结果（供命令输出与自动评分复用）。
///
/// 流程:
/// 1. 校验数据集（schema 版本不匹配记 warn 继续）。
/// 2. 加载生效配置（config.toml + DB 双写合并）作为档位基准。
/// 3. 隐私确认（线上 provider 需确认；本地 LM Studio 直接通过）。
/// 4. 逐档位：覆盖 utt 三参数 →（可选）按档位参数重建 utt 块 → 逐题跑对话管线。
/// 5. 单题/单档位失败均不中断其余（记 warn + 记录失败原因）。
pub async fn build_experiment(
    engine: &Arc<Engine>,
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

    // Step 2: 加载生效配置作为档位基准（失败降级为引擎默认配置）
    let base_config = load_effective_config(engine).await;

    // Step 3: 隐私确认（线上 provider 需确认；本地 LM Studio 直接通过）
    ensure_privacy_with_engine(engine, yes).await?;

    // Step 3.5: 检索器就绪保障。
    //
    // 引擎的检索索引槽为懒加载（首次召回前为空、不装载任何文档）；档位实验此前
    // 只依赖 `rebuild_utt_for_config` 内的重建调用，因此 `--no-rebuild-utt` 会连同
    // 检索器装载一并跳过 —— RAG 记忆与知识通道静默失效，fact 维指标失真
    // （表现为 B0/B1/F0 事实维趋同、回复答"没有相关记录"）。
    // 此处无条件先重建一次，保证任何档位组合都在"检索器已就绪"前提下运行。
    if let Err(e) = engine.rebuild_index().await {
        tracing::warn!(%e, "probe run 检索器装载失败，RAG 记忆可能缺失");
    }

    // Step 3.6: 实验有效性自检——检索器就绪度与各通道命中数写入元数据，
    // 跑数结束即可判定该轮是否有效（检索器空载时输出告警）。
    let diagnostics = collect_run_diagnostics(engine, dataset, &dataset.persona_uid).await;

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

        // 消融扩展：档位带 `ablation` 时，在 utt 覆盖后应用
        // 注入层闸门（B0/B1/F0/F1~F4/S_*）。`ablation=None`（旧数据集）
        // 时零覆盖——行为与未启用消融完全一致（兼容性要求）。
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

        // 档位非 utt 参数覆盖（rag/knowledge）：None 字段保持配置基准，
        // 使参数扫描可按档位只变更目标参数而不串扰其它路。
        if let Some(v) = variant.overrides.rag_max_memories {
            variant_config.retrieval.rag_max_memories = v;
        }
        if let Some(v) = variant.overrides.rag_max_summary_chars {
            variant_config.retrieval.rag_max_summary_chars = v;
        }
        if let Some(v) = variant.overrides.knowledge_retrieve_top_k {
            variant_config.knowledge.retrieve_top_k = v;
        }
        if let Some(v) = variant.overrides.knowledge_retrieve_threshold {
            variant_config.knowledge.retrieve_threshold = v;
        }

        tracing::info!(
            variant_id = %variant.id,
            theta_gap_minutes = variant.theta_gap_minutes,
            max_msgs_per_block = variant.max_msgs_per_block,
            retrieve_top_k = variant.retrieve_top_k,
            ablation = ?variant.ablation,
            overrides = ?variant.overrides,
            "probe run 档位开始"
        );

        // 档位对齐：仅当收到重建指令且该切分参数（θ_gap/条数）尚未在本轮重建过时，
        // 才重建 utt 块——同一切分下的多档位（仅 top_k 不同）复用已建块，embedding
        // 调用数不随 top_k 档位倍增；top_k 变化不影响 utt 块切分。
        // `rebuild_utt=false`（--no-rebuild-utt）时完全不重建，直接复用库中已建块。
        let cut_key = (variant.theta_gap_minutes, variant.max_msgs_per_block);
        if rebuild_utt
            && !rebuilt_cuts.contains_key(&cut_key)
            && let Err(e) = rebuild_utt_for_config(engine, &variant_config).await
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

        // 语域切换：在档位循环内克隆一次配置，题循环内按题覆盖社交基调闸门
        // （statement 题关闭基调走陈述说明语域，chat 题保持社交聊天语域）。
        let mut item_config = variant_config.clone();
        let mut runs = Vec::new();
        let mut failed = 0usize;
        let max_runs = limit
            .unwrap_or(dataset.items.len())
            .min(dataset.items.len());
        for item in dataset.items.iter().take(max_runs) {
            item_config.injection.social_tone = item.register.is_chat();
            let result =
                run_single_question(engine, &item_config, &dataset.persona_uid, item).await;
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
        diagnostics: Some(diagnostics),
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
///    （配对统计口径）。主 `variants` 仍保留最后一次运行明细，
///    供单次评定/兼容读取复用。
///
/// 说明:
/// - 统计法为档位对比共享工具链：档位对比以「多次均值 ± 置信区间」
///   为口径，不期待单次命令逐字复现。
#[allow(clippy::too_many_arguments)] // 参数与 build_experiment 一致（另加 repeat 聚合数）
pub async fn build_experiment_with_repeat(
    engine: &Arc<Engine>,
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
            engine,
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
                engine,
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
        diagnostics: last.diagnostics.clone(),
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
/// `rounds` 保留该档位**每一轮**的完整 `ProbeVariantResult`
/// （含逐轮全量 reply），供 evaluate/report 对每轮 reply 分别语义评分后聚合 fact_score
/// 的均值 ± 置信区间（配对统计口径）。
pub(crate) fn aggregate_repeat_stats(rounds: &[ProbeExperiment]) -> Vec<VariantRepeatStats> {
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
pub(crate) fn metric_stat(samples: &[f64]) -> MetricStat {
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
pub(crate) fn t_critical_975(n: usize) -> f64 {
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

/// 按 --variants 过滤档位（逗号分隔；无效 id 记 warn 跳过）。
pub(crate) fn filter_variants(
    variants: &[ProbeVariant],
    filter: Option<&str>,
) -> Vec<ProbeVariant> {
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

/// 写实验结果到文件（`-` 表示 stdout，输出原始结果 JSON）。
///
/// 说明: `-` 直出 stdout（含库内原文，口径见模块头 CR-SEC-102 登记）。
pub(crate) fn write_experiment_file(out: &str, experiment: &ProbeExperiment) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(experiment).context("实验结果序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入实验结果失败: {out}"))?;
    }
    Ok(())
}

/// 文本模式打印实验结果摘要。
pub(crate) fn print_experiment_summary(experiment: &ProbeExperiment) {
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
