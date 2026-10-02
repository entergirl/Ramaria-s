//! crates/ramaria-service/src/recall/search.rs - 检索模式分层装配与预算裁剪
//!
//! 设计特点:
//! - 记忆层（L1/L2）走共用召回实现 `assemble_recall`（与在线管线同一份代码）
//! - 分层装配顺序即注入优先级：行为 > 知识 > 表达 > 脉络 > 画像 > 记忆 > 原文
//! - 记忆条目按 include 逐层过滤，保证"未请求的分层不出现"
//! - 预算裁剪：context 按字符边界截断、items 按条数上限截断，均标记 stats.truncated
//! - 嵌入 provider 取快照后在锁外使用（未配置 → 向量通道降级，走 BM25 + 关键词镜像）

use ramaria_core::error::RamariaResult;
use ramaria_core::types::now_ms;
use ramaria_memory::recall::{RecallGates, RecallInput, RecallMemoryLayers, assemble_recall};

use crate::engine::Engine;
use crate::types::{RecallItem, RecallLayer, RecallMode, RecallResult, RecallStats};

use super::layers::{
    behavior_layer, iso_time, knowledge_layer, narrative_layer, style_layer, trait_layer,
};
use super::policy::RecallPolicy;

// =========================================================
// 检索模式
// =========================================================

/// 检索模式：记忆层共用召回 + 其余分层装配。
#[allow(clippy::too_many_arguments)]
pub(super) async fn search(
    engine: &Engine,
    policy: &RecallPolicy,
    persona: &str,
    include: &[RecallLayer],
    query: &str,
    max_items: usize,
    max_chars: usize,
) -> RamariaResult<RecallResult> {
    // 懒加载索引（首次召回构建；存储故障向上传播，不掩盖为"无记忆"）
    engine.ensure_index_loaded().await?;

    let config = engine.config();
    let wants = |layer: RecallLayer| include.contains(&layer);

    // ---- 记忆层（L1/L2）：与在线管线同一份实现 ----
    let memory_wanted = wants(RecallLayer::L1) || wants(RecallLayer::L2);
    let raw_allowed = wants(RecallLayer::Raw) && policy.allow_raw_text;
    if wants(RecallLayer::Raw) && !policy.allow_raw_text {
        tracing::debug!("原文层被隐私策略关闭（allow_raw_text=false），本次不返回原文块");
    }
    // 嵌入 provider 取快照后在锁外使用（未配置 → 向量通道降级，检索走 BM25 + 关键词镜像）
    let embedding = engine.embedding_ref();
    let retriever = engine.retriever_slot();
    let keyword_mirror = engine.keyword_mirror();
    let memory_output = assemble_recall(RecallInput {
        retriever: &*retriever,
        keyword_mirror: &*keyword_mirror,
        storage: engine.storage_ref().as_ref(),
        embedding: embedding.as_deref(),
        query,
        persona_uid: Some(persona),
        retrieval: &config.retrieval,
        decay: &config.decay,
        utt: &config.utt,
        gates: RecallGates {
            memory_rag: memory_wanted,
            utt: raw_allowed,
        },
        // 摘要路子层开关：include 只含 l1 时记忆段与条目都不含 L2（反之亦然）
        memory_layers: RecallMemoryLayers {
            l1: wants(RecallLayer::L1),
            l2: wants(RecallLayer::L2),
        },
        now_ms: now_ms(),
    })
    .await;

    // ---- 分层装配（顺序即注入优先级） ----
    let mut sections: Vec<String> = Vec::new();
    let mut items: Vec<RecallItem> = Vec::new();
    let mut truncated = false;

    // 1) 行为层（默认关闭；显式请求时渲染启用规则）
    if wants(RecallLayer::Behavior) {
        let (text, layer_items) = behavior_layer(engine, persona).await;
        push_section(&mut sections, &mut items, text, layer_items);
    }

    // 2) 知识层（判定器命中 + 时效召回；渲染走 memory 的卡片渲染）
    if wants(RecallLayer::Knowledge) {
        let (text, layer_items) = knowledge_layer(engine, persona, query).await;
        push_section(&mut sections, &mut items, text, layer_items);
    }

    // 3) 表达层：自动风格规则（仅 Ready 状态注入）
    if wants(RecallLayer::Style) {
        let (text, layer_items) = style_layer(engine, persona).await;
        push_section(&mut sections, &mut items, text, layer_items);
    }

    // 4) 脉络层：近期 L1 摘要拼装跨会话叙事
    if wants(RecallLayer::Narrative) {
        let (text, layer_items) = narrative_layer(engine, persona).await;
        push_section(&mut sections, &mut items, text, layer_items);
    }

    // 5) 画像层（L3）：性格标签
    if wants(RecallLayer::L3) {
        let (text, layer_items) = trait_layer(engine, persona).await;
        push_section(&mut sections, &mut items, text, layer_items);
    }

    // 6) 记忆层：共用召回产出的上下文与结构化条目
    // 段落文本已由共用实现按 `memory_layers` 收窄（只请求 l1 时不含事件文本）；
    // 条目再按 include 逐层过滤，保证"未请求的分层不出现"。
    if memory_wanted {
        if let Some(text) = memory_output.memory_context.clone() {
            sections.push(text);
        }
        for hit in &memory_output.hits {
            if let Some(layer) = layer_from_hit(&hit.layer) {
                if wants(layer) {
                    items.push(RecallItem {
                        layer,
                        id: hit.id.clone(),
                        text: hit.text.clone(),
                        score: Some(hit.score),
                        time: iso_time(hit.created_at),
                    });
                }
            }
        }
    }

    // 7) 原文层（最高敏感层；策略允许时以文本段落返回，不进入结构化 items）
    if raw_allowed {
        if let Some(text) = memory_output.utt_context.clone() {
            sections.push(text);
        }
    }

    // ---- 预算裁剪 ----
    let mut context = sections.join("\n\n");
    if context.chars().count() > max_chars {
        context = truncate_chars(&context, max_chars);
        truncated = true;
        tracing::debug!(max_chars, "召回上下文超预算，已按字符边界截断");
    }
    if items.len() > max_items {
        items.truncate(max_items);
        truncated = true;
        tracing::debug!(max_items, "召回条目超上限，已截断");
    }

    let stats = RecallStats {
        mode: RecallMode::Search,
        channels: memory_output.channels.as_map(),
        truncated,
    };
    tracing::info!(
        persona = %persona,
        items = items.len(),
        context_chars = context.chars().count(),
        fused = memory_output.fused_count,
        filtered = memory_output.filtered_count,
        truncated,
        "召回用例完成（检索模式）"
    );

    Ok(RecallResult {
        context,
        items,
        stats,
    })
}

// =========================================================
// 辅助
// =========================================================

/// 检索命中分层字符串 → 召回分层枚举。
fn layer_from_hit(layer: &str) -> Option<RecallLayer> {
    match layer {
        "l1" => Some(RecallLayer::L1),
        "l2" => Some(RecallLayer::L2),
        _ => None,
    }
}

/// 按字符边界截断文本（超出部分丢弃，保证 UTF-8 完整）。
pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// 把非空分层文本与条目并入装配结果。
fn push_section(
    sections: &mut Vec<String>,
    items: &mut Vec<RecallItem>,
    text: Option<String>,
    layer_items: Vec<RecallItem>,
) {
    if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
        sections.push(text);
    }
    items.extend(layer_items);
}
