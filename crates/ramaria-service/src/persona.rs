//! crates/ramaria-service/src/persona.rs - 人格读取与重生成用例（persona_list / persona_get / regenerate_import_l1）
//!
//! 设计特点:
//! - 人格读取：人格摘要列表与人格卡片（性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度）
//! - 人格重生成：导入失败后的离线重建路径（全量消息枚举 → 会话去重 → 逐会话 L1 重生成，含连续失败早停）
//! - 严格按 persona_uid 隔离：读取与重生成只处理目标人格的记录，不跨人格聚合
//! - 逐段独立降级：卡片任一段读取失败记 warn 并返回空段，不阻塞整张卡片
//! - 条目上限：卡片各段最多返回 [`MAX_CARD_ITEMS`] 条（避免大库把整张卡片撑爆）
//! - 隐私：卡片不含 utt 原文块；日志中的个人标识经 `mask_id` 脱敏

use std::collections::HashSet;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::types::{ProfileField, TraitStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{
    BehaviorRuleView, DataMaturityView, FactView, PersonaCardRequest, PersonaCardView,
    PersonaSection, PersonaSummaryView, StyleView, TraitView,
};

/// 人格卡片各段最多返回的条目数。
const MAX_CARD_ITEMS: usize = 20;

/// 数据成熟度计数查询上限（诊断用途，超量按上限计）。
const MATURITY_COUNT_LIMIT: u32 = 1_000;

// =========================================================
// persona_list
// =========================================================

/// 列出全部人格摘要（uid / 名称 / 类型 / 来源 / 简介 / 启用状态）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 按存储层顺序返回全部人格（含停用项，调用方可按 `active` 过滤）。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<Vec<PersonaSummaryView>> {
    let personas = engine.storage_ref().list_personas().await?;
    let views = personas
        .into_iter()
        .map(|p| PersonaSummaryView {
            uid: p.uid,
            name: p.name,
            kind: p.kind,
            source: p.source,
            description: p.description,
            active: p.active,
        })
        .collect();
    Ok(views)
}

// =========================================================
// persona_get（人格卡片）
// =========================================================

