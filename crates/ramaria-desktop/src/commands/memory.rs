//! crates/ramaria-desktop/src/commands/memory.rs - 记忆查看 Tauri Commands
//!
//! 设计特点:
//! - 提供 L1/L2/L3 三层记忆查询接口，按 persona_uid 可选过滤
//! - 委托服务层浏览用例，桌面侧只把服务层视图映射为前端字段（含内部字段的简化）
//! - 支持 limit 参数控制返回条数，默认 200（与桌面历史口径一致）
//! - 不写业务逻辑，纯委托服务层用例
//! - 隐私：视图不含原文消息；日志只记计数与人格标识

use crate::DesktopState;
use ramaria_service::{L1BrowseRequest, L2BrowseRequest, TraitEvidenceRequest};
use std::collections::HashMap;
use tauri::State;

use super::memory_view::fact_to_view;
pub use super::memory_view::{
    EventInEvidenceView, FactListView, L1SourceView, MemoryEventView, MemoryL1View,
    PersonaFactView, PersonaView, PersonalityProfileView, ProfileStatusView, TraitDetailView,
    TraitEvidenceChainView,
};

// =========================================================
// get_personas — 列出所有 Persona
// =========================================================

/// 列出所有已注册的人格。
///
/// 返回:
/// - JSON 数组，每项为 PersonaView。
///
/// 说明:
/// - 使用全字段列表用例（摘要字段与创建时间同源），映射为前端摘要视图；
/// - 含停用项（`is_active` 透出）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_personas(state: State<'_, DesktopState>) -> Result<Vec<PersonaView>, String> {
    let personas = state
        .engine
        .persona_list_full()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 persona 列表失败"))?;

    let views: Vec<PersonaView> = personas
        .into_iter()
        .map(|p| PersonaView {
            uid: p.uid,
            name: p.name,
            kind: p.kind,
            source: p.source,
            is_active: p.is_active,
            created_at: p.created_at,
        })
        .collect();

    tracing::debug!(count = views.len(), "get_personas 完成");
    Ok(views)
}

// =========================================================
// get_l1_memories — 查询 L1 摘要
// =========================================================

