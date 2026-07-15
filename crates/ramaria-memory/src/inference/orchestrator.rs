//! rust/crates/ramaria-memory/src/inference/orchestrator.rs - Phase B/C 编排层
//!
//! 设计特点:
//! - Phase B: 三步 prompt 构建 → LLM 调用 → JSON 解析 → post_process → 写入 DB
//! - Phase C: 加载已有 traits/evidence → confidence_update → drift_detection → 持久化
//! - JSON 解析三步递进: 直接解析 → 剥离 think 标签 → 正则提取
//! - 降级策略: 任一 LLM 步骤失败 → 回退 mock_infer（基于统计规则推断）
//! - 首轮推断特殊处理: 无旧 traits 时跳过 post_process diff 和 drift_detection
//! - 依赖注入: 通过 LlmProvider + StorageBackend trait 解耦具体实现
//! - 所有 LLM 调用使用非流式 `chat()`，Phase B/C 不需要流式输出

use ramaria_core::{
    RamariaError, RamariaResult,
    traits::{ChatRequest, LlmProvider, StorageBackend},
    types::{
        EvidenceDirection, MemoryEvent, PersonalityTrait, TraitEvidence, TraitLayer, TraitSource,
        TraitStatus, now_ms,
    },
};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::inference::{
    causal::{extract_causal_features, format_causal_features_text},
    confidence::{ConfidenceConfig, ConfidenceSummary, run_confidence_update},
    drift::{CategoryEventData, DriftSummary, run_drift_detection},
    inferrer::{
        CategorySignal, ConsistencyAnalysis, InferenceResult, InferredTrait, InferrerConfig,
        PostProcessResult, build_step1_prompt, build_step2_prompt, build_step3_prompt, mock_infer,
        post_process_inference,
    },
    shrink::{ShrinkConfig, run_shrinkage_layered},
    stats::StatsSummary,
};
use crate::utils::{extract_first_json_array, extract_first_json_object, strip_thinking};

// =========================================================
// 输出类型
// =========================================================

/// Phase B 推断结果。
///
/// 职责:
/// - 记录 LLM 推断或降级 mock 推断的完整结果。
/// - 供上层（session_lifecycle）判断是否需要触发 Phase C。
#[derive(Debug, Clone)]
pub struct PhaseBResult {
    /// 本次新增的 trait 数量
    pub traits_saved: usize,
    /// 本次更新的 trait 数量
    pub traits_updated: usize,
    /// 本次标记为废弃的 trait 数量
    pub traits_deprecated: usize,
    /// 推断来源：真实 LLM 推断 或 Mock 降级
    pub source: PhaseBSource,
    /// 本次保存/更新后所有活跃 trait 的 ID 列表（供 Phase C 使用）
    pub trait_ids: Vec<i64>,
    /// 推断产出的 PersonalityTrait 列表（供 Phase C 使用）
    pub traits: Vec<PersonalityTrait>,
}

/// Phase B 推断来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseBSource {
    /// 通过真实 LLM 三步推断产出
    LlmInference,
    /// LLM 调用失败，降级为基于统计规则的 mock 推断
    MockFallback,
}

/// Phase C 置信度更新结果。
///
/// 职责:
/// - 记录置信度更新和漂移检测的完整输出。
#[derive(Debug, Clone)]
pub struct PhaseCResult {
    /// 置信度被更新的 trait 数量
    pub traits_updated: usize,
    /// 新增的证据记录数
    pub evidence_saved: usize,
    /// 是否检测到显著漂移（任一分类 needs_review=true）
    pub has_significant_drift: bool,
    /// 触发漂移的分类列表
    pub drift_categories: Vec<String>,
    /// 详细置信度更新摘要
    pub confidence_summary: Option<ConfidenceSummary>,
    /// 详细漂移检测摘要
    pub drift_summary: Option<DriftSummary>,
}

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

    // ---- 1.5. v1.3 因果链特征提取（A8） ----
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

    // ---- 2. 三步 LLM 推断（含降级） ----
    let causal_text_ref: Option<&str> = if causal_text.is_empty() {
        None
    } else {
        Some(&causal_text)
    };
    let inference_result =
        run_three_step_inference(llm, stats, persona_uid, config, causal_text_ref).await;

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
// v1.3 分层先验收缩集成
// =========================================================

/// 从已持久化的人格特质中构建分层先验提示映射。
///
/// 策略:
/// - 仅读取 `Active` 状态的 trait，忽略 `Deprecated` / `Pending`。
/// - 从每条 trait 的 `trait_label`（如"工作""社交"）映射到其 `layer`（Base/Primary/Accent）。
/// - 同一 `trait_label` 出现多次时，按优先级 Base > Primary > Accent 保留最保守的层。
///
/// 说明:
/// - `trait_label` 通常与 Phase A 的 `category` 名称一致，这是两者关联的桥梁。
/// - 若 traits 列表为空（首轮推断），返回空 HashMap，`run_shrinkage_layered` 将退化为全局先验。
///
/// 参数:
/// - `traits`: 从 DB 读取的已有 PersonalityTrait 列表。
///
/// 返回:
/// - trait_label → TraitLayer 的映射。
pub fn build_layer_hints_from_traits(traits: &[PersonalityTrait]) -> HashMap<String, TraitLayer> {
    let mut hints: HashMap<String, TraitLayer> = HashMap::new();

    for t in traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        let label = t.trait_label.clone();
        let layer = t.layer;
        // 优先级: Base > Primary > Accent（数字越小越保守）
        let priority = match layer {
            TraitLayer::Base => 0u8,
            TraitLayer::Primary => 1,
            TraitLayer::Accent => 2,
            _ => 3, // 未知 layer 最低优先级
        };
        hints
            .entry(label)
            .and_modify(|existing| {
                let existing_priority = match *existing {
                    TraitLayer::Base => 0,
                    TraitLayer::Primary => 1,
                    TraitLayer::Accent => 2,
                    _ => 3,
                };
                if priority < existing_priority {
                    *existing = layer;
                }
            })
            .or_insert(layer);
    }

    hints
}

