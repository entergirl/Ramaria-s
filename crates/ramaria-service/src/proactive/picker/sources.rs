//! crates/ramaria-service/src/proactive/picker/sources.rs - Ramaria 主动对话选题素材与四源候选
//!
//! 设计特点:
//! - 素材一次加载：近窗事件（未了结 / 时间节点 / 规则情境共用）、高显著事件、事件会话映射
//! - 未了结口径：负效价或高显著 + 所属会话静默（无消息或查询失败按无法判定保守跳过）
//! - 时间节点：事件结束后第 N 天跟进，要求会话映射存在（不要求静默）
//! - 高显著源：负效价出口约束（强负效价事件不从此源出口），会话映射可缺省
//! - 规则情境：事件文本构造查询上下文，复用情境路由纯函数，命中主规则为候选
//! - 隐私：事件文本仅供内存中转（查询构造），不落日志

use std::collections::{HashMap, HashSet};

use tracing::warn;
use uuid::Uuid;

use ramaria_core::config::{BehaviorConfig, ProactiveConfig};
use ramaria_core::lock::read_recover;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{MemoryEvent, Message, MessageRole, MessageSource};
use ramaria_memory::behavior::{
    QueryKeywordNormalizer, RoutingParams, build_query_context_with_normalizer, route_rules,
};

use crate::engine::Engine;

use super::{
    ANCHOR_SUMMARY_MAX_CHARS, EVENT_LOAD_LIMIT, MS_PER_DAY, SOURCE_EVENT, SOURCE_RULE,
    SOURCE_TIME_NODE, SOURCE_UNRESOLVED, TopicCandidate,
};

// =========================================================
// 素材加载
// =========================================================

/// 加载近窗事件（未了结 / 时间节点 / 规则情境源共用；查询失败降级为空）。
pub(super) async fn load_window_events(
    storage: &dyn StorageBackend,
    persona: &str,
    since: i64,
) -> Vec<MemoryEvent> {
    match storage
        .list_events_since(persona, since, EVENT_LOAD_LIMIT)
        .await
    {
        Ok(events) => events,
        Err(e) => {
            warn!(error = %e, "主动选题：近窗事件查询失败，事件源降级跳过");
            Vec::new()
        }
    }
}

/// 加载高显著事件（高显著事件源；查询失败降级为空）。
pub(super) async fn load_salient_events(
    storage: &dyn StorageBackend,
    persona: &str,
    since: i64,
    threshold: f64,
) -> Vec<MemoryEvent> {
    match storage
        .list_events_by_salience(persona, since, threshold, EVENT_LOAD_LIMIT)
        .await
    {
        Ok(events) => events,
        Err(e) => {
            warn!(error = %e, "主动选题：高显著事件查询失败，事件源降级跳过");
            Vec::new()
        }
    }
}

/// 加载事件所属会话映射（空事件集合跳过查询；查询失败降级为空映射）。
pub(super) async fn load_session_map(
    storage: &dyn StorageBackend,
    window_events: &[MemoryEvent],
    salient_events: &[MemoryEvent],
) -> HashMap<i64, Uuid> {
    let mut ids: Vec<i64> = window_events.iter().map(|event| event.id).collect();
    ids.extend(salient_events.iter().map(|event| event.id));
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return HashMap::new();
    }
    match storage.list_event_session_map(&ids).await {
        Ok(map) => map,
        Err(e) => {
            warn!(error = %e, "主动选题：事件会话映射查询失败，事件跟进源降级跳过");
            HashMap::new()
        }
    }
}

// =========================================================
// 四源构建
// =========================================================

/// 源②：未了结事件（负效价或高显著 + 所属会话在事件结束后无新对话）。
///
/// 说明:
/// - 静默判定要求会话存在且最近一条消息时间不晚于事件结束；无消息或查询失败
///   视为「无法判定」，保守跳过（不视为确已静默）；同会话查询结果缓存一次；
/// - 产出即登记事件 id（同一事件不再进后续源）。
pub(super) async fn build_unresolved_candidates(
    storage: &dyn StorageBackend,
    window_events: &[MemoryEvent],
    session_map: &HashMap<i64, Uuid>,
    config: &ProactiveConfig,
    claimed: &mut HashSet<i64>,
) -> Vec<TopicCandidate> {
    let mut candidates = Vec::new();
    // 同会话静默判定缓存（None = 无法判定，按跳过处理）
    let mut silence_cache: HashMap<Uuid, Option<i64>> = HashMap::new();
    for event in window_events {
        let negative = event.valence < config.unresolved_valence_threshold;
        let salient = event.salience >= config.event_salience_threshold;
        if !(negative || salient) {
            continue;
        }
        let Some(session_id) = session_map.get(&event.id).copied() else {
            continue;
        };
        let last_message = match silence_cache.get(&session_id) {
            Some(cached) => *cached,
            None => {
                let queried = match storage.last_message_time_by_session(session_id).await {
                    Ok(value) => value,
                    Err(e) => {
                        warn!(
                            error = %e,
                            "主动选题：会话最近消息查询失败，跳过该事件的未了结判定"
                        );
                        None
                    }
                };
                silence_cache.insert(session_id, queried);
                queried
            }
        };
        let Some(last_message) = last_message else {
            continue;
        };
        if last_message > event.end {
            continue;
        }
        claimed.insert(event.id);
        candidates.push(TopicCandidate {
            source: SOURCE_UNRESOLVED,
            topic_key: event.id.to_string(),
            session_id: Some(session_id),
            anchor: event_anchor(event),
            confidence: Some(event.confidence),
            salience: event.salience,
            valence: event.valence,
            score: 0.0,
        });
    }
    candidates
}

