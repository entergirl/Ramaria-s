//! crates/ramaria-service/src/recall/overview.rs - 概览模式时间线装配与渲染
//!
//! 设计特点:
//! - 无检索输入时按时间线返回最近记忆（近期 L1 + L2 事件 + 画像与知识摘要）
//! - 返回结构与检索模式一致（`context` + `items` + `stats.mode = overview`）
//! - 时间倒序、受 `max_items` / `max_chars` 约束，空库返回空结构（不报错）
//! - 只装配有时间线语义的素材：行为 / 表达风格 / 脉络 / 原文四层不参与
//!   （概览没有当前输入，行为路由与话题匹配无依据）
//! - 知识段不走判定器：概览没有输入，直接取 active 事实摘要作为时间线素材

use std::collections::BTreeMap;

use ramaria_core::error::RamariaResult;

use crate::engine::Engine;
use crate::types::{RecallItem, RecallLayer, RecallMode, RecallResult, RecallStats};

use super::layers::{MAX_AUX_LAYER_ITEMS, iso_time, knowledge_overview_items, trait_layer};
use super::search::truncate_chars;

// =========================================================
// 概览模式
// =========================================================

/// 概览模式：无检索输入时按时间线返回最近记忆（近期 L1 + L2 事件 + 画像与知识摘要）。
///
/// 说明:
/// - 返回结构与检索模式一致（`context` + `items` + `stats.mode = overview`）；
/// - 时间倒序、受 `max_items` / `max_chars` 约束，空库返回空结构（不报错）；
/// - 分层范围：本模式只装配 L1 / L2 / L3（画像）/ 知识事实四类"有时间线语义"的素材；
///   行为、表达风格、脉络、原文四层不参与——概览没有当前输入，行为路由与话题匹配无依据。
/// - 知识段不走判定器（`[knowledge].detector_enabled`）：判定器以"用户当前消息"为输入，
///   概览模式没有输入，故直接取 active 事实摘要作为时间线素材。
pub(super) async fn overview(
    engine: &Engine,
    persona: &str,
    include: &[RecallLayer],
    max_items: usize,
    max_chars: usize,
) -> RamariaResult<RecallResult> {
    let storage = engine.storage_ref();
    let wants = |layer: RecallLayer| include.contains(&layer);
    // 概览候选缓存的读取条数（各层独立上限，最终按时间合并截断）
    let fetch_limit = max_items.max(MAX_AUX_LAYER_ITEMS) as u32;

    // (时间戳, 分层, 条目)
    let mut candidates: Vec<(i64, RecallItem)> = Vec::new();

    // 近期 L1 摘要
    if wants(RecallLayer::L1) {
        match storage
            .list_recent_l1_by_persona(persona, fetch_limit)
            .await
        {
            Ok(list) => {
                for l1 in list {
                    candidates.push((
                        l1.created_at,
                        RecallItem {
                            layer: RecallLayer::L1,
                            id: l1.id.to_string(),
                            text: l1.summary,
                            score: None,
                            time: iso_time(l1.created_at),
                        },
                    ));
                }
            }
            Err(e) => tracing::warn!(persona, error = %e, "概览：读取近期 L1 失败，跳过"),
        }
    }

    // 近期 L2 事件
    if wants(RecallLayer::L2) {
        match storage
            .list_events_by_persona(persona, 0, fetch_limit as i64)
            .await
        {
            Ok(events) => {
                for event in events {
                    candidates.push((
                        event.created_at,
                        RecallItem {
                            layer: RecallLayer::L2,
                            id: event.id.to_string(),
                            text: format!("{} — {}", event.title, event.summary),
                            score: None,
                            time: iso_time(event.created_at),
                        },
                    ));
                }
            }
            Err(e) => tracing::warn!(persona, error = %e, "概览：读取事件失败，跳过"),
        }
    }

    // 画像摘要（L3 性格标签）
    if wants(RecallLayer::L3) {
        let (_, layer_items) = trait_layer(engine, persona).await;
        for item in layer_items {
            let ts = item
                .time
                .as_ref()
                .map(|t| t.timestamp_millis())
                .unwrap_or_default();
            candidates.push((ts, item));
        }
    }

    // 知识事实摘要
    if wants(RecallLayer::Knowledge) {
        let (_, layer_items) = knowledge_overview_items(engine, persona).await;
        for item in layer_items {
            let ts = item
                .time
                .as_ref()
                .map(|t| t.timestamp_millis())
                .unwrap_or_default();
            candidates.push((ts, item));
        }
    }

    // 时间倒序 → 条数上限 → 渲染时间线
    candidates.sort_by_key(|(created_at, _)| std::cmp::Reverse(*created_at));
    let mut truncated = false;
    if candidates.len() > max_items {
        candidates.truncate(max_items);
        truncated = true;
    }
    let items: Vec<RecallItem> = candidates.into_iter().map(|(_, item)| item).collect();

    let mut context = render_overview(&items);
    if context.chars().count() > max_chars {
        context = truncate_chars(&context, max_chars);
        truncated = true;
    }

    tracing::info!(
        persona = %persona,
        items = items.len(),
        truncated,
        "召回用例完成（概览模式）"
    );

    Ok(RecallResult {
        context,
        items,
        stats: RecallStats {
            mode: RecallMode::Overview,
            channels: BTreeMap::new(),
            truncated,
        },
    })
}

/// 渲染概览时间线文本（`[记忆概览]` + 逐条分层与时间）。
fn render_overview(items: &[RecallItem]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut lines = Vec::with_capacity(items.len() + 1);
    lines.push("[记忆概览]".to_string());
    for (index, item) in items.iter().enumerate() {
        let time = item
            .time
            .as_ref()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "时间未知".to_string());
        lines.push(format!(
            "{}. ({}) {} | {}",
            index + 1,
            item.layer.as_str().to_uppercase(),
            time,
            item.text
        ));
    }
    lines.join("\n")
}
