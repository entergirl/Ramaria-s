//! crates/ramaria-service/src/recall.rs - 召回用例（memory_recall 的服务层实现）
//!
//! 设计特点:
//! - 召回同源：记忆层检索走 `ramaria_memory::recall::assemble_recall`（与在线管线同一份
//!   实现），服务层只负责"分层装配 + 预算裁剪 + 结构化输出"
//! - 分层装配按注入优先级排列：行为 > 知识 > 表达（风格） > 脉络 > 记忆（L1/L2/L3） > 原文
//! - 两种模式：`Search`（有检索输入）与 `Overview`（无 query 且无 messages，按时间线概览）
//! - 隐私策略在服务层执行：人格白名单（`allowed_personas`）与原文开关（`allow_raw_text`）
//!   不满足时直接拒绝或不出原文，保证「策略执行」不依赖协议壳
//! - 预算纪律：`max_items` 上限 20（超出截断）、`max_chars` 按字符边界截断，均计入 stats
//! - 静默降级：知识 / 风格 / 画像 / 行为读取失败均记 warn 并按空处理，不阻塞召回
//!
//! 未接线说明:
//! - `RecallRequest.conversation_id`（"当前外部对话库内历史参与检索去重"）本期仅作数据属性
//!   透传，尚未参与检索去重；接线点见开发计划 §5.2 与 `list_messages_by_channel_ref`。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{PersonaFact, PersonalityTrait, StyleStatsStatus, now_ms};
use ramaria_memory::prompt::builder::build_cross_session_narrative;
use ramaria_memory::recall::{RecallGates, RecallInput, RecallMemoryLayers, assemble_recall};

use crate::engine::Engine;
use crate::types::{
    DEFAULT_PERSONA_UID, RecallItem, RecallLayer, RecallMode, RecallRequest, RecallResult,
    RecallStats,
};

/// 单层最多返回的条目数（辅助层：行为 / 知识 / 风格 / 画像 / 脉络）。
///
/// 说明:
/// - 记忆层条目上限由 `[retrieval].rag_max_memories` 控制（共用实现内截断）；
/// - 辅助层用固定上界避免单层挤占预算（最终仍受 `max_items` 总预算约束）。
const MAX_AUX_LAYER_ITEMS: usize = 5;

/// 脉络层读取的近期摘要条数（跨会话叙事素材）。
const NARRATIVE_RECENT_L1: u32 = 5;

// =========================================================
// 召回策略（入口层注入）
// =========================================================

/// 召回隐私与边界策略。
///
/// 职责:
/// - 承载「谁可见 / 原文是否出端 / 默认预算」这三类由配置决定的边界，
///   在服务层强制执行（协议壳只做参数校验，不承担隐私判断）。
///
/// 字段约定:
/// - `allow_raw_text`: 是否允许返回 utt 原文块（装配缺省按配置闸门映射，见
///   [`RecallPolicy::from_config`]；显式注入的保守值为 false —— 原文是最高敏感层）；
/// - `allowed_personas`: 可见人格白名单，`["*"]` 表示全部可见。
#[derive(Debug, Clone, PartialEq)]
pub struct RecallPolicy {
    pub allow_raw_text: bool,
    pub allowed_personas: Vec<String>,
}

impl Default for RecallPolicy {
    /// 默认最保守：原文不出端、全部人格可见（由用户后续收紧）。
    fn default() -> Self {
        Self {
            allow_raw_text: false,
            allowed_personas: vec!["*".to_string()],
        }
    }
}

impl RecallPolicy {
    /// 从配置映射缺省策略（装配层缺省口径）。
    ///
    /// 职责:
    /// - 把配置闸门映射为服务层缺省策略：`allow_raw_text = [injection].utt × [utt].enabled`；
    /// - 人格白名单缺省不收紧（`["*"]` 全部可见），由宿主按需注入。
    ///
    /// 参数:
    /// - `config`: 当前生效配置。
    ///
    /// 返回:
    /// - 原文开关与配置闸门一致的策略快照。
    ///
    /// 说明:
    /// - 宿主可在装配后经 `Engine::set_recall_policy` 覆盖收紧（如关闭原文 / 限制人格）；
    ///   显式注入方不受缺省影响（注入值整体替换）。
    pub fn from_config(config: &RamariaConfig) -> Self {
        Self {
            allow_raw_text: config.injection.utt && config.utt.enabled,
            allowed_personas: vec!["*".to_string()],
        }
    }

