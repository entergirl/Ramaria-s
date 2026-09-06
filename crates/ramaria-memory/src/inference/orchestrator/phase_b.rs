//! crates/ramaria-memory/src/inference/orchestrator/phase_b.rs - Phase B 三步 LLM 推断编排
//!
//! 设计特点:
//! - run_phase_b_inference: 加载旧 traits → 注入因果链/动机文本 → 三步推断 → 后处理 diff → 持久化。
//! - run_three_step_inference: Step1 分类信号 / Step2 一致性 / Step3 合成结构化 traits。
//! - JSON 三步递进解析（直接解析 → 剥离 think 标签 → 正则提取），失败结构化报错，原始响应不落日志。
//! - convert_to_personality_traits: LLM confidence 优先，缺失时按统计指标动态计算。
//! - 降级由主编排决定：LLM 任一步骤失败回退 mock_infer（基于统计规则的推断）。

use ramaria_core::{
    RamariaError, RamariaResult,
    traits::{ChatRequest, LlmProvider, StorageBackend},
    types::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus, now_ms},
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::inference::{
    causal::{extract_causal_features, format_causal_features_text},
    inferrer::{
        CategorySignal, ConsistencyAnalysis, InferenceResult, InferredTrait, InferrerConfig,
        PostProcessResult, build_step1_prompt, build_step2_prompt, build_step3_prompt,
        format_motive_stats, mock_infer, post_process_inference,
    },
    stats::{CategoryStats, StatsSummary},
};
use crate::utils::{extract_first_json_array, extract_first_json_object, strip_thinking};

use super::types::{PhaseBResult, PhaseBSource};

// =========================================================
// Phase B: LLM 三步结构化推断编排
// =========================================================