/// 对 Phase A 统计结果应用分层先验收缩。
///
/// 流程:
/// 1. 从 DB 读取该 persona 的上一轮 Active traits。
/// 2. 调用 `build_layer_hints_from_traits` 构建 layer 提示映射。
/// 3. 若 hints 非空，调用 `run_shrinkage_layered` 使用分层先验收缩。
/// 4. 若 hints 为空（首轮推断），调用 `run_shrinkage` 使用全局先验收缩。
/// 5. 收缩结果直接写入 `stats_summary.categories`（in-place 修改）。
///
/// 说明:
/// - 本函数应在 Phase A 统计完成后、Phase B 推断前调用。
/// - DB 读取失败不阻塞管线：降级为全局先验收缩，仅记录 warn 日志。
/// - 收缩后需重新计算 `StatsSummary.cross_category` 指标（如有必要，调用方负责）。
///
/// 参数:
/// - `storage`: 存储后端，用于读取已有 traits。
/// - `stats_summary`: Phase A 统计摘要（可变引用，categories 将被 in-place 收缩）。
/// - `persona_uid`: 目标人格标识。
/// - `shrink_config`: 收缩配置（γ 参数等）。
///
/// 返回:
/// - 使用的 γ 值（供日志记录）。失败时返回 0.0。
pub async fn apply_layered_shrinkage(
    storage: &dyn StorageBackend,
    stats_summary: &mut StatsSummary,
    persona_uid: &str,
    shrink_config: &ShrinkConfig,
) -> f64 {
    if stats_summary.categories.is_empty() {
        debug!(
            persona_uid = %persona_uid,
            "Phase A shrinkage: categories 为空，跳过收缩"
        );
        return 0.0;
    }

    // 1. 读取上一轮的 traits
    let old_traits = match storage.list_traits_by_persona(persona_uid).await {
        Ok(traits) => traits,
        Err(e) => {
            warn!(
                persona_uid = %persona_uid,
                error = %e,
                "Phase A shrinkage: 读取已有 traits 失败，降级为全局先验收缩"
            );
            // 降级: 全局先验收缩
            return run_shrinkage_layered(
                &mut stats_summary.categories,
                shrink_config,
                &HashMap::new(), // 空 hints → 所有分类使用全局先验
            );
        }
    };

    // 2. 构建 layer hints
    let layer_hints = build_layer_hints_from_traits(&old_traits);

    if layer_hints.is_empty() {
        info!(
            persona_uid = %persona_uid,
            "Phase A shrinkage: 首轮推断，使用全局先验收缩"
        );
    } else {
        let accent_count = layer_hints
            .values()
            .filter(|l| matches!(l, TraitLayer::Accent))
            .count();
        info!(
            persona_uid = %persona_uid,
            hint_count = layer_hints.len(),
            accent_count,
            "Phase A shrinkage: 加载上轮 layer 提示，执行分层收缩"
        );
    }

    // 3. 执行分层收缩（空 hints 时退化为全局先验）
    let gamma = run_shrinkage_layered(&mut stats_summary.categories, shrink_config, &layer_hints);

    // 4. 收缩后更新叙事一致性（presentation 分布向先验收缩后一致性提高）
    stats_summary.cross_category.narrative_consistency =
        crate::inference::stats::compute_narrative_consistency(&stats_summary.categories);

    debug!(
        persona_uid = %persona_uid,
        gamma,
        narrative_consistency = stats_summary.cross_category.narrative_consistency,
        "Phase A shrinkage: 分层收缩完成"
    );

    gamma
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
async fn run_three_step_inference(
    llm: &dyn LlmProvider,
    stats: &StatsSummary,
    persona_uid: &str,
    config: &InferrerConfig,
    causal_features_text: Option<&str>,
) -> RamariaResult<InferenceResult> {
    // Step 1: 逐分类个性模式提取
    let step1_prompt = build_step1_prompt(stats, config, causal_features_text);
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
    let traits = convert_to_personality_traits(&inferred_traits, persona_uid);

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
fn parse_json_with_degrade<T>(
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
    let preview: String = raw.chars().take(200).collect();
    warn!(
        step = step_name,
        response_preview = %preview,
        "Phase B: JSON 解析全部失败"
    );
    Err(RamariaError::validation(format!(
        "Phase B {step_name} JSON 解析失败，原始响应前 200 字符: {preview}"
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
fn parse_category_signals(raw: &str) -> Option<Vec<CategorySignal>> {
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
fn parse_consistency_analysis(raw: &str) -> Option<ConsistencyAnalysis> {
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
fn parse_inferred_traits(raw: &str) -> Option<Vec<InferredTrait>> {
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
    }

    let items: Vec<Step3Item> = serde_json::from_str(raw).ok()?;
    if items.is_empty() {
        return None;
    }

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
fn convert_to_personality_traits(
    inferred: &[InferredTrait],
    persona_uid: &str,
) -> Vec<PersonalityTrait> {
    let now = now_ms();

    inferred
        .iter()
        .map(|t| {
            let layer = match t.layer.as_str() {
                "base" => TraitLayer::Base,
                "primary" => TraitLayer::Primary,
                _ => TraitLayer::Accent,
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
                confidence: 0.5,
                evidence: 1.0,
                consistency: 0.5,
                status: TraitStatus::Active,
                created_at: now,
                updated_at: now,
            }
        })
        .collect()
}

// =========================================================
// Phase C: 置信度更新 + 漂移检测编排
// =========================================================

/// 执行 Phase C 更新：置信度更新 + 漂移检测 + 持久化。
///
/// 流程:
/// 1. 从 DB 加载当前 persona 的所有活跃 trait 及其证据记录。
/// 2. 使用新事件数据计算置信度更新（E_total、C、conf）。
/// 3. 执行漂移检测（对比 cluster_snapshots 中的旧分布与新事件分布）。
/// 4. 首轮推断跳过漂移检测（无旧分布可对比）。
/// 5. 持久化更新后的置信度和新增证据记录。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `persona_uid`: 目标人格标识。
/// - `new_traits`: Phase B 产出的 trait 列表。
/// - `events`: 本次 L3 推断使用的事件列表（用于计算证据贡献和漂移检测）。
/// - `is_first_round`: 是否为首轮推断（跳过漂移检测）。
///
/// 返回:
/// - PhaseCResult：包含更新数量、证据数量、漂移检测结果。
pub async fn run_phase_c_update(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    new_traits: &[PersonalityTrait],
    events: &[MemoryEvent],
    is_first_round: bool,
) -> RamariaResult<PhaseCResult> {
    let persona_owned = persona_uid.to_string();
    let now = now_ms();
    let confidence_config = ConfidenceConfig::default();

    if new_traits.is_empty() {
        info!(persona_uid = %persona_owned, "Phase C: 无 trait 需要更新置信度");
        return Ok(PhaseCResult {
            traits_updated: 0,
            evidence_saved: 0,
            has_significant_drift: false,
            drift_categories: vec![],
            confidence_summary: None,
            drift_summary: None,
        });
    }

    // ---- 1. 加载已有 traits 和证据 ----
    let stored_traits = storage
        .list_traits_by_persona(&persona_owned)
        .await
        .map_err(|e| {
            error!(persona_uid = %persona_owned, error = %e, "Phase C: 加载 traits 失败");
            e
        })?;

    // 只处理活跃 trait
    let active_traits: Vec<&PersonalityTrait> = stored_traits
        .iter()
        .filter(|t| t.status == TraitStatus::Active)
        .collect();

    if active_traits.is_empty() {
        info!(persona_uid = %persona_owned, "Phase C: 无活跃 trait，跳过");
        return Ok(PhaseCResult {
            traits_updated: 0,
            evidence_saved: 0,
            has_significant_drift: false,
            drift_categories: vec![],
            confidence_summary: None,
            drift_summary: None,
        });
    }

    // ---- 2. 为每个 trait 加载证据记录 ----
    let mut trait_states: Vec<(i64, f64, Vec<TraitEvidence>)> = Vec::new();
    for t in &active_traits {
        let evidence = storage
            .list_evidence_by_trait(t.id)
            .await
            .unwrap_or_else(|e| {
                warn!(trait_id = t.id, error = %e, "Phase C: 加载 trait 证据失败，使用空列表");
                vec![]
            });
        trait_states.push((t.id, t.confidence, evidence));
    }

    // ---- 3. 准备新事件数据 ----
    // 为每个 trait 构建新事件数据 (confidence, created_at) 和匹配度评分
    // 简化方案：使用事件本身的 valence 作为对该 trait 的证据贡献方向
    let n_traits = active_traits.len();
    let mut new_event_data_by_trait: Vec<Vec<(f64, i64)>> = vec![vec![]; n_traits];
    let mut new_event_scores_by_trait: Vec<Vec<f64>> = vec![vec![]; n_traits];

    for event in events {
        // 事件贡献 = (event.confidence, event.created_at)
        let event_data = (event.confidence, event.created_at);

        for (i, _t) in active_traits.iter().enumerate() {
            new_event_data_by_trait[i].push(event_data);

            // 简化评分：事件效价作为对该 trait 的语义匹配度代理
            // 实际应由 LLM 对每条 trait 评估事件匹配度，此处为计算可行性做近似
            let score = event.valence.clamp(-1.0, 1.0);
            new_event_scores_by_trait[i].push(score);
        }
    }

    // ---- 4. 执行置信度更新 ----
    let confidence_summary = run_confidence_update(
        &trait_states,
        &new_event_data_by_trait,
        &new_event_scores_by_trait,
        now,
        &confidence_config,
    );

    // ---- 5. 持久化置信度更新 ----
    let mut traits_updated = 0usize;
    for update in &confidence_summary.updates {
        match storage
            .update_trait_confidence(
                update.trait_id,
                update.conf_after,
                update.e_total_after,
                update.consistency_after,
            )
            .await
        {
            Ok(_) => {
                traits_updated += 1;
                debug!(
                    trait_id = update.trait_id,
                    conf_before = %update.conf_before,
                    conf_after = %update.conf_after,
                    "Phase C: 置信度更新已持久化"
                );
            }
            Err(e) => {
                warn!(
                    trait_id = update.trait_id,
                    error = %e,
                    "Phase C: 置信度更新持久化失败（跳过）"
                );
            }
        }
    }

    // ---- 6. 保存证据记录 ----
    let mut evidence_saved = 0usize;
    for (i, t) in active_traits.iter().enumerate() {
        let trait_id = t.id;
        for (j, event) in events.iter().enumerate() {
            // 跳过无效的事件 ID
            if event.id == 0 {
                continue;
            }

            let score = new_event_scores_by_trait[i].get(j).copied().unwrap_or(0.0);

            let evidence = TraitEvidence {
                id: 0,
                trait_id,
                event_id: event.id,
                direction: if score >= 0.0 {
                    EvidenceDirection::Support
                } else {
                    EvidenceDirection::Contradict
                },
                score,
                decay: 1.0, // 新证据初始衰减为 1.0
                created_at: now,
            };

            match storage.save_evidence(&evidence).await {
                Ok(_) => evidence_saved += 1,
                Err(e) => {
                    warn!(
                        trait_id,
                        event_id = event.id,
                        error = %e,
                        "Phase C: 证据记录保存失败（跳过）"
                    );
                }
            }
        }
    }

    // ---- 7. 漂移检测 ----
    let (has_significant_drift, drift_categories, drift_summary) = if is_first_round {
        info!(persona_uid = %persona_owned, "Phase C: 首轮推断，跳过漂移检测");
        (false, vec![], None)
    } else {
        match detect_and_summarize_drift(storage, &persona_owned, events).await {
            Ok(summary) => {
                let categories: Vec<String> = summary
                    .categories
                    .iter()
                    .filter(|c| c.needs_review)
                    .map(|c| c.category.clone())
                    .collect();
                let has_drift = !categories.is_empty();
                if has_drift {
                    info!(
                        persona_uid = %persona_owned,
                        drift_categories = ?categories,
                        "Phase C: 检测到性格漂移"
                    );
                }
                (has_drift, categories, Some(summary))
            }
            Err(e) => {
                warn!(persona_uid = %persona_owned, error = %e, "Phase C: 漂移检测失败，跳过");
                (false, vec![], None)
            }
        }
    };

    info!(
        persona_uid = %persona_owned,
        traits_updated,
        evidence_saved,
        has_significant_drift,
        "Phase C: 更新完成"
    );

    Ok(PhaseCResult {
        traits_updated,
        evidence_saved,
        has_significant_drift,
        drift_categories,
        confidence_summary: Some(confidence_summary),
        drift_summary,
    })
}

// =========================================================
// 漂移检测辅助
// =========================================================

/// 执行漂移检测：对比 cluster_snapshots 中的旧分布与新事件分布。
///
/// 从 storage 加载当前快照作为旧分布，从 events 中按 category 分组提取新分布。
async fn detect_and_summarize_drift(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    events: &[MemoryEvent],
) -> RamariaResult<DriftSummary> {
    use crate::inference::drift::DriftConfig;
    use crate::inference::stats::extract_primary_category;

    // 按 category 对事件分组
    let mut categories_map: std::collections::BTreeMap<String, Vec<&MemoryEvent>> =
        std::collections::BTreeMap::new();

    for event in events {
        let cat = extract_primary_category(event);
        categories_map.entry(cat).or_default().push(event);
    }

    let mut category_data: Vec<CategoryEventData> = Vec::new();

    for (category, cat_events) in &categories_map {
        // 加载该分类的旧快照数据
        let snapshots = storage
            .get_current_snapshots(persona_uid, category)
            .await
            .unwrap_or_else(|e| {
                warn!(
                    persona_uid,
                    category,
                    error = %e,
                    "Phase C: 加载快照失败，使用空旧分布"
                );
                vec![]
            });

        // 从快照提取旧分布（简化为使用均值作为单点分布）
        let old_valences: Vec<f64> = snapshots.iter().map(|_s| 0.0).collect(); // 实际应从快照的 samples JSON 中恢复
        let old_shares: Vec<f64> = snapshots.iter().map(|_s| 0.5).collect();

        // 从当前事件提取新分布
        let new_valences: Vec<f64> = cat_events.iter().map(|e| e.valence).collect();
        let new_shares: Vec<f64> = cat_events.iter().map(|e| e.share).collect();
        let new_saliences: Vec<f64> = cat_events.iter().map(|e| e.salience).collect();

        // 如果旧分布为空（新分类），跳过漂移检测
        if old_valences.is_empty() || old_valences.iter().all(|&v| v == 0.0) {
            debug!(
                persona_uid,
                category, "Phase C: 新分类无旧分布，跳过漂移检测"
            );
            continue;
        }

        category_data.push(CategoryEventData {
            category: category.clone(),
            old_valences,
            new_valences,
            old_shares,
            new_shares,
            old_saliences: vec![],
            new_saliences,
        });
    }

    if category_data.is_empty() {
        return Ok(DriftSummary {
            categories: vec![],
            review_count: 0,
            any_drift: false,
        });
    }

    let drift_config = DriftConfig::default();
    Ok(run_drift_detection(&category_data, &drift_config))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::stats::{
        CategoryStats, CrossCategoryMetrics, RepresentativeEvent, StatsSummary,
    };

    // =========================================================
    // JSON 解析测试
    // =========================================================

    #[test]
    fn parse_category_signals_valid_json() {
        let raw = r#"{
            "工作": {"signal_label": "尽责", "evidence_citation": "valence_mean=0.6", "stability_judgment": "stable", "sufficient_evidence": true},
            "社交": {"signal_label": "社交回避", "evidence_citation": "share_mean=0.2", "stability_judgment": "contextual", "sufficient_evidence": false}
        }"#;
        let result = parse_category_signals(raw);
        assert!(result.is_some());
        let signals = result.unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].category, "工作");
        assert_eq!(signals[0].signal_label, "尽责");
        assert!(signals[0].sufficient_evidence);
    }

    #[test]
    fn parse_category_signals_malformed_json() {
        let raw = "这不是 JSON";
        let result = parse_category_signals(raw);
        assert!(result.is_none());
    }

    #[test]
    fn parse_consistency_analysis_valid() {
        let raw = r#"{"base_candidates":["尽责"],"primary_candidates":["温和"],"accent_candidates":["幽默"],"notes":"分析说明"}"#;
        let result = parse_consistency_analysis(raw);
        assert!(result.is_some());
        let analysis = result.unwrap();
        assert_eq!(analysis.base_candidates, vec!["尽责"]);
        assert_eq!(analysis.primary_candidates, vec!["温和"]);
        assert_eq!(analysis.accent_candidates, vec!["幽默"]);
    }

    #[test]
    fn parse_inferred_traits_valid() {
        let raw = r#"[
            {"layer":"primary","trait_label":"温和","meaning":"待人友善","not_meaning":null,"trigger":null,"suppress":null,"related":null,"seq":0},
            {"layer":"accent","trait_label":"幽默","meaning":"用自嘲化解尴尬","not_meaning":"并非轻浮","trigger":"朋友聚会","suppress":"正式场合","related":"与温和互补","seq":1}
        ]"#;
        let result = parse_inferred_traits(raw);
        assert!(result.is_some());
        let traits = result.unwrap();
        assert_eq!(traits.len(), 2);
        assert_eq!(traits[0].trait_label, "温和");
        assert_eq!(traits[0].layer, "primary");
        assert_eq!(traits[1].trait_label, "幽默");
        assert_eq!(traits[1].meaning, "用自嘲化解尴尬");
    }

    #[test]
    fn parse_inferred_traits_empty_array() {
        let raw = "[]";
        let result = parse_inferred_traits(raw);
        assert!(result.is_none(), "空数组应返回 None");
    }

    // =========================================================
    // JSON 三步解析降级测试
    // =========================================================

    #[test]
    fn parse_json_direct_success() {
        let raw = r#"{"base_candidates":["A"],"primary_candidates":["B"],"accent_candidates":[],"notes":"ok"}"#;
        let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_json_with_think_tags() {
        let raw = r#"<think>让我分析一下...</think>
{"base_candidates":["尽责"],"primary_candidates":["温和"],"accent_candidates":[],"notes":"ok"}"#;
        let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
        assert!(result.is_ok(), "剥离 think 标签后应解析成功");
    }

    #[test]
    fn parse_json_extract_from_text() {
        let raw = r#"分析结果如下：
{"base_candidates":["尽责"],"primary_candidates":["温和"],"accent_candidates":[],"notes":"ok"}
以上是分析结果。"#;
        let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
        assert!(result.is_ok(), "正则提取 JSON 后应解析成功");
    }

    #[test]
    fn parse_json_all_fail() {
        let raw = "完全没有 JSON 的文本";
        let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
        assert!(result.is_err());
    }

    // =========================================================
    // 类型转换测试
    // =========================================================

    #[test]
    fn convert_inferred_to_personality_traits() {
        let inferred = vec![
            InferredTrait {
                layer: "base".into(),
                trait_label: "尽责".into(),
                meaning: "对任务高度负责".into(),
                not_meaning: None,
                trigger: None,
                suppress: None,
                related: None,
                seq: 0,
            },
            InferredTrait {
                layer: "primary".into(),
                trait_label: "温和".into(),
                meaning: "待人友善".into(),
                not_meaning: Some("并非软弱".into()),
                trigger: None,
                suppress: None,
                related: None,
                seq: 0,
            },
        ];

        let traits = convert_to_personality_traits(&inferred, "user-0001");

        assert_eq!(traits.len(), 2);
        assert_eq!(traits[0].layer, TraitLayer::Base);
        assert_eq!(traits[0].persona_uid, "user-0001");
        assert_eq!(traits[0].source, TraitSource::Inferred);
        assert_eq!(traits[0].status, TraitStatus::Active);
        assert_eq!(traits[0].confidence, 0.5);

        assert_eq!(traits[1].layer, TraitLayer::Primary);
        assert_eq!(traits[1].not_meaning, Some("并非软弱".into()));
    }

    #[test]
    fn convert_unknown_layer_defaults_to_accent() {
        let inferred = vec![InferredTrait {
            layer: "unknown_layer".into(),
            trait_label: "测试".into(),
            meaning: "测试含义".into(),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: 0,
        }];

        let traits = convert_to_personality_traits(&inferred, "user-0001");
        assert_eq!(traits[0].layer, TraitLayer::Accent);
    }

    // =========================================================
    // helper: 构建测试用 StatsSummary
    // =========================================================

    fn make_test_stats() -> StatsSummary {
        StatsSummary {
            total_events_in: 15,
            total_events_filtered: 12,
            confirmed_count: 12,
            tentative_count: 0,
            discarded_count: 3,
            category_count: 2,
            categories: vec![
                CategoryStats {
                    category: "工作".into(),
                    event_count: 8,
                    n_eff: 6.5,
                    valence_mean: 0.55,
                    valence_std: 0.35,
                    valence_positive_ratio: 0.75,
                    share_mean: 0.7,
                    share_std: 0.2,
                    presentation_objective_ratio: 0.5,
                    presentation_subjective_ratio: 0.3,
                    presentation_mixed_ratio: 0.2,
                    group_weight: 0.6,
                },
                CategoryStats {
                    category: "社交".into(),
                    event_count: 4,
                    n_eff: 3.2,
                    valence_mean: -0.1,
                    valence_std: 0.5,
                    valence_positive_ratio: 0.45,
                    share_mean: 0.8,
                    share_std: 0.15,
                    presentation_objective_ratio: 0.2,
                    presentation_subjective_ratio: 0.6,
                    presentation_mixed_ratio: 0.2,
                    group_weight: 0.4,
                },
            ],
            cross_category: CrossCategoryMetrics {
                emotional_stability: 0.45,
                narrative_consistency: 0.7,
                attitude_contradiction_count: 0,
                share_skewness: 0.1,
                share_kurtosis: -0.5,
            },
            representative_events: vec![RepresentativeEvent {
                title: "项目验收".into(),
                summary: "完成项目验收".into(),
                attitude: Some("对成果满意".into()),
                valence: 0.8,
                salience: 0.9,
                category: "工作".into(),
            }],
        }
    }

    // =========================================================
    // StatsSummary 构建测试
    // =========================================================

    #[test]
    fn test_stats_summary_structure() {
        let stats = make_test_stats();
        assert_eq!(stats.total_events_in, 15);
        assert_eq!(stats.category_count, 2);
        assert_eq!(stats.categories.len(), 2);
        assert_eq!(stats.categories[0].category, "工作");
        assert_eq!(stats.categories[1].category, "社交");
        assert!(!stats.representative_events.is_empty());
        assert_eq!(stats.representative_events[0].title, "项目验收");
    }

    // =========================================================
    // T-V12-3-010: 纯函数边界测试（无需 StorageBackend/LlmProvider mock）
    // 说明: 异步集成测试（Phase B/C 全链路 + mock LLM）在
    //       `crates/ramaria-app/tests/m3_integration.rs` 中。
    // =========================================================

    // ---- PhaseBResult / PhaseCResult 构造与字段 ----

    #[test]
    fn phase_b_result_fields() {
        let result = PhaseBResult {
            traits_saved: 3,
            traits_updated: 1,
            traits_deprecated: 0,
            source: PhaseBSource::LlmInference,
            trait_ids: vec![1, 2, 3],
            traits: vec![],
        };
        assert_eq!(result.traits_saved, 3);
        assert_eq!(result.traits_updated, 1);
        assert_eq!(result.traits_deprecated, 0);
        assert_eq!(result.source, PhaseBSource::LlmInference);
        assert_eq!(result.trait_ids.len(), 3);
    }

    #[test]
    fn phase_b_result_mock_fallback_source() {
        let result = PhaseBResult {
            traits_saved: 2,
            traits_updated: 0,
            traits_deprecated: 0,
            source: PhaseBSource::MockFallback,
            trait_ids: vec![1, 2],
            traits: vec![],
        };
        assert_eq!(result.source, PhaseBSource::MockFallback);
    }

    #[test]
    fn phase_c_result_zero_events() {
        let result = PhaseCResult {
            traits_updated: 0,
            evidence_saved: 0,
            has_significant_drift: false,
            drift_categories: vec![],
            confidence_summary: None,
            drift_summary: None,
        };
        assert_eq!(result.traits_updated, 0);
        assert!(!result.has_significant_drift);
    }

    #[test]
    fn phase_c_result_with_drift() {
        let result = PhaseCResult {
            traits_updated: 3,
            evidence_saved: 6,
            has_significant_drift: true,
            drift_categories: vec!["工作".into(), "社交".into()],
            confidence_summary: None,
            drift_summary: None,
        };
        assert!(result.has_significant_drift);
        assert_eq!(result.drift_categories.len(), 2);
        assert!(result.drift_categories.contains(&"工作".to_string()));
    }

    // ---- PhaseBSource enum ----

    #[test]
    fn phase_b_source_equality() {
        assert_eq!(PhaseBSource::LlmInference, PhaseBSource::LlmInference);
        assert_ne!(PhaseBSource::LlmInference, PhaseBSource::MockFallback);
    }

    #[test]
    fn phase_b_source_debug() {
        // 验证 Debug 实现可正常工作
        let s = format!("{:?}", PhaseBSource::LlmInference);
        assert!(s.contains("LlmInference"));
        let s = format!("{:?}", PhaseBSource::MockFallback);
        assert!(s.contains("MockFallback"));
    }

    #[test]
    fn phase_b_source_clone() {
        let s1 = PhaseBSource::MockFallback;
        let s2 = s1.clone();
        assert_eq!(s1, s2);
    }

    // ---- convert_to_personality_traits 边界情况 ----

    #[test]
    fn convert_empty_inferred_list() {
        let traits = convert_to_personality_traits(&[], "user-0001");
        assert!(traits.is_empty());
    }

    #[test]
    fn convert_multiple_layers_preserves_order() {
        let inferred = vec![
            InferredTrait {
                layer: "base".into(),
                trait_label: "底色A".into(),
                meaning: "m".into(),
                not_meaning: None,
                trigger: None,
                suppress: None,
                related: None,
                seq: 0,
            },
            InferredTrait {
                layer: "primary".into(),
                trait_label: "主色A".into(),
                meaning: "m".into(),
                not_meaning: None,
                trigger: None,
                suppress: None,
                related: None,
                seq: 0,
            },
            InferredTrait {
                layer: "accent".into(),
                trait_label: "点缀A".into(),
                meaning: "m".into(),
                not_meaning: None,
                trigger: Some("条件".into()),
                suppress: None,
                related: None,
                seq: 0,
            },
        ];
        let traits = convert_to_personality_traits(&inferred, "user-0001");
        assert_eq!(traits.len(), 3);
        assert_eq!(traits[0].layer, TraitLayer::Base);
        assert_eq!(traits[1].layer, TraitLayer::Primary);
        assert_eq!(traits[2].layer, TraitLayer::Accent);
    }

    #[test]
    fn convert_preserves_all_fields() {
        let inferred = vec![InferredTrait {
            layer: "accent".into(),
            trait_label: "幽默".into(),
            meaning: "用自嘲化解尴尬".into(),
            not_meaning: Some("并非轻浮".into()),
            trigger: Some("朋友聚会".into()),
            suppress: Some("正式场合".into()),
            related: Some("与温和互补".into()),
            seq: 3,
        }];
        let traits = convert_to_personality_traits(&inferred, "user-0001");
        let t = &traits[0];
        assert_eq!(t.trait_label, "幽默");
        assert_eq!(t.meaning, "用自嘲化解尴尬");
        assert_eq!(t.not_meaning, Some("并非轻浮".into()));
        assert_eq!(t.trigger, Some("朋友聚会".into()));
        assert_eq!(t.suppress, Some("正式场合".into()));
        assert_eq!(t.related, Some("与温和互补".into()));
        assert_eq!(t.seq, 3);
        assert_eq!(t.source, TraitSource::Inferred);
        assert_eq!(t.status, TraitStatus::Active);
    }

    // ---- JSON 解析边界 ----

    #[test]
    fn parse_category_signals_partial_fields() {
        // 部分字段缺失时应使用默认值
        let raw = r#"{"工作": {"signal_label": "尽责"}}"#;
        let result = parse_category_signals(raw);
        assert!(result.is_some());
        let signals = result.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_label, "尽责");
        assert_eq!(signals[0].stability_judgment, "uncertain");
        assert!(!signals[0].sufficient_evidence);
    }

    #[test]
    fn parse_category_signals_missing_signal_label() {
        let raw = r#"{"工作": {"sufficient_evidence": true}}"#;
        let result = parse_category_signals(raw);
        assert!(result.is_some());
        let signals = result.unwrap();
        assert_eq!(signals[0].signal_label, "insufficient_data");
    }

    #[test]
    fn parse_consistency_analysis_empty_candidates() {
        let raw =
            r#"{"base_candidates":[],"primary_candidates":[],"accent_candidates":[],"notes":""}"#;
        let result = parse_consistency_analysis(raw);
        assert!(result.is_some());
        let analysis = result.unwrap();
        assert!(analysis.base_candidates.is_empty());
        assert!(analysis.primary_candidates.is_empty());
    }

    #[test]
    fn parse_inferred_traits_missing_optional_fields() {
        let raw = r#"[{"layer":"base","trait_label":"T","meaning":"M","seq":0}]"#;
        let result = parse_inferred_traits(raw);
        assert!(result.is_some());
        let traits = result.unwrap();
        assert_eq!(traits[0].trait_label, "T");
        assert!(traits[0].not_meaning.is_none());
        assert!(traits[0].trigger.is_none());
    }

    #[test]
    fn parse_inferred_traits_non_json_prefix() {
        // LLM 有时会在 JSON 前加说明文字
        let raw = r#"好的，以下是分析结果：
[{"layer":"base","trait_label":"尽责","meaning":"M","seq":0}]"#;
        // 直接 parse_inferred_traits 会失败，但 parse_json_with_degrade 能处理
        let direct = parse_inferred_traits(raw);
        assert!(direct.is_none(), "直接解析应失败（有前缀文本）");

        let degraded = parse_json_with_degrade(raw, "test", parse_inferred_traits);
        assert!(degraded.is_ok(), "三步解析应成功提取 JSON 数组");
    }

    // ---- InferrerConfig 默认值 ----

    #[test]
    fn inferrer_config_defaults() {
        let config = InferrerConfig::default();
        assert_eq!(config.temperature, 0.3);
        assert_eq!(config.max_tokens, 2048);
        assert_eq!(config.low_evidence_threshold, 5.0);
        assert_eq!(config.step_max_tokens, 2048);
    }

    // ---- Prompt 构建非空 ----

    #[test]
    fn build_prompts_are_non_empty() {
        let stats = make_test_stats();
        let config = InferrerConfig::default();
        let result = mock_infer(&stats, "user-0001");

        let p1 = build_step1_prompt(&stats, &config, None);
        assert!(!p1.is_empty());
        assert!(p1.contains("工作"));
        assert!(p1.contains("性格心理分析师"));

        let p2 = build_step2_prompt(
            &result.category_signals,
            &stats.cross_category,
            &stats.categories,
        );
        assert!(!p2.is_empty());
        assert!(p2.contains("base_candidates"));
        assert!(p2.contains("跨领域一致性"));

        let p3 = build_step3_prompt(&result.consistency, &result.category_signals, &stats);
        assert!(!p3.is_empty());
        assert!(p3.contains("layer"));
        assert!(p3.contains("trait_label"));
    }

    // =========================================================
    // v1.3 分层先验收缩集成
    // =========================================================

    /// 构造测试用 PersonalityTrait。
    fn make_trait(
        id: i64,
        label: &str,
        layer: TraitLayer,
        confidence: f64,
        status: TraitStatus,
    ) -> PersonalityTrait {
        let now = now_ms();
        PersonalityTrait {
            id,
            persona_uid: "user-0001".into(),
            trait_label: label.into(),
            meaning: format!("{} 的描述", label),
            layer,
            confidence,
            evidence: 1.0,
            consistency: 0.5,
            source: TraitSource::Inferred,
            status,
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: 1,
            ref_event_id: None,
            ref_l1_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn build_layer_hints_empty_traits() {
        let hints = build_layer_hints_from_traits(&[]);
        assert!(hints.is_empty());
    }

    #[test]
    fn build_layer_hints_basic() {
        let traits = vec![
            make_trait(1, "工作", TraitLayer::Base, 0.7, TraitStatus::Active),
            make_trait(2, "社交", TraitLayer::Accent, 0.5, TraitStatus::Active),
        ];
        let hints = build_layer_hints_from_traits(&traits);
        assert_eq!(hints.len(), 2);
        assert_eq!(hints.get("工作"), Some(&TraitLayer::Base));
        assert_eq!(hints.get("社交"), Some(&TraitLayer::Accent));
    }

    #[test]
    fn build_layer_hints_skips_deprecated() {
        let traits = vec![
            make_trait(1, "工作", TraitLayer::Base, 0.7, TraitStatus::Deprecated),
            make_trait(2, "社交", TraitLayer::Accent, 0.5, TraitStatus::Active),
        ];
        let hints = build_layer_hints_from_traits(&traits);
        assert_eq!(hints.len(), 1, "Deprecated trait 应被跳过");
        assert_eq!(hints.get("社交"), Some(&TraitLayer::Accent));
        assert!(!hints.contains_key("工作"));
    }

    #[test]
    fn build_layer_hints_priority_base_over_accent() {
        // 同一 label 出现两次: Base > Accent
        let traits = vec![
            make_trait(1, "工作", TraitLayer::Accent, 0.5, TraitStatus::Active),
            make_trait(2, "工作", TraitLayer::Base, 0.7, TraitStatus::Active),
        ];
        let hints = build_layer_hints_from_traits(&traits);
        assert_eq!(hints.len(), 1);
        assert_eq!(
            hints.get("工作"),
            Some(&TraitLayer::Base),
            "Base 优先级应高于 Accent"
        );
    }

    #[test]
    fn build_layer_hints_priority_primary_over_accent() {
        let traits = vec![
            make_trait(3, "社交", TraitLayer::Accent, 0.4, TraitStatus::Active),
            make_trait(4, "社交", TraitLayer::Primary, 0.6, TraitStatus::Active),
        ];
        let hints = build_layer_hints_from_traits(&traits);
        assert_eq!(hints.len(), 1);
        assert_eq!(
            hints.get("社交"),
            Some(&TraitLayer::Primary),
            "Primary 优先级应高于 Accent"
        );
    }

    #[test]
    fn build_layer_hints_mixed_status() {
        let traits = vec![
            make_trait(1, "工作", TraitLayer::Base, 0.7, TraitStatus::Active),
            make_trait(2, "工作", TraitLayer::Accent, 0.5, TraitStatus::Deprecated),
            make_trait(3, "社交", TraitLayer::Primary, 0.6, TraitStatus::Active),
        ];
        let hints = build_layer_hints_from_traits(&traits);
        assert_eq!(hints.len(), 2);
        // 工作: 只有 Active 的 Base，Deprecated Accent 被忽略
        assert_eq!(hints.get("工作"), Some(&TraitLayer::Base));
        assert_eq!(hints.get("社交"), Some(&TraitLayer::Primary));
    }
}
