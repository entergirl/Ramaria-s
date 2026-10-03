//! crates/ramaria-service/src/proactive/picker.rs - Ramaria 主动对话真实选题器
//!
//! 设计特点:
//! - 四源候选按构建顺序即去重优先级：未了结（负效价或高显著 + 会话静默）→ 时间节点
//!   （结束后第 N 天跟进）→ 高显著事件 → 行为规则情境（事件上下文路由命中）
//! - 统一过滤链：选题冷却 → 负效价出口兜底 → 不连选（上一投递为负效价时本轮不选负效价）
//! - 打分层：置信度先门槛后折扣、显著性主权重、效价强度加成；规则候选直取路由得分
//! - 轻触达兜底：四源皆空且不在冷却窗口时产出低权重候选；冷却窗口内保持沉默不硬凑话题
//! - 决策：判据开启交由 AI 裁决（三态记账）；关闭时按算法排序直取第一名，不调用 LLM
//! - 隐私：日志只记来源 / 计数 / 开关与判据结论，不记候选锚点与事件文本
//!
//! 模块划分:
//! - `sources`：素材加载与四源候选构建（事件查询 / 静默判定 / 规则情境路由）

use async_trait::async_trait;
use tracing::{debug, info, warn};
use uuid::Uuid;

use ramaria_core::config::ProactiveConfig;

use crate::engine::Engine;

use super::judge::{self, JudgeCandidate, JudgeOutcome, JudgeSignals};
use super::schedule::TopicPicker;
use super::state::{self, ProactiveState};
use super::topic::ProactiveDirective;

mod sources;

use sources::{
    build_rule_candidate, build_salient_candidates, build_time_node_candidates,
    build_unresolved_candidates, load_salient_events, load_session_map, load_window_events,
};

// =========================================================
// 常量
// =========================================================

/// 事件源单次加载上限。
const EVENT_LOAD_LIMIT: u32 = 200;
/// 候选锚点内事件摘要的字符截断上限。
const ANCHOR_SUMMARY_MAX_CHARS: usize = 80;
/// 判据候选列表的编号前缀（`c0`/`c1`…；与 judge 系统提示词约定一致）。
const JUDGE_CANDIDATE_ID_PREFIX: &str = "c";
/// 轻触达候选的选题键（冷却记录与去重的稳定标识）。
const LIGHT_TOUCH_TOPIC_KEY: &str = "light_touch";

/// 来源标识：未了结事件。
const SOURCE_UNRESOLVED: &str = "unresolved";
/// 来源标识：时间节点。
const SOURCE_TIME_NODE: &str = "time_node";
/// 来源标识：高显著事件。
const SOURCE_EVENT: &str = "event";
/// 来源标识：行为规则情境。
const SOURCE_RULE: &str = "rule";
/// 来源标识：轻触达兜底。
const SOURCE_LIGHT_TOUCH: &str = "light_touch";

/// 一天的毫秒数（事件时间窗与跟进点换算）。
const MS_PER_DAY: i64 = 86_400_000;
/// 一小时的毫秒数（选题冷却窗口换算）。
const MS_PER_HOUR: i64 = 3_600_000;

// =========================================================
// 候选与选题器
// =========================================================

/// 算法层选题候选（来源 / 落点 / 打分统一形态）。
///
/// 字段约定:
/// - `source`: 来源标识（`event` / `unresolved` / `time_node` / `rule` / `light_touch`）。
/// - `topic_key`: 来源内稳定标识（事件 / 规则 id 的文本形态，或轻触达键）。
/// - `session_id`: 目标会话（None = 问候类新建）。
/// - `anchor`: 情境摘要（None = 轻触达无具体话题）。
/// - `confidence`: 事件置信度（规则与轻触达为 None，不参与事件打分公式）。
/// - `salience` / `valence`: 事件信号原值（非事件候选不参与事件打分）。
/// - `score`: 最终排序分（事件类候选由打分层计算，规则候选直取路由得分）。
pub(super) struct TopicCandidate {
    pub(super) source: &'static str,
    pub(super) topic_key: String,
    pub(super) session_id: Option<Uuid>,
    pub(super) anchor: Option<String>,
    pub(super) confidence: Option<f64>,
    pub(super) salience: f64,
    pub(super) valence: f64,
    pub(super) score: f64,
}

/// 真实选题器：四源候选 + 安全网 + AI 判据（无状态，可共享）。
pub(crate) struct PickerTopicProvider;

