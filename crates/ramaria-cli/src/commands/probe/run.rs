//! crates/ramaria-cli/src/commands/probe/run.rs - 探针 probe run 档位批量实验执行族
//!
//! 设计特点:
//! - 从命令入口接收数据集与参数档位，逐档位批量执行对话管线，收集输出与可测指标。
//! - 支持 `--repeat N` 统计法：多次独立运行后跨轮配对聚合均值 / 标准差 / 95% 置信区间。
//! - 隐私确认与静默降级：线上 provider 需确认，档位/单题失败记 warn 不中断批量。
//! - 档位 utt 块按切分参数去重重建，top_k 变化复用已建块，避免 embedding 调用倍增。
//! - 结果输出辅助（写入 / 文本摘要）随 run 族维护，不对外公开。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaError;
use ramaria_memory::utt::builder::UttBuilder;

use super::types::{
    AblationProfile, DATASET_SCHEMA_VERSION, DatasetItem, ItemRepeatStats, MetricStat,
    ProbeDataset, ProbeExperiment, ProbeMetrics, ProbeRepeatMeta, ProbeRunItem, ProbeVariant,
    ProbeVariantResult, VariantParams, VariantRepeatStats,
};

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
pub(super) async fn run_experiment(
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
        generated_at: super::now_iso8601(),
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
        generated_at: super::now_iso8601(),
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
pub(super) fn aggregate_repeat_stats(rounds: &[ProbeExperiment]) -> Vec<VariantRepeatStats> {
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
pub(super) fn metric_stat(samples: &[f64]) -> MetricStat {
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
pub(super) fn t_critical_975(n: usize) -> f64 {
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
        ramaria_core::text::truncate_chars_bare(&reply, total_chars)
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
pub(super) fn filter_variants(
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
