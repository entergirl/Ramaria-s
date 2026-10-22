//! crates/ramaria-cli/src/commands/probe/run/experiment.rs - 探针 档位实验装配与统计聚合
//!
//! 设计特点:
//! - 档位实验构建（基线 + 单参数档）与 repeat 多轮装配
//! - 跨轮聚合（均值 / 标准差 / 95% CI）与 t 临界值
//! - 档位过滤与结果写出 / 文本摘要

use super::super::now_iso8601;
use super::super::types::AblationProfile;
use super::super::types::DATASET_SCHEMA_VERSION;
use super::super::types::ProbeDataset;
use super::super::types::ProbeExperiment;
use super::super::types::ProbeRepeatMeta;
use super::super::types::ProbeVariant;
use super::super::types::ProbeVariantResult;
use super::super::types::VariantParams;
use super::session::{
    collect_run_diagnostics, ensure_privacy_with_engine, load_effective_config,
    rebuild_utt_for_config, run_single_question,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaError;
use ramaria_service::Engine;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

mod output;
pub(crate) mod stats;

pub(crate) use output::filter_variants;
use output::{print_experiment_summary, write_experiment_file};
pub(crate) use stats::{aggregate_repeat_stats, metric_stat};

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

    // 复原失败的告警走 stderr（--json 模式的 stdout 只保留回执信封）
    if experiment.utt_restored == Some(false) {
        crate::ui::warn("utt 块按基准参数复原失败，请运行 `ramaria blocks rebuild --force` 重建");
    }

    // Step 6: 输出
    if let Some(out) = output.as_deref() {
        // `-` + --json：stdout 只出一行信封，原始结果放 data.raw（避免两段 JSON）
        if out == "-" && json {
            let data = serde_json::json!({
                "file": "-",
                "persona_uid": experiment.persona_uid,
                "variants": experiment.variants.len(),
                "runs": run_count,
                "utt_restored": experiment.utt_restored,
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
                "utt_restored": experiment.utt_restored,
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
    let results = run_variants(engine, dataset, &variants, &base_config, rebuild_utt, limit).await;

    // Step 5.5: 复原 utt 块。
    // 档位重建会按各档参数重切全库 utt 块（破坏性改写），实验结束必须按基准配置切回；
    // 复原在错误上抛之前执行，失败路径同样还原用户原始切分。
    let utt_restored = restore_utt_blocks(engine, &base_config, rebuild_utt).await;

    let results = results?;

    Ok(ProbeExperiment {
        dataset_file: super::super::file_label(dataset_path),
        dataset_seed: dataset.seed,
        persona_uid: dataset.persona_uid.clone(),
        rebuild_utt,
        utt_restored,
        variants: results,
        repeat: None,
        diagnostics: Some(diagnostics),
        generated_at: now_iso8601(),
    })
}

/// 逐档位执行实验（覆盖档位参数 → 按切分重建 utt 块 → 逐题跑对话管线）。
///
/// 说明:
/// - 同一切分参数（θ_gap / 条数）在本轮只重建一次 utt 块，仅 top_k 不同的档位复用已建块。
/// - `rebuild_utt=false` 时不改动 utt 块，直接复用库中现有切分。
/// - 单题失败不中断批量（失败原因记入题项）；单档位重建失败记 warn 后继续。
async fn run_variants(
    engine: &Arc<Engine>,
    dataset: &ProbeDataset,
    variants: &[ProbeVariant],
    base_config: &RamariaConfig,
    rebuild_utt: bool,
    limit: Option<usize>,
) -> anyhow::Result<Vec<ProbeVariantResult>> {
    let mut results = Vec::with_capacity(variants.len());
    // 已按切分参数（θ_gap / 条数）重建过的档位集合；top_k 不参与切分，复用已建块。
    let mut rebuilt_cuts: std::collections::HashMap<(u32, u32), ()> =
        std::collections::HashMap::new();
    for variant in variants {
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
        let cut_key = (variant.theta_gap_minutes, variant.max_msgs_per_block);
        if rebuild_utt && !rebuilt_cuts.contains_key(&cut_key) {
            if let Err(e) = rebuild_utt_for_config(engine, &variant_config).await {
                tracing::warn!(
                    variant_id = %variant.id,
                    %e,
                    "档位 utt 块重建失败，本次档位可能未按目标参数生效"
                );
            }
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

    Ok(results)
}

/// 按基准配置复原 utt 块（档位实验的破坏性切分在结束后必须还原）。
///
/// 参数:
/// - `base_config`: 档位实验的配置基准（其 `[utt]` 段即用户原始切分参数）。
/// - `rebuild_utt`: 本轮是否重建过 utt 块；否时不做任何写操作。
///
/// 返回:
/// - `None`: 未重建 utt 块，无需复原。
/// - `Some(true)`: 已按基准参数重建完成。
/// - `Some(false)`: 复原失败（记 warn，不影响已产出的实验结果）。
async fn restore_utt_blocks(
    engine: &Arc<Engine>,
    base_config: &RamariaConfig,
    rebuild_utt: bool,
) -> Option<bool> {
    if !rebuild_utt {
        return None;
    }
    match rebuild_utt_for_config(engine, base_config).await {
        Ok(()) => {
            tracing::info!("probe run 结束，utt 块已按基准参数复原");
            Some(true)
        }
        Err(e) => {
            tracing::warn!(%e, "probe run 结束，utt 块按基准参数复原失败");
            Some(false)
        }
    }
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
        utt_restored: last.utt_restored,
        variants: last.variants.clone(),
        repeat: Some(ProbeRepeatMeta {
            count: repeat,
            per_variant,
        }),
        diagnostics: last.diagnostics.clone(),
        generated_at: now_iso8601(),
    })
}