#[async_trait]
impl TopicPicker for PickerTopicProvider {
    async fn pick(
        &self,
        engine: &Engine,
        persona: &str,
        now: i64,
        state: &mut ProactiveState,
        activity_weight: f64,
    ) -> Option<ProactiveDirective> {
        let config = engine.config();
        let proactive = &config.proactive;
        let storage = engine.storage_ref().as_ref();

        // ---- 素材加载：近窗事件 / 高显著事件 / 事件会话映射 ----
        let since = now.saturating_sub(proactive.event_window_days as i64 * MS_PER_DAY);
        let window_events = load_window_events(storage, persona, since).await;
        let salient_events =
            load_salient_events(storage, persona, since, proactive.event_salience_threshold).await;
        let session_map = load_session_map(storage, &window_events, &salient_events).await;

        // ---- 四源候选（构建顺序即去重优先级）----
        let mut candidates: Vec<TopicCandidate> = Vec::new();
        let mut claimed: std::collections::HashSet<i64> = std::collections::HashSet::new();
        candidates.extend(
            build_unresolved_candidates(
                storage,
                &window_events,
                &session_map,
                proactive,
                &mut claimed,
            )
            .await,
        );
        candidates.extend(build_time_node_candidates(
            &window_events,
            &session_map,
            now,
            proactive,
            &mut claimed,
        ));
        candidates.extend(build_salient_candidates(
            &salient_events,
            &session_map,
            proactive,
            &mut claimed,
        ));
        if let Some(rule_candidate) =
            build_rule_candidate(engine, persona, &window_events, &config.behavior).await
        {
            candidates.push(rule_candidate);
        }

        // ---- 打分层（事件类候选：置信度门槛后折扣）----
        apply_event_scores(&mut candidates, proactive);

        // ---- 过滤链：选题冷却 → 负效价出口兜底 → 不连选 ----
        apply_filters(&mut candidates, state, now, proactive);

        // ---- 排序（得分降序，同分按键升序保证确定性）----
        sort_candidates(&mut candidates);

        // ---- 轻触达兜底：四源皆无候选且不在冷却窗口 ----
        if candidates.is_empty() {
            match light_touch_candidate(state, now, proactive) {
                Some(candidate) => candidates.push(candidate),
                None => {
                    debug!(
                        persona,
                        "主动选题：四源无候选且轻触达在冷却窗口内，本轮无题"
                    );
                    return None;
                }
            }
        }

        // ---- 决策：判据裁决或算法直取 ----
        let directive = decide(engine, persona, now, state, activity_weight, &candidates).await?;
        info!(
            persona,
            candidates = candidates.len(),
            source = %directive.source,
            "主动选题完成"
        );
        Some(directive)
    }
}

// =========================================================
// 打分与过滤
// =========================================================

/// 事件类候选打分：置信度低于门槛剔除；通过者按「置信度 ×（显著性 + 效价强度入权）」折扣。
fn apply_event_scores(candidates: &mut Vec<TopicCandidate>, config: &ProactiveConfig) {
    candidates.retain_mut(|candidate| {
        let Some(confidence) = candidate.confidence else {
            // 规则候选得分已由路由给出
            return true;
        };
        if confidence < config.confidence_floor {
            return false;
        }
        candidate.score = (confidence
            * (candidate.salience + config.valence_weight * candidate.valence.abs()))
        .clamp(0.0, 2.0);
        true
    });
}

/// 统一过滤链：选题冷却 → 负效价出口兜底 → 不连选。
fn apply_filters(
    candidates: &mut Vec<TopicCandidate>,
    state: &ProactiveState,
    now: i64,
    config: &ProactiveConfig,
) {
    let cooldown_ms = config.topic_cooldown_hours as i64 * MS_PER_HOUR;
    candidates.retain(|candidate| {
        let cooling = state.recent_topics.iter().any(|recent| {
            recent.source == candidate.source
                && recent.key == candidate.topic_key
                && now.saturating_sub(recent.sent_at) < cooldown_ms
        });
        if cooling {
            return false;
        }
        // 负效价出口兜底：强负效价仅经未了结源出口
        if candidate.valence < config.unresolved_valence_threshold
            && candidate.source != SOURCE_UNRESOLVED
        {
            return false;
        }
        // 不连选：上一投递为负效价时本轮不再选负效价（正效价不受限）
        if state.last_valence_sign == -1 && candidate.valence < config.unresolved_valence_threshold
        {
            return false;
        }
        true
    });
}

/// 候选排序：得分降序，同分按选题键升序（确定性）。
fn sort_candidates(candidates: &mut [TopicCandidate]) {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.topic_key.cmp(&b.topic_key))
    });
}

