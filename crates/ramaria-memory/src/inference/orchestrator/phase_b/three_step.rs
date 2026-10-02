//! crates/ramaria-memory/src/inference/orchestrator/phase_b/three_step.rs - Phase B 三步推断与 LLM 调用
//!
//! 设计特点:
//! - Step 1 逐分类个性模式提取 → Step 2 跨分类一致性比较 → Step 3 合成结构化画像。
//! - 每步独立解析，任一步骤失败返回错误，由调用方决定降级策略。
//! - LLM 调用按 provider capability 收敛 max_tokens，预留上下文余量。
//! - 原始响应不落日志，仅记录长度供诊断。

use ramaria_core::{
    RamariaResult,
    traits::{ChatRequest, LlmProvider},
};
use tracing::{debug, error};
use uuid::Uuid;

use crate::inference::inferrer::{
    CategorySignal, ConsistencyAnalysis, InferenceResult, InferredTrait, InferrerConfig,
    build_step1_prompt, build_step2_prompt, build_step3_prompt,
};
use crate::inference::stats::StatsSummary;

use super::convert::convert_to_personality_traits;
use super::parse::{
    parse_category_signals, parse_consistency_analysis, parse_inferred_traits,
    parse_json_with_degrade,
};

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
pub(super) async fn run_three_step_inference(
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