/// 组装人格卡片（按 `sections` 选择分段；缺省全部分段）。
///
/// 流程:
/// 1. 读取人格行（不存在 → Validation 错误，错误可见）；
/// 2. 按分段读取关联数据（每段独立降级，失败记 warn 并置空）；
/// 3. 组装视图（各段受 [`MAX_CARD_ITEMS`] 截断；成熟度计数为全量计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 卡片请求（uid 必填；sections 可选）。
///
/// 返回:
/// - 成功时返回人格卡片视图；人格不存在时返回 `Validation` 错误。
pub(crate) async fn card(
    engine: &Engine,
    req: PersonaCardRequest,
) -> RamariaResult<PersonaCardView> {
    let storage = engine.storage_ref();
    let uid = req.uid.trim().to_string();
    if uid.is_empty() {
        return Err(RamariaError::validation("uid 不能为空"));
    }

    let persona = match storage.get_persona_by_uid(&uid).await? {
        Some(persona) => persona,
        None => {
            return Err(RamariaError::validation(format!("人格不存在: {uid}")));
        }
    };

    let sections = req.effective_sections();
    let wants = |section: PersonaSection| sections.contains(&section);

    // ---- 性格画像（L3 三层标签，仅 Active 参与展示） ----
    let traits = if wants(PersonaSection::Traits) || wants(PersonaSection::Maturity) {
        match storage.list_traits_by_persona(&uid).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取性格标签失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let trait_views: Vec<TraitView> = if wants(PersonaSection::Traits) {
        traits
            .iter()
            .filter(|t| t.status == TraitStatus::Active)
            .take(MAX_CARD_ITEMS)
            .map(|t| TraitView {
                layer: t.layer,
                label: t.trait_label.clone(),
                meaning: t.meaning.clone(),
                trigger: t.trigger.clone(),
                confidence: t.confidence,
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- 行为规则（含停用项，`enabled` 字段供调用方判断） ----
    let behaviors: Vec<BehaviorRuleView> = if wants(PersonaSection::Behaviors) {
        match storage.list_behavior_rules_by_persona(&uid).await {
            Ok(rules) => rules
                .iter()
                .take(MAX_CARD_ITEMS)
                .map(|rule| BehaviorRuleView {
                    id: rule.id,
                    situation: rule.situation.keywords.join("、"),
                    reaction: rule.reaction.clone(),
                    avoid: rule.avoid.clone(),
                    confidence: rule.confidence,
                    enabled: rule.enabled,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取行为规则失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // ---- 表达风格（自动风格规则，仅 Ready 有文本） ----
    let style = if wants(PersonaSection::Style) {
        match storage.get_style_stats(&uid).await {
            Ok(Some(stats)) => Some(StyleView {
                rule_text: stats.rule_text.clone().filter(|t| !t.trim().is_empty()),
                status: stats.status,
                sample_count: stats.sample_count,
            }),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取风格统计失败，卡片该段为空");
                None
            }
        }
    } else {
        None
    };

    // ---- 知识事实（active；SpeakingStyle 由表达层单独呈现，此处不重复） ----
    let facts = if wants(PersonaSection::Facts) || wants(PersonaSection::Maturity) {
        match storage.list_active_facts_by_persona(&uid).await {
            Ok(facts) => facts,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取知识事实失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let fact_views: Vec<FactView> = if wants(PersonaSection::Facts) {
        facts
            .iter()
            .take(MAX_CARD_ITEMS)
            .map(|f| FactView {
                field: f.field,
                content: f.content.clone(),
                tier: f.tier,
                confidence: f.confidence,
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- 数据成熟度（各层数据量计数；诊断用，受查询上限约束） ----
    let maturity = if wants(PersonaSection::Maturity) {
        maturity_view(engine, &uid, &traits, &facts).await
    } else {
        DataMaturityView::default()
    };

    tracing::debug!(
        uid = %uid,
        traits = trait_views.len(),
        behaviors = behaviors.len(),
        facts = fact_views.len(),
        "人格卡片已组装"
    );

    Ok(PersonaCardView {
        uid: persona.uid,
        name: persona.name,
        kind: persona.kind,
        source: persona.source,
        description: persona.description,
        active: persona.active,
        traits: trait_views,
        behaviors,
        style,
        facts: fact_views,
        maturity,
    })
}

/// 组装数据成熟度视图（各层数据量计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标人格。
/// - `traits` / `facts`: 已读取的画像数据（复用，避免重复查询）。
///
/// 返回:
/// - 计数视图；单项查询失败记 warn 并按 0 计（不阻塞卡片）。
async fn maturity_view(
    engine: &Engine,
    uid: &str,
    traits: &[ramaria_core::types::PersonalityTrait],
    facts: &[ramaria_core::types::PersonaFact],
) -> DataMaturityView {
    let storage = engine.storage_ref();

    // L1 摘要条数（按 persona 取最近 N 条计数，超量按上限计）
    let l1_count = match storage
        .list_recent_l1_by_persona(uid, MATURITY_COUNT_LIMIT)
        .await
    {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取 L1 计数失败，按 0 计");
            0
        }
    };

    // L2 事件条数
    let event_count = match storage
        .list_events_by_persona(uid, 0, MATURITY_COUNT_LIMIT as i64)
        .await
    {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取事件计数失败，按 0 计");
            0
        }
    };

    // 对话示例条数（候选池全量）
    let example_count = match storage.list_all_examples(uid).await {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取示例计数失败，按 0 计");
            0
        }
    };

    // 知识事实条数：排除 SpeakingStyle（表达层单独呈现，不计入知识卡片成熟度）
    let fact_count = facts
        .iter()
        .filter(|f| f.field != ProfileField::SpeakingStyle)
        .count();

    DataMaturityView {
        l1_count,
        event_count,
        trait_count: traits
            .iter()
            .filter(|t| t.status == TraitStatus::Active)
            .count(),
        fact_count,
        example_count,
    }
}

// =========================================================
// regenerate_import_l1（人格 L1 重生成）
// =========================================================

/// 外层连续失败阈值：单 session 内部已有重试与退避，外层连续 3 次失败即判定 LLM 不可用。
const MAX_CONSECUTIVE_L1_FAILURES: u32 = 3;

/// 人格 L1 重生成结果（供宿主构造用户提示与统计展示）。
///
/// 字段约定:
/// - `l1_regenerated` / `l1_failed`: 生成成功 / 失败的会话数（单会话内部重试耗尽后才计入失败）。
/// - `total_sessions`: 参与重生成的会话总数（含跳过与未处理会话）。
/// - `early_terminated`: 是否因连续失败提前终止。
/// - `remaining_skipped`: 提前终止时未处理的会话数（未提前终止为 0）。
/// - `message`: 面向用户的提示文案。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaRegenerateOutcome {
    pub l1_regenerated: usize,
    pub l1_failed: usize,
    pub total_sessions: usize,
    pub early_terminated: bool,
    pub remaining_skipped: usize,
    pub message: String,
}

/// 为某人格的导入会话重新生成 L1 摘要（导入失败后的离线重建路径）。
///
/// 流程:
/// 1. 校验 UID 非空并确认人格存在（否则返回业务校验错误）；
/// 2. 全量枚举该人格消息并推导会话列表（去重按消息枚举顺序保留首次出现，顺序确定）；
/// 3. 逐会话按单段口径重生成 L1，连续失败达 [`MAX_CONSECUTIVE_L1_FAILURES`] 次判定 LLM 不可用并提前终止；
/// 4. 按成功 / 部分失败 / 提前终止三个分支构造提示文案。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 UID。
///
/// 返回:
/// - 计数与提示文案；空 UID 与人格不存在返回 `Validation` 错误。
///
/// 说明:
/// - L2/L3 级联不在本用例内触发：宿主拿到结果后自行触发 [`Engine::trigger_l2_check`]
///   （提示文案中的"L2/L3 正在后台处理中"即指该宿主行为，避免阻塞当前调用）。
/// - 幂等：会话已有目标人格的 L1 时按跳过处理（不计成功 / 失败，也不影响连续失败计数）。
pub(crate) async fn regenerate_import_l1(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<PersonaRegenerateOutcome> {
    if persona_uid.trim().is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }

    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(persona_uid).await?.is_none() {
        return Err(RamariaError::validation(format!(
            "人格不存在: uid={persona_uid}"
        )));
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        "重新生成导入会话的 L1 摘要"
    );

    // 离线重建路径：必须覆盖该人格的全部会话，故全量枚举其消息（不做截断）；
    // 若将来出现 persona 消息的浏览 / 展示需求，须另走分页查询。
    let messages = storage.list_messages_by_persona(persona_uid).await?;

    // 会话去重：按消息枚举顺序保留首次出现（确定性顺序便于复现），
    // 不使用 HashSet 的迭代顺序，避免会话处理顺序随哈希随机化漂移。
    let mut seen_sessions = HashSet::new();
    let mut session_ids: Vec<Uuid> = Vec::new();
    for message in &messages {
        if seen_sessions.insert(message.session_id) {
            session_ids.push(message.session_id);
        }
    }

    if session_ids.is_empty() {
        return Ok(PersonaRegenerateOutcome {
            l1_regenerated: 0,
            l1_failed: 0,
            total_sessions: 0,
            early_terminated: false,
            remaining_skipped: 0,
            message: "该人格没有关联的导入消息，无需处理。".to_string(),
        });
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        session_count = session_ids.len(),
        message_count = messages.len(),
        "找到关联的导入 session，开始重新生成 L1"
    );

    let total = session_ids.len();
    let mut l1_regenerated = 0usize;
    let mut l1_failed = 0usize;
    let mut consecutive_failures: u32 = 0;
    let mut early_terminated = false;
    let mut remaining_skipped = 0usize;

    for (idx, sid) in session_ids.iter().enumerate() {
        match engine
            .regenerate_l1_no_cascade(*sid, Some(persona_uid), None, None)
            .await
        {
            Ok(Some(_)) => {
                l1_regenerated += 1;
                consecutive_failures = 0;
                tracing::debug!(session_id = %sid, "L1 重新生成成功");
            }
            Ok(None) => {
                // 会话已有目标人格的 L1：幂等跳过，不计成功 / 失败，也不影响连续失败计数
                tracing::debug!(session_id = %sid, "L1 无需生成，跳过");
            }
            Err(e) => {
                l1_failed += 1;
                consecutive_failures += 1;
                tracing::warn!(
                    session_id = %sid,
                    error = %e,
                    consecutive_failures,
                    "L1 重新生成失败"
                );

                if consecutive_failures >= MAX_CONSECUTIVE_L1_FAILURES {
                    remaining_skipped = total.saturating_sub(idx + 1);
                    tracing::warn!(
                        persona_uid = %mask_id(persona_uid),
                        consecutive_failures,
                        l1_regenerated,
                        l1_failed,
                        remaining_skipped,
                        "L1 连续失败达到上限，判定 LLM 不可用，跳过剩余会话"
                    );
                    early_terminated = true;
                    break;
                }
            }
        }
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        l1_regenerated,
        l1_failed,
        total,
        early_terminated,
        remaining_skipped,
        "L1 重新生成完成"
    );

    let message = if early_terminated {
        format!(
            "L1 连续失败 {MAX_CONSECUTIVE_L1_FAILURES} 次，已提前终止。成功 {l1_regenerated}/{total}, 失败 {l1_failed}。请确认 LLM 模型已连接后重试。剩余 {remaining_skipped} 个 session 未处理。"
        )
    } else if l1_failed > 0 {
        format!(
            "L1 重新生成完成: 成功 {l1_regenerated}/{total}, 失败 {l1_failed}。请确认 LLM 模型已连接。L2/L3 正在后台处理中..."
        )
    } else {
        format!("L1 全部重新生成成功 ({l1_regenerated}/{total})。L2/L3 正在后台处理中...")
    };

    Ok(PersonaRegenerateOutcome {
        l1_regenerated,
        l1_failed,
        total_sessions: total,
        early_terminated,
        remaining_skipped,
        message,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, ScriptedLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply,
        engine_with_shared_scripted_llm, seed_persona, seed_session_with_messages,
    };
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::MemoryL1;
    use ramaria_storage::SqliteStorage;
    use std::sync::Arc;

    /// 造一个"已有目标人格 L1"的会话（带消息；供幂等跳过路径用例）。
    async fn seed_session_with_persona_l1(
        storage: &SqliteStorage,
        persona: &str,
        count: usize,
        base_ts: i64,
    ) -> Uuid {
        let session = seed_session_with_messages(storage, persona, count, base_ts).await;
        let mut l1 = MemoryL1::new(session, "既有摘要".to_string(), None);
        l1.persona_uid = Some(persona.to_string());
        storage
            .save_memory_l1(&l1)
            .await
            .expect("写入既有 L1 应成功");
        session
    }

    /// 空 UID（含纯空白）：返回业务校验错误。
    #[tokio::test]
    async fn regenerate_rejects_blank_uid() {
        let (engine, _storage, dir) = engine_with_db("persona-regen-blank-uid").await;

        let err = engine
            .regenerate_persona_l1("   ")
            .await
            .expect_err("空 UID 应返回错误");
        assert_eq!(err.category(), "validation", "应为业务校验错误: {err}");
        assert!(
            err.to_string().contains("人格 UID 不能为空"),
            "错误文案应提示 UID 为空: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 人格不存在：返回业务校验错误（文案含目标 UID）。
    #[tokio::test]
    async fn regenerate_rejects_missing_persona() {
        let (engine, storage, dir) = engine_with_db("persona-regen-missing").await;
        seed_persona(&storage, "char-0001").await;

        let err = engine
            .regenerate_persona_l1("char-missing")
            .await
            .expect_err("人格不存在应返回错误");
        assert_eq!(err.category(), "validation", "应为业务校验错误: {err}");
        assert!(
            err.to_string().contains("人格不存在: uid=char-missing"),
            "错误文案应含目标 UID: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 该人格无消息：返回零计数与"无需处理"提示，不触达 LLM。
    #[tokio::test]
    async fn regenerate_returns_noop_without_messages() {
        let (engine, storage, dir) =
            engine_with_l1_reply("persona-regen-noop", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("无消息应正常返回");
        assert_eq!(outcome.total_sessions, 0);
        assert_eq!(outcome.l1_regenerated, 0);
        assert_eq!(outcome.l1_failed, 0);
        assert!(!outcome.early_terminated);
        assert_eq!(outcome.remaining_skipped, 0);
        assert_eq!(outcome.message, "该人格没有关联的导入消息，无需处理。");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多会话全部成功：逐会话产出绑定人格的 L1，计数与提示为全成功分支。
    #[tokio::test]
    async fn regenerate_all_sessions_succeed() {
        let (engine, storage, dir) =
            engine_with_l1_reply("persona-regen-success", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_a = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
        let session_b = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("重生成应成功");
        assert_eq!(outcome.total_sessions, 2);
        assert_eq!(outcome.l1_regenerated, 2);
        assert_eq!(outcome.l1_failed, 0);
        assert!(!outcome.early_terminated);
        assert_eq!(outcome.remaining_skipped, 0);
        assert_eq!(
            outcome.message,
            "L1 全部重新生成成功 (2/2)。L2/L3 正在后台处理中..."
        );

        for session in [session_a, session_b] {
            let l1_list = storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功");
            assert_eq!(l1_list.len(), 1, "每个会话应恰有一条 L1");
            assert_eq!(
                l1_list[0].persona_uid.as_deref(),
                Some("char-0001"),
                "L1 应绑定目标人格"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 部分失败未达早停阈值：成功 / 失败计数如实上报，提示为失败分支。
    #[tokio::test]
    async fn regenerate_reports_partial_failure_without_early_stop() {
        // 脚本队列仅一条有效回复：处理顺序在前的会话成功；其后的会话耗尽队列后
        // 拿到的空回复解析失败，内部重试均失败 → 计入失败，但未达连续失败阈值。
        let llm = Arc::new(ScriptedLlm::replies(&[L1_JSON_REPLY]));
        let (engine, storage, dir) = engine_with_shared_scripted_llm(
            "persona-regen-partial",
            llm,
            RamariaConfig::default(),
            None,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        let ok_session = seed_session_with_messages(&storage, "char-0001", 2, 2_000).await;
        let fail_session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("部分失败应正常返回");
        assert_eq!(outcome.total_sessions, 2);
        assert_eq!(outcome.l1_regenerated, 1);
        assert_eq!(outcome.l1_failed, 1);
        assert!(!outcome.early_terminated, "失败未达阈值不应提前终止");
        assert_eq!(outcome.remaining_skipped, 0);
        assert_eq!(
            outcome.message,
            "L1 重新生成完成: 成功 1/2, 失败 1。请确认 LLM 模型已连接。L2/L3 正在后台处理中..."
        );

        assert_eq!(
            storage
                .list_memory_l1(ok_session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "成功会话应产出 L1"
        );
        assert!(
            storage
                .list_memory_l1(fail_session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "失败会话不应残留 L1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 连续失败达到阈值：提前终止并报告跳过数量，跳过的会话未被处理。
    #[tokio::test]
    async fn regenerate_stops_after_consecutive_failures() {
        let (engine, storage, dir) = engine_with_failing_llm("persona-regen-early-stop").await;
        seed_persona(&storage, "char-0001").await;
        let session_1 = seed_session_with_messages(&storage, "char-0001", 3, 4_000).await;
        let session_2 = seed_session_with_messages(&storage, "char-0001", 3, 3_000).await;
        let session_3 = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
        let session_skipped = seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("失败路径应返回结果而非错误");
        assert_eq!(outcome.total_sessions, 4);
        assert_eq!(outcome.l1_regenerated, 0);
        assert_eq!(outcome.l1_failed, 3, "连续 3 次失败后应停止");
        assert!(outcome.early_terminated, "应提前终止");
        assert_eq!(outcome.remaining_skipped, 1, "应跳过剩余 1 个会话");
        assert_eq!(
            outcome.message,
            "L1 连续失败 3 次，已提前终止。成功 0/4, 失败 3。请确认 LLM 模型已连接后重试。剩余 1 个 session 未处理。"
        );

        for session in [session_1, session_2, session_3] {
            assert!(
                storage
                    .list_memory_l1(session)
                    .await
                    .expect("读取 L1 应成功")
                    .is_empty(),
                "失败的会话不应残留 L1"
            );
        }
        assert!(
            storage
                .list_memory_l1(session_skipped)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "提前终止后跳过的会话不应被处理"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 幂等跳过（已有目标人格 L1）夹在失败之间：不重置连续失败计数，也不计入成功。
    #[tokio::test]
    async fn regenerate_skip_does_not_reset_failure_streak() {
        let (engine, storage, dir) = engine_with_failing_llm("persona-regen-skip-streak").await;
        seed_persona(&storage, "char-0001").await;
        // 处理顺序按消息时间倒序：失败、跳过、失败、失败、未处理
        let fail_a = seed_session_with_messages(&storage, "char-0001", 3, 5_000).await;
        let skipped = seed_session_with_persona_l1(&storage, "char-0001", 2, 4_000).await;
        let fail_b = seed_session_with_messages(&storage, "char-0001", 3, 3_000).await;
        let fail_c = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
        let fail_unprocessed = seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("失败路径应返回结果而非错误");
        assert_eq!(outcome.total_sessions, 5);
        assert_eq!(outcome.l1_regenerated, 0, "跳过不计入成功");
        assert_eq!(
            outcome.l1_failed, 3,
            "跳过不重置连续失败计数：第 4 个失败不应发生"
        );
        assert!(outcome.early_terminated, "应在第 3 次连续失败时提前终止");
        assert_eq!(outcome.remaining_skipped, 1);
        assert_eq!(
            outcome.message,
            "L1 连续失败 3 次，已提前终止。成功 0/5, 失败 3。请确认 LLM 模型已连接后重试。剩余 1 个 session 未处理。"
        );

        assert_eq!(
            storage
                .list_memory_l1(skipped)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "跳过会话应保留既有 L1"
        );
        for session in [fail_a, fail_b, fail_c, fail_unprocessed] {
            assert!(
                storage
                    .list_memory_l1(session)
                    .await
                    .expect("读取 L1 应成功")
                    .is_empty(),
                "失败 / 未处理会话不应有 L1"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