/// 轻触达兜底候选：冷却窗口内返回 None（保持沉默，不硬凑话题）。
fn light_touch_candidate(
    state: &ProactiveState,
    now: i64,
    config: &ProactiveConfig,
) -> Option<TopicCandidate> {
    let cooldown_ms = config.topic_cooldown_hours as i64 * MS_PER_HOUR;
    let cooling = state.recent_topics.iter().any(|recent| {
        recent.source == SOURCE_LIGHT_TOUCH
            && recent.key == LIGHT_TOUCH_TOPIC_KEY
            && now.saturating_sub(recent.sent_at) < cooldown_ms
    });
    if cooling {
        return None;
    }
    Some(TopicCandidate {
        source: SOURCE_LIGHT_TOUCH,
        topic_key: LIGHT_TOUCH_TOPIC_KEY.to_string(),
        session_id: None,
        anchor: None,
        confidence: None,
        salience: 0.0,
        valence: 0.0,
        score: config.light_touch_weight,
    })
}

// =========================================================
// 决策
// =========================================================

/// 从排序后的候选产出最终指令。
///
/// 口径:
/// - 判据开启：候选编号 `c0..cn` 透传（锚点 / 效价 / 来源），由 AI 裁决开口与选题；
///   开口计 `judge_yes_count`、沉默计 `judge_no_count`、失败不计数；
/// - 判据关闭（消融路径）：按算法排序直取第一名，不调用 LLM。
async fn decide(
    engine: &Engine,
    persona: &str,
    now: i64,
    state: &mut ProactiveState,
    activity_weight: f64,
    candidates: &[TopicCandidate],
) -> Option<ProactiveDirective> {
    if engine.config().proactive.judge_enabled {
        let signals = JudgeSignals {
            hour: state::local_hour(now),
            activity_weight,
            hours_since_last_chat: hours_since_last_chat(engine, persona, now).await,
            silence_streak: state.silence_streak,
            daily_count: state.daily_count,
        };
        let judge_candidates: Vec<JudgeCandidate> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| JudgeCandidate {
                id: format!("{JUDGE_CANDIDATE_ID_PREFIX}{index}"),
                source: candidate.source.to_string(),
                anchor: candidate.anchor.clone(),
                valence: candidate.valence,
            })
            .collect();
        return match judge::decide(engine, &judge_candidates, &signals).await {
            JudgeOutcome::Speak(decision) => {
                state.judge_yes_count = state.judge_yes_count.saturating_add(1);
                let index = decision
                    .candidate_id
                    .strip_prefix(JUDGE_CANDIDATE_ID_PREFIX)
                    .and_then(|number| number.parse::<usize>().ok());
                let Some(candidate) = index.and_then(|index| candidates.get(index)) else {
                    warn!(persona, "主动选题：判据编号无法回引候选，本轮放弃");
                    return None;
                };
                Some(directive_from(
                    candidate,
                    persona,
                    decision.angle,
                    decision.tone,
                ))
            }
            JudgeOutcome::Silent => {
                state.judge_no_count = state.judge_no_count.saturating_add(1);
                debug!(persona, "主动选题：判据裁决沉默，本轮不主动");
                None
            }
            JudgeOutcome::Failed => None,
        };
    }

    // 判据关闭（消融路径）：按算法排序直取第一名
    candidates
        .first()
        .map(|candidate| directive_from(candidate, persona, None, None))
}

/// 距上次对话小时数（None = 无历史；查询失败按无历史降级）。
async fn hours_since_last_chat(engine: &Engine, persona: &str, now: i64) -> Option<f64> {
    match engine
        .storage_ref()
        .as_ref()
        .last_message_time_by_persona(persona)
        .await
    {
        Ok(Some(last)) => Some(now.saturating_sub(last) as f64 / MS_PER_HOUR as f64),
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, "主动选题：最近消息时间查询失败，判据按无历史处理");
            None
        }
    }
}

/// 候选 → 主动生成指令（判据角度 / 语气可为空）。
fn directive_from(
    candidate: &TopicCandidate,
    persona: &str,
    angle: Option<String>,
    tone: Option<String>,
) -> ProactiveDirective {
    ProactiveDirective {
        persona: persona.to_string(),
        session_id: candidate.session_id,
        source: candidate.source.to_string(),
        topic_key: Some(candidate.topic_key.clone()),
        anchor: candidate.anchor.clone(),
        angle,
        tone,
        valence: candidate.valence,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