/// 查询 L1 会话摘要记忆。
///
/// 参数:
/// - `persona_uid`: 可选，按人格过滤
/// - `limit`: 返回条数上限，默认 200（上限 1000）
///
/// 返回:
/// - JSON 数组，每项为 MemoryL1View。
///
/// 说明:
/// - 按会话收集摘要的统一排序口径由服务层用例承担（收集、倒序、截断）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_l1_memories(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<MemoryL1View>, String> {
    // 桌面口径：缺省 200、上限 1000（越界值收敛到边界）
    let limit = limit.map(|l| l.clamp(0, 1000) as u32);

    let page = state
        .engine
        .memory_l1(L1BrowseRequest {
            persona: persona_uid,
            unabsorbed_only: false,
            limit,
            offset: None,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 L1 记忆失败"))?;

    let views: Vec<MemoryL1View> = page
        .items
        .into_iter()
        .map(|m| MemoryL1View {
            id: m.id.to_string(),
            session_id: m.session_id.to_string(),
            summary: m.summary,
            keywords: m.keywords.unwrap_or_default(),
            atmosphere: m.atmosphere.unwrap_or_default(),
            valence: m.valence,
            salience: m.salience,
            persona_uid: m.persona_uid,
            created_at: m.created_at,
            time_period: m.time_period,
            context_json: m.context_json,
        })
        .collect();

    tracing::debug!(count = views.len(), "get_l1_memories 完成");
    Ok(views)
}

// =========================================================
// get_l2_events — 查询 L2 事件
// =========================================================

/// 查询 L2 离散事件记忆。
///
/// 参数:
/// - `persona_uid`: 可选，按人格过滤
/// - `limit`: 返回条数上限，默认 200（上限 1000）
///
/// 返回:
/// - JSON 数组，每项为 MemoryEventView。
///
/// 说明:
/// - persona 缺省时由服务层合并全部人格事件后统一排序截断。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_l2_events(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<MemoryEventView>, String> {
    // 桌面口径：缺省 200、上限 1000（越界值收敛到边界）
    let limit = limit.map(|l| l.clamp(0, 1000) as u32);

    let page = state
        .engine
        .memory_l2(L2BrowseRequest {
            persona: persona_uid,
            limit,
            offset: None,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 L2 事件失败"))?;

    let views: Vec<MemoryEventView> = page
        .items
        .into_iter()
        .map(|e| MemoryEventView {
            id: e.id,
            persona_uid: e.persona_uid,
            title: e.title,
            summary: e.summary,
            keywords: e.keywords.unwrap_or_default(),
            valence: e.valence,
            confidence: e.confidence,
            presentation: e.presentation.as_str().to_string(),
            share: e.share,
            attitude: e.attitude.unwrap_or_default(),
            salience: e.salience,
            created_at: e.created_at,
            start: e.start,
            end: e.end,
        })
        .collect();

    tracing::debug!(count = views.len(), "get_l2_events 完成");
    Ok(views)
}

// =========================================================
// trigger_memory_pipeline — 手动触发记忆管线
// =========================================================

/// 手动触发 L2 事件提取和 L3 性格推断管线。
///
/// 说明:
/// - 服务层用例遍历所有 persona，检查未吸收 L1 是否达到阈值 → 触发 L2 事件提取。
/// - L2 提取成功后自动级联 L3 性格推断。
/// - 适用于快速导入后，用户手动启动深度处理。
/// - 此操作为异步后台任务，返回"已启动"即表示成功提交。
///
/// 返回:
/// - `"ok"`: 管线已触发，后台异步执行。
///
/// 接线:
/// - 由前端"深度处理导入的消息"按钮触发：L1 重新生成后追加调用本命令，
///   作为遍历全部 persona 的全局补救入口。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn trigger_memory_pipeline(state: State<'_, DesktopState>) -> Result<String, String> {
    tracing::info!("手动触发记忆管线（L2→L3）");

    let engine = state.engine.clone();
    tokio::spawn(async move {
        engine.trigger_l2_check().await;
    });

    Ok("ok".to_string())
}

// =========================================================
// get_personality_profile — 查询 L3 三层性格画像
// =========================================================

/// 查询指定人格的完整三层性格画像。
///
/// 参数:
/// - `persona_uid`: 目标人格业务标识（如 "user-0001"）
///
/// 返回:
/// - `PersonalityProfileView`: 按 base/primary/accent 三层分组的性格标签列表。
///
/// 说明:
/// - 仅返回 status=Active 的 trait（排除 Deprecated/Historical）。
/// - 每层内按 seq 升序排列。
/// - 若指定 persona 没有已生成的性格画像，返回三层均为空数组（非错误）。
///
/// 日志:
/// - INFO: 记录查询的 persona_uid 和各层 trait 数量。
/// - ERROR: 服务层查询失败。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_personality_profile(
    state: State<'_, DesktopState>,
    persona_uid: String,
) -> Result<PersonalityProfileView, String> {
    // 参数校验
    if persona_uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    let profile = state
        .engine
        .personality_profile(&persona_uid)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询性格画像失败"))?;

    let base_count = profile.base.len();
    let primary_count = profile.primary.len();
    let accent_count = profile.accent.len();

    tracing::info!(
        %persona_uid,
        base_count,
        primary_count,
        accent_count,
        "get_personality_profile 完成"
    );

    Ok(PersonalityProfileView {
        persona_uid: profile.persona_uid,
        base: profile
            .base
            .into_iter()
            .map(TraitDetailView::from)
            .collect(),
        primary: profile
            .primary
            .into_iter()
            .map(TraitDetailView::from)
            .collect(),
        accent: profile
            .accent
            .into_iter()
            .map(TraitDetailView::from)
            .collect(),
    })
}

// =========================================================
// get_trait_evidence — 查询性格标签的完整证据链
// =========================================================