/// 执行 Phase B 推断：三步 prompt → LLM → JSON 解析 → post_process → 写入 DB。
///
/// 流程:
/// 1. 从 DB 加载已有 trait 列表（用于后处理对比）。
/// 2. 构建 Step 1 prompt，调用 LLM 获取逐分类性格信号。
/// 3. 构建 Step 2 prompt，调用 LLM 进行跨分类一致性分析。
/// 4. 构建 Step 3 prompt，调用 LLM 合成结构化性格画像。
/// 5. 任一 LLM 步骤失败 → 降级至 mock_infer（基于统计规则推断）。
/// 6. 首轮推断（无旧 traits）跳过 post_process diff 计算。
/// 7. 将推断结果持久化到 personality_traits 表。
///
/// 参数:
/// - `llm`: LLM provider，用于三步推断。
/// - `storage`: 存储后端，用于读写 personality_traits。
/// - `stats`: Phase A 统计摘要。
/// - `persona_uid`: 目标人格标识。
/// - `config`: 推断器配置。
///
/// 返回:
/// - PhaseBResult：包含保存/更新/废弃的 trait 数量及推断来源。
pub async fn run_phase_b_inference(
    llm: &dyn LlmProvider,
    storage: &dyn StorageBackend,
    stats: &StatsSummary,
    persona_uid: &str,
    config: &InferrerConfig,
) -> RamariaResult<PhaseBResult> {
    let persona_owned = persona_uid.to_string();

    // ---- 1. 加载已有 traits ----
    let old_traits = storage
        .list_traits_by_persona(&persona_owned)
        .await
        .map_err(|e| {
            error!(persona_uid = %persona_owned, error = %e, "Phase B: 加载已有 traits 失败");
            e
        })?;

    let is_first_round = old_traits.is_empty();
    if is_first_round {
        info!(persona_uid = %persona_owned, "Phase B: 首轮推断，跳过 post_process diff 和 drift_detection");
    } else {
        info!(
            persona_uid = %persona_owned,
            old_trait_count = old_traits.len(),
            "Phase B: 加载已有 traits，执行增量推断"
        );
    }

    // ---- 1.5. 因果链特征提取（A8） ----
    let causal_text = match storage
        .list_event_relations_by_persona(&persona_owned)
        .await
    {
        Ok(relations) if !relations.is_empty() => {
            // 查询该 persona 的所有事件用于类别映射
            let events = storage
                .list_events_by_persona(&persona_owned, 0, 10000)
                .await
                .unwrap_or_default();
            let features = extract_causal_features(&events, &relations);
            let text = format_causal_features_text(&features);
            if !text.is_empty() {
                debug!(
                    persona_uid = %persona_owned,
                    chain_length = features.chain_length,
                    cycle_count = features.cyclic_patterns.len(),
                    "Phase B: 因果链特征提取完成"
                );
            }
            text
        }
        Ok(_) => {
            debug!(persona_uid = %persona_owned, "Phase B: 无事件关系数据，跳过因果链分析");
            String::new()
        }
        Err(e) => {
            warn!(persona_uid = %persona_owned, error = %e, "Phase B: 查询事件关系失败，跳过因果链分析");
            String::new()
        }
    };

    // ---- 1.6. 动机维度统计文本（E 模块） ----
    let motive_stats_text = if !stats.motive_stats.is_empty() {
        let text = format_motive_stats(&stats.motive_stats, 5);
        if !text.is_empty() {
            debug!(
                persona_uid = %persona_owned,
                motive_count = stats.motive_stats.len(),
                "Phase B: 动机维度统计已格式化"
            );
        }
        text
    } else {
        debug!(persona_uid = %persona_owned, "Phase B: 无动机数据，跳过动机维度统计");
        String::new()
    };

    // ---- 2. 三步 LLM 推断（含降级） ----
    let causal_text_ref: Option<&str> = if causal_text.is_empty() {
        None
    } else {
        Some(&causal_text)
    };
    let motive_stats_ref: Option<&str> = if motive_stats_text.is_empty() {
        None
    } else {
        Some(&motive_stats_text)
    };
    let inference_result = run_three_step_inference(
        llm,
        stats,
        persona_uid,
        config,
        causal_text_ref,
        motive_stats_ref,
    )
    .await;

    let (result, source) = match inference_result {
        Ok(r) => {
            info!(persona_uid = %persona_owned, trait_count = r.traits.len(), "Phase B: LLM 三步推断完成");
            (r, PhaseBSource::LlmInference)
        }
        Err(e) => {
            warn!(persona_uid = %persona_owned, error = %e, "Phase B: LLM 推断失败，降级至 mock_infer");
            (
                mock_infer(stats, &persona_owned),
                PhaseBSource::MockFallback,
            )
        }
    };

    // ---- 3. 后处理：与旧 traits 对比 ----
    let post_result = if is_first_round {
        // 首轮推断：所有 trait 直接新增，不做 diff
        info!(persona_uid = %persona_owned, "Phase B: 首轮推断，所有 trait 直接新增");
        PostProcessResult {
            to_add: result.traits.clone(),
            to_update: vec![],
            to_deprecate: vec![],
            diffs: vec![],
        }
    } else {
        post_process_inference(&result, &old_traits, &persona_owned)
    };

    // ---- 4. 持久化 ----
    let mut traits_saved = 0usize;
    let mut traits_updated = 0usize;
    let mut traits_deprecated = 0usize;
    let mut active_trait_ids: Vec<i64> = Vec::new();

    // 已有 trait 的 ID 列表（用于 Phase C）
    // 先收集未废弃的旧 trait ID
    for t in &old_traits {
        if t.status == TraitStatus::Active {
            active_trait_ids.push(t.id);
        }
    }

    // 4a. 新增 trait
    for mut t in post_result.to_add {
        t.persona_uid = persona_owned.clone();
        t.source = TraitSource::Inferred;
        t.status = TraitStatus::Active;
        // 首轮推断置信度初始值
        if t.confidence == 0.0 {
            t.confidence = 0.5;
        }
        if t.evidence == 0.0 {
            t.evidence = 1.0;
        }
        if t.consistency == 0.0 {
            t.consistency = 0.5;
        }

        match storage.save_trait(&t).await {
            Ok(id) => {
                traits_saved += 1;
                active_trait_ids.push(id);
                debug!(
                    persona_uid = %persona_owned,
                    trait_label = %t.trait_label,
                    trait_id = id,
                    "Phase B: 新增 trait 已保存"
                );
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    trait_label = %t.trait_label,
                    error = %e,
                    "Phase B: 新增 trait 保存失败（跳过，不影响其他 trait）"
                );
            }
        }
    }

    // 4b. 更新已有 trait
    // `to_update` 元素为 (old_id: i64, updated_trait: PersonalityTrait)
    for (old_id, mut updated_trait) in post_result.to_update {
        updated_trait.id = old_id;
        updated_trait.persona_uid = persona_owned.clone();
        updated_trait.source = TraitSource::Inferred;
        updated_trait.status = TraitStatus::Active;

        match storage.save_trait(&updated_trait).await {
            Ok(_) => {
                traits_updated += 1;
                if !active_trait_ids.contains(&old_id) {
                    active_trait_ids.push(old_id);
                }
                debug!(
                    persona_uid = %persona_owned,
                    trait_label = %updated_trait.trait_label,
                    old_id,
                    "Phase B: 更新 trait 已保存"
                );
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    trait_label = %updated_trait.trait_label,
                    error = %e,
                    "Phase B: 更新 trait 保存失败（跳过）"
                );
            }
        }
    }

    // 4c. 废弃旧 trait（`to_deprecate` 为旧 trait ID 列表）
    for old_id in post_result.to_deprecate {
        match storage
            .update_trait_status(old_id, TraitStatus::Deprecated)
            .await
        {
            Ok(_) => {
                traits_deprecated += 1;
                active_trait_ids.retain(|&id| id != old_id);
                debug!(
                    persona_uid = %persona_owned,
                    old_id,
                    "Phase B: trait 已标记废弃"
                );
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    old_id,
                    error = %e,
                    "Phase B: 废弃 trait 状态更新失败（跳过）"
                );
            }
        }
    }

    info!(
        persona_uid = %persona_owned,
        saved = traits_saved,
        updated = traits_updated,
        deprecated = traits_deprecated,
        source = ?source,
        "Phase B: 推断完成并持久化"
    );

    Ok(PhaseBResult {
        traits_saved,
        traits_updated,
        traits_deprecated,
        source,
        trait_ids: active_trait_ids,
        traits: result.traits,
    })
}