    /// 链式设置原文开关。
    pub fn with_allow_raw_text(mut self, allow: bool) -> Self {
        self.allow_raw_text = allow;
        self
    }

    /// 链式设置人格白名单（空列表按 `["*"]` 处理，避免"空即全禁"的误配）。
    pub fn with_allowed_personas(mut self, personas: Vec<String>) -> Self {
        self.allowed_personas = if personas.is_empty() {
            vec!["*".to_string()]
        } else {
            personas
        };
        self
    }

    /// 判定某人格是否可见。
    ///
    /// 参数:
    /// - `persona_uid`: 待判定人格。
    ///
    /// 返回:
    /// - `true`: 白名单含 `*` 或该人格。
    pub fn persona_allowed(&self, persona_uid: &str) -> bool {
        self.allowed_personas
            .iter()
            .any(|rule| rule == "*" || rule == persona_uid)
    }
}

// =========================================================
// 召回用例入口
// =========================================================

/// 执行召回用例。
///
/// 流程:
/// 1. 策略校验：人格白名单不通过 → `Privacy` 错误（越权可见性拒绝）；
/// 2. 归一化请求（分层 / 上限 / 预算）；
/// 3. 检索输入判定：显式 `query` > 最后一条用户消息 → 检索模式；
///    两者皆空 → 概览模式（时间线返回最近记忆）；
/// 4. 分层装配（记忆层走共用召回实现，其余层按层读取并渲染）；
/// 5. 预算裁剪（context 字符预算 + items 条数上限）与 stats 汇总。
///
/// 参数:
/// - `engine`: 服务层引擎（存储 / 配置 / 检索槽 / 策略）。
/// - `req`: 召回请求（对话片段 / 人格 / 分层 / 预算）。
///
/// 返回:
/// - 成功时返回 `context`（可直接拼接的段落文本）、`items`（结构化明细）与 `stats`。
pub(crate) async fn run(engine: &Engine, req: RecallRequest) -> RamariaResult<RecallResult> {
    let policy = engine.recall_policy();
    let persona = normalize_persona(req.persona.as_deref());
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "召回请求的人格不在可见白名单内，拒绝");
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可见白名单内（allowed_personas）"
        )));
    }

    let include = req.effective_include();
    let max_items = req.effective_max_items() as usize;
    let max_chars = req.effective_max_chars() as usize;

    let query = resolve_query(&req);
    if query.trim().is_empty() {
        tracing::debug!(persona = %persona, "召回无检索输入，进入概览模式");
        return overview(engine, &persona, &include, max_items, max_chars).await;
    }

    tracing::debug!(
        persona = %persona,
        layers = include.len(),
        max_items,
        max_chars,
        "召回检索模式开始"
    );
    search(
        engine, &policy, &persona, &include, &query, max_items, max_chars,
    )
    .await
}

// =========================================================
// 检索模式
// =========================================================