/// 源③：时间节点（事件结束后第 N 天跟进；要求会话映射存在，不要求静默）。
///
/// 说明:
/// - 事件尚未结束（`now < end`）按第 0 天处理；
/// - 产出即登记事件 id（同一事件不再进后续源）。
pub(super) fn build_time_node_candidates(
    window_events: &[MemoryEvent],
    session_map: &HashMap<i64, Uuid>,
    now: i64,
    config: &ProactiveConfig,
    claimed: &mut HashSet<i64>,
) -> Vec<TopicCandidate> {
    let mut candidates = Vec::new();
    for event in window_events {
        if claimed.contains(&event.id) {
            continue;
        }
        let elapsed_days = now.saturating_sub(event.end).max(0) / MS_PER_DAY;
        if elapsed_days != config.follow_up_days as i64 {
            continue;
        }
        let Some(session_id) = session_map.get(&event.id).copied() else {
            continue;
        };
        claimed.insert(event.id);
        candidates.push(TopicCandidate {
            source: SOURCE_TIME_NODE,
            topic_key: event.id.to_string(),
            session_id: Some(session_id),
            anchor: event_anchor(event),
            confidence: Some(event.confidence),
            salience: event.salience,
            valence: event.valence,
            score: 0.0,
        });
    }
    candidates
}

/// 源①：高显著事件（负效价出口约束：强负效价事件不从此源出口）。
///
/// 说明:
/// - 会话映射可缺省（None = 问候类新建会话落点）；
/// - 得分由打分层计算，本函数只组装候选。
pub(super) fn build_salient_candidates(
    salient_events: &[MemoryEvent],
    session_map: &HashMap<i64, Uuid>,
    config: &ProactiveConfig,
    claimed: &mut HashSet<i64>,
) -> Vec<TopicCandidate> {
    let mut candidates = Vec::new();
    for event in salient_events {
        if claimed.contains(&event.id) {
            continue;
        }
        if event.valence < config.unresolved_valence_threshold {
            continue;
        }
        claimed.insert(event.id);
        candidates.push(TopicCandidate {
            source: SOURCE_EVENT,
            topic_key: event.id.to_string(),
            session_id: session_map.get(&event.id).copied(),
            anchor: event_anchor(event),
            confidence: Some(event.confidence),
            salience: event.salience,
            valence: event.valence,
            score: 0.0,
        });
    }
    candidates
}

/// 源④：行为规则情境（近窗事件文本构造查询上下文，路由命中取主规则为候选）。
///
/// 说明:
/// - 无近窗事件或无规则直接跳过；规则读取 / 查询构造失败记 warn 后降级跳过；
/// - 查询侧关键词规范化词表取关键词镜像锁内快照，锁外使用；
/// - 候选得分直取路由得分（不参与事件打分公式）。
pub(super) async fn build_rule_candidate(
    engine: &Engine,
    persona: &str,
    window_events: &[MemoryEvent],
    behavior: &BehaviorConfig,
) -> Option<TopicCandidate> {
    if window_events.is_empty() {
        return None;
    }
    let storage = engine.storage_ref().as_ref();
    let rules = match storage.list_behavior_rules_by_persona(persona).await {
        Ok(rules) => rules,
        Err(e) => {
            warn!(error = %e, "主动选题：行为规则读取失败，规则源跳过");
            return None;
        }
    };
    if rules.is_empty() {
        return None;
    }

    // 事件上下文伪消息：按时间升序排列（最近事件对齐查询窗口尾部），仅内存中转不落库
    let messages: Vec<Message> = window_events
        .iter()
        .rev()
        .map(|event| {
            let mut message = Message::new(
                Uuid::nil(),
                MessageRole::User,
                format!("{} {}", event.title.trim(), event.summary.trim()),
                MessageSource::Local,
            );
            message.created_at = event.created_at;
            message
        })
        .collect();

    // 查询侧关键词规范化：词表从关键词镜像锁内取快照（值形态，锁外复用）
    let normalizer = {
        let mirror = engine.keyword_mirror();
        let guard = read_recover(&*mirror, "proactive.picker.keyword_mirror");
        QueryKeywordNormalizer::from_pool(guard.pool())
    };
    let embedding = engine.embedding_ref();
    let query =
        match build_query_context_with_normalizer(&messages, embedding.as_deref(), &normalizer)
            .await
        {
            Ok(query) => query,
            Err(e) => {
                warn!(error = %e, "主动选题：规则情境查询构造失败，规则源跳过");
                return None;
            }
        };

    let result = route_rules(&rules, &query, &RoutingParams::from(behavior));
    if !result.matched {
        return None;
    }
    let primary = result.primary?;
    let anchor = primary
        .rule
        .reaction
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| truncate_chars(text, ANCHOR_SUMMARY_MAX_CHARS));
    Some(TopicCandidate {
        source: SOURCE_RULE,
        topic_key: primary.rule.id.to_string(),
        session_id: None,
        anchor,
        confidence: None,
        salience: 0.0,
        valence: primary.rule.situation.valence_mean,
        score: primary.score,
    })
}

// =========================================================
// 私有辅助
// =========================================================

/// 组装事件锚点：`标题：摘要`（摘要按字符截断；任一侧为空时省略该侧，全空为 None）。
fn event_anchor(event: &MemoryEvent) -> Option<String> {
    let title = event.title.trim();
    let summary = truncate_chars(event.summary.trim(), ANCHOR_SUMMARY_MAX_CHARS);
    match (title.is_empty(), summary.is_empty()) {
        (true, true) => None,
        (true, false) => Some(summary),
        (false, true) => Some(title.to_string()),
        (false, false) => Some(format!("{title}：{summary}")),
    }
}

/// 按字符截断文本（未超限时原样返回）。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}
