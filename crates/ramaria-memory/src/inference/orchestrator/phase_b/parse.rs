//! crates/ramaria-memory/src/inference/orchestrator/phase_b/parse.rs - Phase B JSON 解析
//!
//! 设计特点:
//! - 三步递进解析：直接解析 → 剥离 think 标签 → 正则提取 JSON。
//! - 全失败返回 Validation 错误，仅记录响应长度，原始响应不落日志。
//! - Step1/Step2 部分字段缺失时按默认值降级，不整体判失败。
//! - Step3 空数组为合法响应（明确表示无可推断 traits），避免误触发 MockFallback。

use ramaria_core::{RamariaError, RamariaResult};
use tracing::{debug, warn};

use crate::inference::inferrer::{CategorySignal, ConsistencyAnalysis, InferredTrait};
use crate::utils::{extract_first_json_array, extract_first_json_object, strip_thinking};

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
pub(in crate::inference::orchestrator) fn parse_json_with_degrade<T>(
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
pub(in crate::inference::orchestrator) fn parse_category_signals(
    raw: &str,
) -> Option<Vec<CategorySignal>> {
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
pub(in crate::inference::orchestrator) fn parse_consistency_analysis(
    raw: &str,
) -> Option<ConsistencyAnalysis> {
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
pub(in crate::inference::orchestrator) fn parse_inferred_traits(
    raw: &str,
) -> Option<Vec<InferredTrait>> {
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
