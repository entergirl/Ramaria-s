//! crates/ramaria-memory/src/behavior/rule_gen/translate.rs - 规则文本 LLM 翻译
//!
//! 设计特点:
//! - 宽容 JSON 解析：提取首个对象块，缺字段按空处理
//! - 极性一致性校验：文本极性与簇 valence 符号不符时可重试一次
//! - 翻译失败返回 None 走降级候选规则，不阻塞其余簇

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider as LlmProviderTrait};
use uuid::Uuid;

use super::super::clustering::RefinedCluster;
use super::{
    BEHAVIOR_RULE_PROMPT_VERSION, RuleGenConfig, check_polarity, polarity_of_text, validate_avoid,
};

// =========================================================
// LLM 翻译
// =========================================================

/// 翻译 prompt 构造（示例取簇内真实态度文本）。
///
/// 参数:
/// - `cluster`: 提炼后的簇。
///
/// 说明:
/// - 示例取 `representative_attitudes`（簇成员前 3 条真实态度，已由提炼层截断）；
///   无态度数据时示例段整体省略（不留空壳占位）。
pub(super) fn build_translation_prompt(cluster: &RefinedCluster) -> String {
    let valence_desc = if cluster.situation.valence_mean > 0.1 {
        "积极/正面"
    } else if cluster.situation.valence_mean < -0.1 {
        "消极/负面"
    } else {
        "中性"
    };
    let examples = if cluster.representative_attitudes.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = cluster
            .representative_attitudes
            .iter()
            .enumerate()
            .map(|(i, attitude)| format!("{}. {}", i + 1, attitude))
            .collect();
        format!("代表事件态度示例:\n{}\n", lines.join("\n"))
    };
    format!(
        "你是行为规则翻译器。根据情境-反应簇的统计，把簇内反应模式翻译为一条可注入的人格规则文本。\n\
         规则文本格式：'当{{情境}}时，倾向{{反应}}……'，要求用自然口语表达、可解释。\n\
         同时给出 avoid 列表（该情境下应避免的话题/行为，0-3 个词）。\n\
         \n\
         簇关键词: {keywords}\n\
         簇情绪方向: {valence_desc}（valence={valence:.2}）\n\
         {examples}\n\
         只输出 JSON，格式: {{\"reaction\": \"...\", \"avoid\": [\"...\"]}}",
        keywords = cluster.situation.keywords.join("、"),
        valence = cluster.situation.valence_mean,
        examples = examples,
    )
}

/// 宽容解析 LLM 返回的 JSON（提取首个 `{...}` 块）。
pub(super) fn parse_translation(raw: &str) -> RamariaResult<(Option<String>, Vec<String>)> {
    let start = raw
        .find('{')
        .ok_or_else(|| RamariaError::validation("行为规则翻译输出缺少 JSON 对象"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| RamariaError::validation("行为规则翻译输出缺少 JSON 结束符"))?;
    if end <= start {
        return Err(RamariaError::validation("行为规则翻译输出 JSON 结构非法"));
    }
    let body = &raw[start..=end];
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| RamariaError::validation(format!("行为规则翻译 JSON 解析失败: {e}")))?;
    let reaction = parsed
        .get("reaction")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let avoid = parsed
        .get("avoid")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok((reaction, avoid))
}

/// LLM 翻译规则文本（含重试）。
///
/// 参数:
/// - `llm`: LLM provider（mock 可注入）。
/// - `cluster`: 提炼后的簇。
/// - `config`: 规则生成配置。
///
/// 返回:
/// - `Ok(Some((reaction, avoid)))`: 翻译成功且极性校验通过。
/// - `Ok(None)`: 翻译失败或极性校验不通过（重试后仍失败）→ 降级候选规则。
/// - `Err`: 非翻译类错误（不应出现；内部错误已转为 None）。
pub async fn translate_reaction(
    llm: &dyn LlmProviderTrait,
    cluster: &RefinedCluster,
    config: &RuleGenConfig,
) -> RamariaResult<Option<(String, Vec<String>)>> {
    let prompt = build_translation_prompt(cluster);

    for attempt in 0..=config.max_retries {
        let request = ChatRequest {
            system_prompt: String::new(),
            memory_context: None,
            history: Vec::new(),
            user_message: prompt.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            request_id: Uuid::new_v4(),
            template_version: BEHAVIOR_RULE_PROMPT_VERSION.to_string(),
            enable_thinking: None,
        };
        let raw = match llm.chat(&request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    attempt,
                    error = %e,
                    "行为规则翻译 LLM 调用失败（第 {} 次）",
                    attempt + 1
                );
                continue;
            }
        };
        let (reaction, avoid) = match parse_translation(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(attempt, error = %e, "行为规则翻译输出解析失败");
                continue;
            }
        };
        let Some(reaction) = reaction else {
            tracing::warn!(attempt, "行为规则翻译输出缺少 reaction 字段");
            continue;
        };

        // 极性一致性校验：不一致且还有重试次数 → 重新翻译
        let verdict = check_polarity(polarity_of_text(&reaction), cluster.situation.valence_mean);
        if !verdict.consistent && attempt < config.max_retries {
            tracing::warn!(
                attempt,
                text_polarity = %format!("{:.2}", verdict.text_polarity),
                cluster_valence = %format!("{:.2}", verdict.cluster_valence),
                "行为规则翻译极性不一致，重试"
            );
            continue;
        }
        if !verdict.consistent {
            tracing::warn!(
                text_polarity = %format!("{:.2}", verdict.text_polarity),
                cluster_valence = %format!("{:.2}", verdict.cluster_valence),
                "行为规则翻译极性校验不通过（重试后），降级候选规则"
            );
            return Ok(None);
        }
        return Ok(Some((reaction, validate_avoid(&avoid, cluster))));
    }
    tracing::warn!("行为规则翻译失败（达到重试上限），降级候选规则");
    Ok(None)
}