/// 查询指定性格标签的完整证据溯源链。
///
/// 参数:
/// - `persona_uid`: 目标人格业务标识（用于查询事件和 L1 数据）。
/// - `trait_id`: 目标性格标签 ID（personality_traits 表的主键）。
///
/// 返回:
/// - `Vec<TraitEvidenceChainView>`: 按时间降序的证据链事件列表。
///
/// 说明:
/// - 链路层级：trait → trait_evidence → memory_events → event_sources → memory_l1 → evidence_notes。
/// - 每层查询均做错误隔离：单条记录查询失败不影响整体（服务层记录 warn 后跳过）。
/// - 按 TraitEvidence.created_at 降序排列（最新证据在前）。
/// - 统计 evidence 中 support/contradict/neutral 的数量分布。
///
/// 边界处理:
/// - trait_id 不存在或无证据记录时返回空链（非错误）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_trait_evidence(
    state: State<'_, DesktopState>,
    persona_uid: String,
    trait_id: i64,
) -> Result<Vec<TraitEvidenceChainView>, String> {
    // 参数校验
    if persona_uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }
    if trait_id <= 0 {
        return Err("trait_id 必须为正整数".to_string());
    }

    let chains = state
        .engine
        .memory_trait_evidence(TraitEvidenceRequest {
            persona: persona_uid,
            trait_id,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询性格标签证据失败"))?;

    let views: Vec<TraitEvidenceChainView> = chains
        .into_iter()
        .map(|chain| TraitEvidenceChainView {
            trait_id: chain.trait_id,
            trait_label: chain.trait_label,
            total_evidence: chain.total_evidence,
            support_count: chain.support_count,
            contradict_count: chain.contradict_count,
            neutral_count: chain.neutral_count,
            evidence_events: chain
                .evidence_events
                .into_iter()
                .map(|event| EventInEvidenceView {
                    event_id: event.event_id,
                    title: event.title,
                    summary: event.summary,
                    confidence: event.confidence,
                    valence: event.valence,
                    salience: event.salience,
                    attitude: event.attitude,
                    paraphrase: event.paraphrase,
                    motives: event.motives,
                    l1_sources: event
                        .l1_sources
                        .into_iter()
                        .map(|src| L1SourceView {
                            l1_id: src.l1_id.to_string(),
                            summary: src.summary,
                            evidence_notes: src.evidence_notes,
                            atmosphere: src.atmosphere,
                            valence: src.valence,
                            weight: src.weight,
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    tracing::debug!(trait_id, count = views.len(), "get_trait_evidence 完成");
    Ok(views)
}

// =========================================================
// get_profile_status — 查询数据状态指示器
// =========================================================

/// 查询指定人格的数据画像状态。
///
/// 参数:
/// - `persona_uid`: 目标人格业务标识。
///
/// 返回:
/// - `ProfileStatusView`: 包含有效样本量、状态标识和描述文本。
///
/// 说明:
/// - n_total_eff = Σ(所有活跃 trait 的 evidence 字段)。
/// - 状态判定: n_total_eff < 5 → "insufficient" / 5-20 → "preliminary" / ≥20 → "trusted"。
/// - 若 persona 无任何 trait，返回 n_total_eff=0, status="insufficient"。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_profile_status(
    state: State<'_, DesktopState>,
    persona_uid: String,
) -> Result<ProfileStatusView, String> {
    // 参数校验
    if persona_uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    let status = state
        .engine
        .profile_status(&persona_uid)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询画像状态失败"))?;

    tracing::info!(
        %persona_uid,
        n_total_eff = status.n_total_eff,
        active_count = status.active_trait_count,
        status = %status.status,
        "get_profile_status 完成"
    );

    Ok(ProfileStatusView {
        persona_uid: status.persona_uid,
        n_total_eff: status.n_total_eff,
        active_trait_count: status.active_trait_count,
        status: status.status,
        status_text: status.status_text,
    })
}

// =========================================================
// get_facts — 知识事实只读查询
// =========================================================

/// 查询指定人格的 active 知识事实，按 ProfileField 分组。
///
/// 返回:
/// - `grouped`: { field_label: [PersonaFactView] }——每组含该 field 的全部 active 事实。
/// - `versions`: { fact_id: [版本链] }——覆盖链历史版本折叠展示用（链头最早在前）。
///
/// 只读视图约定:
/// - 无删除/编辑入口，仅展示。
/// - 严格按 persona_uid 隔离。
/// - 事实内容为陈述句（非原文）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_facts(
    state: State<'_, DesktopState>,
    persona_uid: String,
) -> Result<FactListView, String> {
    if persona_uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    let view = state
        .engine
        .memory_facts_grouped(&persona_uid)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询知识事实失败"))?;

    let grouped: HashMap<String, Vec<PersonaFactView>> = view
        .grouped
        .into_iter()
        .map(|(field, facts)| (field, facts.into_iter().map(fact_to_view).collect()))
        .collect();
    let versions: HashMap<i64, Vec<PersonaFactView>> = view
        .versions
        .into_iter()
        .map(|(fact_id, chain)| (fact_id, chain.into_iter().map(fact_to_view).collect()))
        .collect();

    tracing::info!(
        %persona_uid,
        active_groups = grouped.len(),
        version_chains = versions.len(),
        "get_facts 完成（知识卡片只读查询）"
    );

    Ok(FactListView {
        persona_uid: view.persona_uid,
        grouped,
        versions,
    })
}