/// 检索模式：记忆层共用召回 + 其余分层装配。
#[allow(clippy::too_many_arguments)]
async fn search(
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
    let memory_output = assemble_recall(RecallInput {
        retriever: &**engine.retriever_slot(),
        keyword_mirror: &**engine.keyword_mirror_ref(),
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
async fn overview(
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

// =========================================================
// 各分层读取与渲染
// =========================================================

/// 行为层：渲染启用的行为规则（情境 → 反应）。
///
/// 降级:
/// - `[behavior].enabled=false` / 无启用规则 / 读取失败 → 空（不注入）。
async fn behavior_layer(engine: &Engine, persona: &str) -> (Option<String>, Vec<RecallItem>) {
    if !engine.config().behavior.enabled {
        return (None, Vec::new());
    }
    let rules = match engine
        .storage_ref()
        .list_behavior_rules_by_persona(persona)
        .await
    {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(persona, error = %e, "行为层读取失败，本次不注入");
            return (None, Vec::new());
        }
    };

    let active: Vec<_> = rules
        .iter()
        .filter(|rule| rule.enabled)
        .filter(|rule| {
            rule.reaction
                .as_deref()
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false)
        })
        .take(MAX_AUX_LAYER_ITEMS)
        .collect();
    if active.is_empty() {
        return (None, Vec::new());
    }

    let mut lines = vec!["# 行为（行为层）".to_string()];
    let mut items = Vec::with_capacity(active.len());
    for rule in active {
        let situation = rule.situation.keywords.join("、");
        let reaction = rule.reaction.clone().unwrap_or_default();
        lines.push(format!("- 情境「{situation}」→ {reaction}"));
        items.push(RecallItem {
            layer: RecallLayer::Behavior,
            id: rule.id.to_string(),
            text: reaction,
            score: Some(rule.confidence),
            time: iso_time(rule.updated_at),
        });
    }
    (Some(lines.join("\n")), items)
}

/// 知识层：判定器命中后渲染知识卡片。
///
/// 降级:
/// - 判定器关闭 / 未命中 / 读取失败 → 空（不注入）。
async fn knowledge_layer(
    engine: &Engine,
    persona: &str,
    query: &str,
) -> (Option<String>, Vec<RecallItem>) {
    let config = engine.config().knowledge.clone();
    let facts = ramaria_memory::fact::retriever::load_knowledge_facts_for_query(
        engine.storage_ref().as_ref(),
        &config,
        persona,
        query,
    )
    .await;
    if facts.is_empty() {
        return (None, Vec::new());
    }

    let cards = ramaria_memory::fact::retriever::render_knowledge_cards(&facts);
    if cards.trim().is_empty() {
        return (None, Vec::new());
    }
    let text = format!("# 知识（知识层）\n{cards}");
    let items = facts.iter().map(fact_item).collect();
    (Some(text), items)
}

/// 概览用的知识摘要条目（不做判定器命中判断，直接取 active 事实前若干条）。
async fn knowledge_overview_items(
    engine: &Engine,
    persona: &str,
) -> (Option<String>, Vec<RecallItem>) {
    let facts = match engine
        .storage_ref()
        .list_active_facts_by_persona(persona)
        .await
    {
        Ok(facts) => facts,
        Err(e) => {
            tracing::warn!(persona, error = %e, "概览：读取 active 事实失败，跳过");
            return (None, Vec::new());
        }
    };
    let items: Vec<RecallItem> = facts
        .iter()
        .take(MAX_AUX_LAYER_ITEMS)
        .map(fact_item)
        .collect();
    (None, items)
}

/// 事实 → 结构化条目（渲染为 `字段：内容` 文本）。
fn fact_item(fact: &PersonaFact) -> RecallItem {
    let time = if fact.updated_at > 0 {
        fact.updated_at
    } else {
        fact.created_at
    };
    RecallItem {
        layer: RecallLayer::Knowledge,
        id: fact.id.to_string(),
        text: format!("{}：{}", fact.field.label(), fact.content.trim()),
        score: Some(fact.confidence),
        time: iso_time(time),
    }
}

/// 表达层：自动风格规则（仅 `Ready` 状态注入）。
///
/// 降级:
/// - `[style].enabled=false` / 未统计 / 样本不足（非 Ready）/ 读取失败 → 空（不注入）。
async fn style_layer(engine: &Engine, persona: &str) -> (Option<String>, Vec<RecallItem>) {
    if !engine.config().style.enabled {
        return (None, Vec::new());
    }
    let stats = match engine.storage_ref().get_style_stats(persona).await {
        Ok(Some(stats)) => stats,
        Ok(None) => return (None, Vec::new()),
        Err(e) => {
            tracing::warn!(persona, error = %e, "表达层读取风格统计失败，本次不注入");
            return (None, Vec::new());
        }
    };
    if stats.status != StyleStatsStatus::Ready {
        return (None, Vec::new());
    }
    let Some(rule) = stats.rule_text.clone().filter(|t| !t.trim().is_empty()) else {
        return (None, Vec::new());
    };

    let text = format!("# 表达风格（表达层）\n- {rule}");
    let items = vec![RecallItem {
        layer: RecallLayer::Style,
        id: "style".to_string(),
        text: rule,
        score: None,
        time: iso_time(stats.updated_at),
    }];
    (Some(text), items)
}

/// 脉络层：近期 L1 摘要拼装跨会话叙事。
async fn narrative_layer(engine: &Engine, persona: &str) -> (Option<String>, Vec<RecallItem>) {
    let recent = match engine
        .storage_ref()
        .list_recent_l1_by_persona(persona, NARRATIVE_RECENT_L1)
        .await
    {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(persona, error = %e, "脉络层读取近期摘要失败，本次不注入");
            return (None, Vec::new());
        }
    };
    if recent.is_empty() {
        return (None, Vec::new());
    }

    let summaries: Vec<String> = recent.iter().map(|l1| l1.summary.clone()).collect();
    let narrative = build_cross_session_narrative(&summaries);
    if narrative.trim().is_empty() {
        return (None, Vec::new());
    }

    // 脉络层只贡献叙事段落，不产出结构化条目：
    // 其素材（近期 L1）已由记忆层以带分数与时间的条目返回，此处再列会重复占用 items 预算。
    (Some(narrative), Vec::new())
}

/// 画像层（L3）：性格标签。
async fn trait_layer(engine: &Engine, persona: &str) -> (Option<String>, Vec<RecallItem>) {
    let traits = match engine.storage_ref().list_traits_by_persona(persona).await {
        Ok(traits) => traits,
        Err(e) => {
            tracing::warn!(persona, error = %e, "画像层读取性格标签失败，本次不注入");
            return (None, Vec::new());
        }
    };
    let active: Vec<_> = traits
        .iter()
        .filter(|t| t.status == ramaria_core::types::TraitStatus::Active)
        .take(MAX_AUX_LAYER_ITEMS)
        .collect();
    if active.is_empty() {
        return (None, Vec::new());
    }

    let mut lines = vec!["# 性格画像（L3）".to_string()];
    let mut items = Vec::with_capacity(active.len());
    for t in active {
        lines.push(format!("- {}：{}", t.trait_label, t.meaning));
        items.push(trait_item(t));
    }
    (Some(lines.join("\n")), items)
}

/// 性格标签 → 结构化条目。
fn trait_item(t: &PersonalityTrait) -> RecallItem {
    RecallItem {
        layer: RecallLayer::L3,
        id: t.id.to_string(),
        text: format!("{}：{}", t.trait_label, t.meaning),
        score: Some(t.confidence),
        time: iso_time(t.updated_at),
    }
}

// =========================================================
// 辅助
// =========================================================

/// 归一化人格 uid（空串视为缺省）。
fn normalize_persona(persona: Option<&str>) -> String {
    persona
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_PERSONA_UID)
        .to_string()
}