// =========================================================
// Phase B 内部: 三步 LLM 推断
// =========================================================

/// 执行三步 LLM 推断（内部函数，不含降级逻辑）。
///
/// 任一步骤失败返回错误，由调用方决定降级策略。
///
/// 参数:
/// - `causal_features_text`: 可选的因果链特征文本（A8 模块产出），注入 Step 1 Prompt。
/// - `motive_stats_text`: 可选的动机维度统计文本（E 模块产出），注入 Step 1 Prompt。
async fn run_three_step_inference(
    llm: &dyn LlmProvider,
    stats: &StatsSummary,
    persona_uid: &str,
    config: &InferrerConfig,
    causal_features_text: Option<&str>,
    motive_stats_text: Option<&str>,
) -> RamariaResult<InferenceResult> {
    // Step 1: 逐分类个性模式提取
    let step1_prompt = build_step1_prompt(stats, config, causal_features_text, motive_stats_text);
    let step1_raw = call_llm_and_get_text(llm, &step1_prompt, config, "Step1").await?;
    let category_signals: Vec<CategorySignal> =
        parse_json_with_degrade(&step1_raw, "Step1", parse_category_signals)?;

    // Step 2: 跨分类一致性比较
    let step2_prompt =
        build_step2_prompt(&category_signals, &stats.cross_category, &stats.categories);
    let step2_raw = call_llm_and_get_text(llm, &step2_prompt, config, "Step2").await?;
    let consistency: ConsistencyAnalysis =
        parse_json_with_degrade(&step2_raw, "Step2", parse_consistency_analysis)?;

    // Step 3: 合成结构化性格画像
    let step3_prompt = build_step3_prompt(&consistency, &category_signals, stats);
    let step3_raw = call_llm_and_get_text(llm, &step3_prompt, config, "Step3").await?;
    let inferred_traits: Vec<InferredTrait> =
        parse_json_with_degrade(&step3_raw, "Step3", parse_inferred_traits)?;

    // 将 InferredTrait 转换为 PersonalityTrait
    // 传入 stats 用于 LLM 未提供 confidence 时的动态校准
    let traits = convert_to_personality_traits(&inferred_traits, persona_uid, stats);

    Ok(InferenceResult {
        category_signals,
        consistency,
        traits,
    })
}

