//! crates/ramaria-service/src/persona/import/decompose.rs - Ramaria 预设拆解
//!
//! 设计特点:
//! - 隐私未确认 / 无输入 / LLM 失败均降级为空条目并给出跳过原因
//! - 拆解文本按预设块拼装，保持条目顺序稳定

use ramaria_memory::preset::{DecomposedEntries, ParsedPreset};
use ramaria_memory::{ColdStartConfig, decompose_profile_text};

use crate::engine::Engine;
use crate::types::DecomposeExecution;

// =========================================================
// 可选 LLM 拆解
// =========================================================

/// 执行可选 LLM 拆解（隐私门禁 + 输入组装；失败降级不阻断）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `parsed`: 预设解析产物（拆解输入 = ST personality 原文与 `[blocks]` 汇总）。
/// - `no_ai`: true 表示用户关闭拆解（未请求，不计跳过原因）。
///
/// 返回:
/// - 拆解条目（失败 / 跳过时为空）与执行情况；
/// - 拆解失败（LLM 调用 / 响应解析）与隐私未确认均降级，错误不上抛。
pub(super) async fn run_decompose(
    engine: &Engine,
    parsed: &ParsedPreset,
    no_ai: bool,
) -> (DecomposedEntries, DecomposeExecution) {
    if no_ai {
        return (
            DecomposedEntries::default(),
            DecomposeExecution {
                requested: false,
                executed: false,
                skipped_reason: None,
                fact_count: 0,
                trait_count: 0,
            },
        );
    }

    let text = compose_decompose_text(parsed);
    if text.is_empty() {
        return (DecomposedEntries::default(), skipped_execution("no_input"));
    }

    match crate::privacy::online_privacy_confirmed(engine).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!("预设拆解跳过：线上 provider 隐私未确认");
            return (
                DecomposedEntries::default(),
                skipped_execution("privacy_unconfirmed"),
            );
        }
        Err(e) => {
            // 隐私状态不可判定时保守按未确认处理（不调用 LLM）
            tracing::warn!(error = %e, "隐私确认读取失败，跳过预设拆解");
            return (
                DecomposedEntries::default(),
                skipped_execution("privacy_unconfirmed"),
            );
        }
    }

    match decompose_profile_text(
        engine.llm_ref().as_ref(),
        &text,
        &ColdStartConfig::default(),
    )
    .await
    {
        Ok(decomposed) => {
            let entries = DecomposedEntries {
                traits: decomposed.traits,
                facts: decomposed.facts,
            };
            let execution = DecomposeExecution {
                requested: true,
                executed: true,
                skipped_reason: None,
                fact_count: entries.facts.len(),
                trait_count: entries.traits.len(),
            };
            (entries, execution)
        }
        Err(e) => {
            // 隐私红线：错误细节不含原文
            tracing::warn!(error = %e, "预设拆解失败，降级为不拆解");
            (
                DecomposedEntries::default(),
                skipped_execution("llm_failed"),
            )
        }
    }
}

/// 构造拆解输入文本：ST personality 原文与 `[blocks]` 段以空行连接（整体 trim）。
fn compose_decompose_text(parsed: &ParsedPreset) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(raw) = parsed.personality_raw.as_deref() {
        if !raw.trim().is_empty() {
            parts.push(raw.trim().to_string());
        }
    }
    for (key, value) in &parsed.blocks {
        parts.push(format!("## {key}\n\n{value}"));
    }
    parts.join("\n\n").trim().to_string()
}

/// 未执行拆解的执行情况（requested=true，附跳过原因）。
fn skipped_execution(reason: &str) -> DecomposeExecution {
    DecomposeExecution {
        requested: true,
        executed: false,
        skipped_reason: Some(reason.to_string()),
        fact_count: 0,
        trait_count: 0,
    }
}