/// 解析检索输入：显式 `query` 优先，其次最后一条用户消息（与在线管线口径一致）。
fn resolve_query(req: &RecallRequest) -> String {
    if let Some(query) = req.query.as_deref().map(str::trim) {
        if !query.is_empty() {
            return query.to_string();
        }
    }
    req.messages
        .iter()
        .rev()
        .find(|turn| turn.role == crate::types::ChatRole::User)
        .map(|turn| turn.content.trim().to_string())
        .or_else(|| {
            req.messages
                .last()
                .map(|turn| turn.content.trim().to_string())
        })
        .unwrap_or_default()
}

/// 检索命中分层字符串 → 召回分层枚举。
fn layer_from_hit(layer: &str) -> Option<RecallLayer> {
    match layer {
        "l1" => Some(RecallLayer::L1),
        "l2" => Some(RecallLayer::L2),
        _ => None,
    }
}

/// Unix 毫秒 → UTC 时间（非法值返回 None，不报错）。
fn iso_time(ms: i64) -> Option<DateTime<Utc>> {
    if ms <= 0 {
        return None;
    }
    DateTime::from_timestamp_millis(ms)
}

/// 按字符边界截断文本（超出部分丢弃，保证 UTF-8 完整）。
fn truncate_chars(text: &str, max_chars: usize) -> String {
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

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        MockLlm, engine_with_db, engine_with_llm_and_config, seed_l1 as seed_l1_raw, seed_persona,
    };
    use crate::types::{ChatRole, ChatTurn};
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{FactSource, FactTier, ProfileField};
    use ramaria_storage::SqliteStorage;
    use uuid::Uuid;

    /// 造一条 L1（默认不带关键词；BM25 直接命中摘要文本）。
    async fn seed_l1(
        storage: &SqliteStorage,
        persona: &str,
        summary: &str,
        created_at: i64,
    ) -> Uuid {
        seed_l1_raw(storage, persona, summary, None, created_at).await
    }

    // ---- 策略 ----

    #[test]
    fn policy_default_is_conservative() {
        let policy = RecallPolicy::default();
        assert!(!policy.allow_raw_text, "默认不返回原文块");
        assert!(policy.persona_allowed("char-0001"), "默认全部人格可见");
    }

    /// 配置映射：原文开关 = `[injection].utt` × `[utt].enabled`；白名单缺省不收紧。
    #[test]
    fn policy_from_config_maps_utt_gates() {
        let config = RamariaConfig::default();
        let policy = RecallPolicy::from_config(&config);
        assert!(policy.allow_raw_text, "默认配置：两闸门开启 → 原文层开放");
        assert_eq!(
            policy.allowed_personas,
            vec!["*".to_string()],
            "缺省不收紧人格白名单"
        );

        let mut injection_off = RamariaConfig::default();
        injection_off.injection.utt = false;
        assert!(
            !RecallPolicy::from_config(&injection_off).allow_raw_text,
            "注入闸门关闭 → 原文层关闭"
        );

        let mut utt_off = RamariaConfig::default();
        utt_off.utt.enabled = false;
        assert!(
            !RecallPolicy::from_config(&utt_off).allow_raw_text,
            "utt 链路关闭 → 原文层关闭"
        );
    }

    /// Engine 装配缺省：策略随配置映射（默认配置开放原文；配置关闭 utt 则关闭）。
    #[tokio::test]
    async fn engine_default_policy_follows_config() {
        let (engine, _storage, dir) = engine_with_llm_and_config(
            "policy-default",
            MockLlm::local(),
            RamariaConfig::default(),
        )
        .await;
        assert!(
            engine.recall_policy().allow_raw_text,
            "默认配置装配 → 原文层开放"
        );
        let _ = std::fs::remove_dir_all(&dir);

        let mut utt_off = RamariaConfig::default();
        utt_off.utt.enabled = false;
        let (engine, _storage, dir) =
            engine_with_llm_and_config("policy-utt-off", MockLlm::local(), utt_off).await;
        assert!(
            !engine.recall_policy().allow_raw_text,
            "配置关闭 utt → 原文层关闭"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 宿主覆盖优先：`set_recall_policy` 注入值整体替换装配缺省。
    #[tokio::test]
    async fn engine_policy_override_beats_default() {
        let (engine, _storage, dir) = engine_with_db("policy-override").await;
        assert!(
            engine.recall_policy().allow_raw_text,
            "装配缺省为配置映射（默认配置开放原文）"
        );

        engine.set_recall_policy(RecallPolicy::default());
        assert!(
            !engine.recall_policy().allow_raw_text,
            "注入的保守策略覆盖装配缺省"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn policy_persona_whitelist() {
        let policy = RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]);
        assert!(policy.persona_allowed("char-0001"));
        assert!(!policy.persona_allowed("char-0002"));
        // 空列表兜底为全可见（避免"空即全禁"的误配）
        let all = RecallPolicy::default().with_allowed_personas(Vec::new());
        assert!(all.persona_allowed("char-0002"));
    }

    // ---- 检索输入解析 ----

    #[test]
    fn query_resolution_prefers_explicit_then_last_user_turn() {
        let req = RecallRequest {
            query: Some(" 项目进度 ".to_string()),
            messages: vec![ChatTurn {
                role: ChatRole::User,
                content: "另一句".to_string(),
            }],
            ..RecallRequest::default()
        };
        assert_eq!(resolve_query(&req), "项目进度");

        let req = RecallRequest {
            query: None,
            messages: vec![
                ChatTurn {
                    role: ChatRole::User,
                    content: "第一句".to_string(),
                },
                ChatTurn {
                    role: ChatRole::Assistant,
                    content: "回复".to_string(),
                },
                ChatTurn {
                    role: ChatRole::User,
                    content: "最后一句".to_string(),
                },
            ],
            ..RecallRequest::default()
        };
        assert_eq!(resolve_query(&req), "最后一句", "取最后一条用户消息");

        let empty = RecallRequest {
            query: Some("   ".to_string()),
            ..RecallRequest::default()
        };
        assert!(resolve_query(&empty).is_empty(), "空白 query 视为无输入");
    }

    // ---- 检索模式 ----

    /// 检索模式：命中 L1 → context 含摘要、items 结构完整、stats.mode = search。
    #[tokio::test]
    async fn search_mode_returns_context_and_items() {
        let (engine, storage, dir) = engine_with_db("search").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大，常常加班",
            1_000,
        )
        .await;
        engine.ensure_index_loaded().await.expect("索引加载");

        let result = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert_eq!(result.stats.mode, RecallMode::Search);
        assert!(
            result.context.contains("工作压力"),
            "上下文应含命中摘要: {}",
            result.context
        );
        assert!(!result.items.is_empty(), "应返回结构化条目");
        let item = &result.items[0];
        assert_eq!(item.layer, RecallLayer::L1);
        assert!(item.score.is_some(), "检索条目应带融合分");
        assert!(item.time.is_some(), "检索条目应带时间");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分层开关：include 只含 knowledge 时不返回记忆层条目（也不触发检索）。
    #[tokio::test]
    async fn include_filters_layers() {
        let (engine, storage, dir) = engine_with_db("layers").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "用户最近工作压力很大", 1_000).await;

        let result = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::Knowledge]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert!(
            result
                .items
                .iter()
                .all(|i| i.layer == RecallLayer::Knowledge),
            "仅请求知识层时不应返回记忆层条目: {:?}",
            result.items
        );
        assert!(!result.context.contains("[相关记忆]"), "记忆段落未请求");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分层开关（摘要路子层）：只请求 L2 时不返回 L1 条目与 L1 文本（反向亦然）。
    ///
    /// 回归背景: 早期实现把 L1/L2 当"整层"处理，include=[L2] 仍会带出 L1 条目与正文，
    /// 违反 T-V21-2-002「分层开关生效」。
    #[tokio::test]
    async fn memory_sublayer_switch_is_honoured() {
        let (engine, storage, dir) = engine_with_db("sublayers").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1_raw(
            &storage,
            "char-0001",
            "用户提到工作压力（摘要侧）",
            Some("工作压力"),
            1_000,
        )
        .await;
        // 同关键词的 L2 事件（确保两层都能被同一查询命中）
        let mut event = ramaria_core::types::MemoryEvent::new(
            "char-0001".to_string(),
            "工作压力事件（事件侧）".to_string(),
            "群聊里被点名批评".to_string(),
            1_000,
            2_000,
        );
        event.keywords = Some("工作压力".to_string());
        event.share = 1.0;
        event.confidence = 0.9;
        storage.save_event(&event).await.expect("写入事件");
        engine.ensure_index_loaded().await.expect("索引加载");

        // 只请求 L2：无 L1 条目、段落不含摘要侧文本
        let l2_only = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L2]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(
            l2_only
                .items
                .iter()
                .all(|item| item.layer == RecallLayer::L2),
            "仅请求 L2 时不应出现其它分层: {:?}",
            l2_only.items
        );
        assert!(!l2_only.items.is_empty(), "L2 应命中");
        assert!(
            !l2_only.context.contains("摘要侧"),
            "段落不应含 L1 文本: {}",
            l2_only.context
        );

        // 只请求 L1：无 L2 条目、段落不含事件侧文本
        let l1_only = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(
            l1_only
                .items
                .iter()
                .all(|item| item.layer == RecallLayer::L1),
            "仅请求 L1 时不应出现其它分层: {:?}",
            l1_only.items
        );
        assert!(!l1_only.items.is_empty(), "L1 应命中");
        assert!(
            !l1_only.context.contains("事件侧"),
            "段落不应含 L2 文本: {}",
            l1_only.context
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预算裁剪：max_chars 极小时 context 被截断且 stats.truncated = true。
    #[tokio::test]
    async fn budget_truncates_context() {
        let (engine, storage, dir) = engine_with_db("budget").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大，常常加班到深夜，反复提到职场焦虑",
            1_000,
        )
        .await;
        engine.ensure_index_loaded().await.expect("索引加载");

        let result = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                max_chars: Some(4),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert!(result.stats.truncated, "超预算应标记截断");
        assert!(
            result.context.chars().count() <= 4,
            "context 应在字符预算内: {}",
            result.context
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// max_items 上限：请求超过 20 时按硬边界截断（不 panic、不越界）。
    #[tokio::test]
    async fn max_items_clamped_to_hard_limit() {
        let (engine, storage, dir) = engine_with_db("items").await;
        seed_persona(&storage, "char-0001").await;
        for i in 0..3 {
            seed_l1(
                &storage,
                "char-0001",
                &format!("用户第{i}次提到工作压力"),
                1_000 + i,
            )
            .await;
        }
        engine.ensure_index_loaded().await.expect("索引加载");

        let result = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                max_items: Some(999),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert!(result.items.len() <= 20, "items 不得超过硬上限 20");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 隐私策略：人格不在白名单 → Privacy 错误（越权拒绝）。
    #[tokio::test]
    async fn persona_whitelist_rejects_others() {
        let (engine, storage, dir) = engine_with_db("policy").await;
        seed_persona(&storage, "char-0001").await;
        engine.set_recall_policy(
            RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
        );

        let err = engine
            .recall(RecallRequest {
                query: Some("任意".to_string()),
                persona: Some("char-0002".to_string()),
                ..RecallRequest::default()
            })
            .await
            .expect_err("白名单外人格应被拒绝");
        assert_eq!(err.category(), "privacy");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 原文层：策略关闭时即使请求 raw 也不返回原文块。
    #[tokio::test]
    async fn raw_layer_requires_policy_allow() {
        let (engine, storage, dir) = engine_with_db("raw").await;
        seed_persona(&storage, "char-0001").await;
        // 本用例验证"策略关闭"分支：显式注入保守策略（装配缺省由配置映射决定）
        engine.set_recall_policy(RecallPolicy::default());

        let result = engine
            .recall(RecallRequest {
                query: Some("任意".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::Raw]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert!(result.context.is_empty(), "策略关闭时原文层不产出段落");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 概览模式 ----

    /// 概览模式：无 query / messages → 时间倒序返回最近记忆。
    #[tokio::test]
    async fn overview_mode_returns_timeline() {
        let (engine, storage, dir) = engine_with_db("overview").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "较早的摘要内容", 1_000).await;
        seed_l1(&storage, "char-0001", "较新的摘要内容", 2_000).await;

        let result = engine
            .recall(RecallRequest {
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("概览成功");

        assert_eq!(result.stats.mode, RecallMode::Overview);
        assert!(result.context.contains("[记忆概览]"));
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].text, "较新的摘要内容", "应时间倒序");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 概览模式：空库 → 空结构（不报错）。
    #[tokio::test]
    async fn overview_mode_empty_db() {
        let (engine, _storage, dir) = engine_with_db("overview-empty").await;

        let result = engine
            .recall(RecallRequest::default())
            .await
            .expect("概览成功");

        assert_eq!(result.stats.mode, RecallMode::Overview);
        assert!(result.items.is_empty());
        assert!(result.context.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 概览条数上限：max_items 生效且指标标记截断。
    #[tokio::test]
    async fn overview_respects_max_items() {
        let (engine, storage, dir) = engine_with_db("overview-limit").await;
        seed_persona(&storage, "char-0001").await;
        for i in 0..4 {
            seed_l1(&storage, "char-0001", &format!("第{i}条摘要"), 1_000 + i).await;
        }

        let result = engine
            .recall(RecallRequest {
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                max_items: Some(2),
                ..RecallRequest::default()
            })
            .await
            .expect("概览成功");

        assert_eq!(result.items.len(), 2);
        assert!(result.stats.truncated);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 知识层与辅助条目 ----

    /// 知识层：命中判定 + 卡片渲染 + 条目结构（含事实 id 与置信度）。
    #[tokio::test]
    async fn knowledge_layer_renders_hit_facts() {
        let (engine, storage, dir) = engine_with_db("knowledge").await;
        seed_persona(&storage, "char-0001").await;
        let mut fact = ramaria_core::types::PersonaFact::new(
            "char-0001".to_string(),
            ProfileField::Interests,
            "喜欢露营和徒步".to_string(),
            FactSource::Manual,
        );
        fact.tier = FactTier::Stable;
        fact.keyword_hint = Some("露营,徒步".to_string());
        storage.save_fact(&fact).await.expect("写入事实");

        let result = engine
            .recall(RecallRequest {
                query: Some("你记得我喜欢露营吗".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::Knowledge]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert!(
            result.context.contains("露营"),
            "知识卡片应命中并渲染: {}",
            result.context
        );
        assert!(!result.items.is_empty());
        assert_eq!(result.items[0].layer, RecallLayer::Knowledge);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空白请求的归一化：query 与 messages 皆空 → 概览；messages 单条 → 检索。
    #[tokio::test]
    async fn messages_only_enters_search_mode() {
        let (engine, storage, dir) = engine_with_db("messages").await;
        seed_persona(&storage, "char-0001").await;

        let result = engine
            .recall(RecallRequest {
                persona: Some("char-0001".to_string()),
                messages: vec![ChatTurn {
                    role: ChatRole::User,
                    content: "今天想聊聊工作".to_string(),
                }],
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");

        assert_eq!(result.stats.mode, RecallMode::Search, "有对话片段应走检索");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