// =========================================================
// LLM 调用辅助
// =========================================================

/// 调用 LLM 非流式接口获取文本响应。
///
/// 使用 provider 的 capability 配置 temperature 和 max_tokens。
async fn call_llm_and_get_text(
    llm: &dyn LlmProvider,
    prompt: &str,
    config: &InferrerConfig,
    step_name: &str,
) -> RamariaResult<String> {
    let capability = llm.capability();

    let request = ChatRequest {
        system_prompt: String::new(), // Phase B 不使用 system prompt
        memory_context: None,
        history: vec![],
        user_message: prompt.to_string(),
        temperature: config.temperature,
        max_tokens: config
            .step_max_tokens
            .min(capability.context_window.saturating_sub(2048)),
        request_id: Uuid::new_v4(),
        template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
    };

    debug!(
        step = step_name,
        prompt_len = prompt.len(),
        temperature = config.temperature,
        max_tokens = request.max_tokens,
        "Phase B: 调用 LLM"
    );

    llm.chat(&request).await.map_err(|e| {
        error!(step = step_name, error = %e, "Phase B: LLM 调用失败");
        e
    })
}

// =========================================================
// JSON 解析（三步递进 + 降级）
// =========================================================

/// 三步递进 JSON 解析 + 自定义解析逻辑。
///
/// 步骤:
/// 1. 直接 `serde_json::from_str`
/// 2. 剥离 `<think>...</think>` 标签后重试
/// 3. 正则提取首对 `{...}` / `[...]` 后解析
///
/// 全部失败返回 Validation 错误。
pub(super) fn parse_json_with_degrade<T>(
    raw: &str,
    step_name: &str,
    parser: impl Fn(&str) -> Option<T>,
) -> RamariaResult<T> {
    // 步骤 1: 直接解析
    if let Some(result) = parser(raw) {
        debug!(step = step_name, "JSON 直接解析成功");
        return Ok(result);
    }

    // 步骤 2: 剥离 think 标签
    let stripped = strip_thinking(raw);
    if stripped != raw
        && let Some(result) = parser(&stripped)
    {
        debug!(step = step_name, "剥离 think 标签后 JSON 解析成功");
        return Ok(result);
    }

    // 步骤 3: 正则提取 JSON
    // 先尝试提取 JSON 数组（Step3 输出是数组）
    if let Some(json_segment) = extract_first_json_array(raw)
        && let Some(result) = parser(&json_segment)
    {
        debug!(step = step_name, "正则提取 JSON 数组后解析成功");
        return Ok(result);
    }

    // 再尝试提取 JSON 对象（Step1/Step2 输出是对象）
    if let Some(json_segment) = extract_first_json_object(raw)
        && let Some(result) = parser(&json_segment)
    {
        debug!(step = step_name, "正则提取 JSON 对象后解析成功");
        return Ok(result);
    }

    // 全部失败
    // 隐私红线：LLM 原始响应不落日志，仅记录长度供诊断
    warn!(
        step = step_name,
        response_len = raw.chars().count(),
        "Phase B: JSON 解析全部失败（原始响应不记录）"
    );
    Err(RamariaError::validation(format!(
        "Phase B {step_name} JSON 解析失败，原始响应 {} 字符（不记录原文，防隐私泄漏）",
        raw.chars().count()
    )))
}

// =========================================================
// 具体 JSON 解析函数
// =========================================================

/// Step 1 响应 JSON 格式。
#[derive(serde::Deserialize)]
struct Step1Response {
    #[serde(rename = "signal_label")]
    signal_label: Option<String>,
    #[serde(rename = "evidence_citation")]
    evidence_citation: Option<String>,
    #[serde(rename = "stability_judgment")]
    stability_judgment: Option<String>,
    #[serde(rename = "sufficient_evidence")]
    sufficient_evidence: Option<bool>,
}

/// 解析 Step 1 响应：LLM 输出为 `{ "分类名": { signal_label, ... }, ... }` 的 JSON 对象。
pub(super) fn parse_category_signals(raw: &str) -> Option<Vec<CategorySignal>> {
    // 尝试作为 map 解析
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw).ok()?;
    let mut signals = Vec::with_capacity(map.len());

    for (category, value) in map {
        // 尝试将 value 解析为 Step1Response
        if let Ok(resp) = serde_json::from_value::<Step1Response>(value) {
            signals.push(CategorySignal {
                category,
                signal_label: resp.signal_label.unwrap_or("insufficient_data".into()),
                evidence_citation: resp.evidence_citation.unwrap_or_default(),
                stability_judgment: resp.stability_judgment.unwrap_or("uncertain".into()),
                sufficient_evidence: resp.sufficient_evidence.unwrap_or(false),
            });
        } else {
            // 降级：为无法解析的分类生成默认信号
            signals.push(CategorySignal {
                category,
                signal_label: "insufficient_data".into(),
                evidence_citation: String::new(),
                stability_judgment: "uncertain".into(),
                sufficient_evidence: false,
            });
        }
    }

    if signals.is_empty() {
        None
    } else {
        Some(signals)
    }
}

/// 解析 Step 2 响应：`{ "base_candidates": [...], "primary_candidates": [...], "accent_candidates": [...], "notes": "..." }`。
pub(super) fn parse_consistency_analysis(raw: &str) -> Option<ConsistencyAnalysis> {
    #[derive(serde::Deserialize)]
    struct Step2Response {
        #[serde(default)]
        base_candidates: Vec<String>,
        #[serde(default)]
        primary_candidates: Vec<String>,
        #[serde(default)]
        accent_candidates: Vec<String>,
        #[serde(default)]
        notes: String,
    }

    let resp: Step2Response = serde_json::from_str(raw).ok()?;
    Some(ConsistencyAnalysis {
        base_candidates: resp.base_candidates,
        primary_candidates: resp.primary_candidates,
        accent_candidates: resp.accent_candidates,
        notes: resp.notes,
    })
}

/// 解析 Step 3 响应：`[{ "layer": "base", "trait_label": "...", "meaning": "...", ... }, ...]`。
///
/// 空数组 `[]` 是 LLM 的合法响应（数据不足时明确表示
/// "无可推断 traits"），应返回 `Some(vec![])` 而非解析失败——否则会
/// 误触发 MockFallback 降级，用 mock 数据污染真实画像。
pub(super) fn parse_inferred_traits(raw: &str) -> Option<Vec<InferredTrait>> {
    #[derive(serde::Deserialize)]
    struct Step3Item {
        #[serde(default)]
        layer: String,
        #[serde(default, rename = "trait_label")]
        trait_label: String,
        #[serde(default)]
        meaning: String,
        #[serde(default)]
        not_meaning: Option<String>,
        #[serde(default)]
        trigger: Option<String>,
        #[serde(default)]
        suppress: Option<String>,
        #[serde(default)]
        related: Option<String>,
        #[serde(default)]
        seq: i32,
        // LLM 推断置信度（0.0..1.0），None 表示 LLM 未提供
        #[serde(default)]
        confidence: Option<f64>,
    }

    let items: Vec<Step3Item> = serde_json::from_str(raw).ok()?;
    // 空数组是合法响应（LLM 明确表示无足够证据），不再视为解析失败
    Some(
        items
            .into_iter()
            .map(|item| InferredTrait {
                layer: item.layer,
                trait_label: item.trait_label,
                meaning: item.meaning,
                not_meaning: item.not_meaning,
                trigger: item.trigger,
                suppress: item.suppress,
                related: item.related,
                seq: item.seq,
                confidence: item.confidence,
            })
            .collect(),
    )
}

// =========================================================
// 类型转换
// =========================================================

/// 将 InferredTrait 转换为 PersonalityTrait。
///
/// 转换规则:
/// - `layer` 字符串映射到 `TraitLayer` 枚举。
/// - 无法识别的 layer 默认归入 Accent。
/// - id=0 表示由 DB 自动分配。
///
/// 置信度/证据量/一致性不再硬编码 0.5/1.0/0.5。
/// - 优先使用 LLM 推断的 confidence 值。
/// - 若 LLM 未提供 confidence，根据 trait_label 前缀匹配 stats 中
///   对应分类的 n_eff/valence_std/share_std 动态计算。
pub(super) fn convert_to_personality_traits(
    inferred: &[InferredTrait],
    persona_uid: &str,
    stats: &StatsSummary,
) -> Vec<PersonalityTrait> {
    let now = now_ms();

    // 根据 n_eff 等统计指标动态计算 evidence/consistency/confidence
    let compute_evidence = |n_eff: f64| n_eff.clamp(0.0, 100.0);
    let compute_consistency = |valence_std: f64, share_std: f64| {
        let avg_std = (valence_std + share_std) / 2.0;
        (1.0 - avg_std).clamp(0.1, 0.95)
    };
    let compute_confidence = |evidence: f64, consistency: f64| {
        if evidence <= 0.0 {
            0.0
        } else {
            consistency * (1.0 - 1.0 / (1.0 + evidence))
        }
    };

    // 按 trait_label 前缀匹配 stats 中对应分类的统计指标
    let find_stats_for_label = |label: &str| -> Option<(&CategoryStats, f64, f64)> {
        stats
            .categories
            .iter()
            .find(|c| label.starts_with(&c.category))
            .map(|cs| {
                let ev = compute_evidence(cs.n_eff);
                let con = compute_consistency(cs.valence_std, cs.share_std);
                (cs, ev, con)
            })
    };

    inferred
        .iter()
        .map(|t| {
            let layer = match t.layer.as_str() {
                "base" => TraitLayer::Base,
                "primary" => TraitLayer::Primary,
                _ => TraitLayer::Accent,
            };

            // 置信度优先取 LLM 的推断值，无则用统计指标计算
            let (confidence, evidence, consistency) = if let Some(llm_conf) = t.confidence {
                // LLM 提供了置信度，用它；evidence/consistency 从 stats 匹配
                let (ev, con) = find_stats_for_label(&t.trait_label)
                    .map(|(_, ev, con)| (ev, con))
                    .unwrap_or((1.0, 0.5));
                tracing::debug!(
                    trait_label = %t.trait_label,
                    llm_conf,
                    computed_conf = ?compute_confidence(ev, con),
                    ev,
                    con,
                    "LLM 推断置信度已解析，evidence/consistency 由统计指标补全"
                );
                (llm_conf, ev, con)
            } else {
                // LLM 未提供置信度，全部由统计指标计算
                let (ev, con, conf) = find_stats_for_label(&t.trait_label)
                    .map(|(_, ev, con)| (ev, con, compute_confidence(ev, con)))
                    .unwrap_or((1.0, 0.5, 0.5));
                tracing::debug!(
                    trait_label = %t.trait_label,
                    ev,
                    con,
                    conf,
                    "LLM 未提供置信度，由统计指标动态计算"
                );
                (conf, ev, con)
            };

            PersonalityTrait {
                id: 0,
                persona_uid: persona_uid.to_string(),
                layer,
                trait_label: t.trait_label.clone(),
                meaning: t.meaning.clone(),
                not_meaning: t.not_meaning.clone(),
                trigger: t.trigger.clone(),
                suppress: t.suppress.clone(),
                related: t.related.clone(),
                seq: t.seq,
                source: TraitSource::Inferred,
                ref_event_id: None,
                ref_l1_id: None,
                confidence,
                evidence,
                consistency,
                status: TraitStatus::Active,
                created_at: now,
                updated_at: now,
            }
        })
        .collect()
}
