//! rust/crates/ramaria-desktop/src/commands/memory.rs - 记忆查看 Tauri Commands
//!
//! 设计特点:
//! - 提供 L1/L2/L3 三层记忆查询接口，按 persona_uid 可选过滤
//! - 返回值经过简化序列化，移除内部字段（如 INTEGER id），仅暴露业务字段
//! - 支持 limit 参数控制返回条数，默认 50
//! - 不写业务逻辑，纯委托 StorageBackend
//! - 使用 futures::future::join_all 并发查询，避免串行 N+1 问题

use crate::DesktopState;
use futures::future::join_all;
use ramaria_core::types::{
    EvidenceDirection, MemoryEvent, PersonalityTrait, TraitLayer, TraitStatus,
};
use serde::Serialize;
use std::collections::HashMap;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// L1 记忆摘要视图。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryL1View {
    pub id: String,
    pub session_id: String,
    pub summary: String,
    pub keywords: String,
    pub atmosphere: String,
    pub valence: f64,
    pub salience: f64,
    pub persona_uid: Option<String>,
    pub created_at: i64,
    /// 时间段（清晨/上午/下午/傍晚/夜间/深夜）
    pub time_period: Option<String>,
    /// 分组上下文 JSON，含 chat_partners / message_count 等
    pub context_json: Option<String>,
}

/// L2 事件视图。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEventView {
    pub id: i64,
    pub persona_uid: String,
    pub title: String,
    pub summary: String,
    pub keywords: String,
    pub valence: f64,
    pub confidence: f64,
    pub presentation: String,
    pub share: f64,
    pub attitude: String,
    pub salience: f64,
    pub created_at: i64,
}

/// L3 性格标签视图。
#[derive(Debug, Clone, Serialize)]
pub struct PersonalityTraitView {
    pub id: i64,
    pub persona_uid: String,
    pub layer: String,
    pub label: String,
    pub meaning: String,
    pub confidence: f64,
    pub evidence: f64,
    pub consistency: f64,
    pub status: String,
    pub created_at: i64,
}

/// Persona 摘要视图。
#[derive(Debug, Clone, Serialize)]
pub struct PersonaView {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub source: String,
    pub is_active: bool,
    pub created_at: i64,
}

// =========================================================
// get_personas — 列出所有 Persona
// =========================================================

/// 列出所有已注册的人格。
///
/// 返回:
/// - JSON 数组，每项为 PersonaView
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_personas(state: State<'_, DesktopState>) -> Result<Vec<PersonaView>, String> {
    let personas = state
        .app
        .storage()
        .list_personas()
        .await
        .map_err(|e| format!("查询 persona 列表失败: {}", e))?;

    let views: Vec<PersonaView> = personas
        .into_iter()
        .map(|p| PersonaView {
            uid: p.uid,
            name: p.name,
            kind: p.kind.as_str().to_string(),
            source: p.source,
            is_active: p.active,
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
/// - `limit`: 返回条数上限，默认 50
///
/// 返回:
/// - JSON 数组，每项为 MemoryL1View
///
/// 说明:
/// - list_memory_l1 按 session_id 查询，本命令遍历所有会话收集 L1 摘要
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_l1_memories(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<MemoryL1View>, String> {
    let limit = limit.unwrap_or(200).min(1000) as usize;
    let storage = state.app.storage();

    // 获取所有会话
    let sessions = storage
        .list_sessions()
        .await
        .map_err(|e| format!("查询会话列表失败: {}", e))?;

    // 并发查询最近 N 个会话的 L1 摘要
    let session_limit = sessions.len().min(500);
    let l1_futures: Vec<_> = sessions[..session_limit]
        .iter()
        .map(|s| storage.list_memory_l1(s.id))
        .collect();
    let l1_results = join_all(l1_futures).await;

    // 收集结果，按 persona_uid 过滤
    let mut all_l1: Vec<MemoryL1View> = Vec::new();
    for result in l1_results {
        let l1_list = result.map_err(|e| format!("查询 L1 记忆失败: {}", e))?;
        for m in l1_list {
            // 按 persona_uid 可选过滤
            if let Some(ref uid) = persona_uid {
                if m.persona_uid.as_deref() != Some(uid.as_str()) {
                    continue;
                }
            }
            all_l1.push(MemoryL1View {
                id: m.id.to_string(),
                session_id: m.session_id.to_string(),
                summary: m.summary,
                keywords: m.keywords.unwrap_or_default(),
                atmosphere: m.atmosphere.unwrap_or_default(),
                time_period: m.time_period,
                context_json: m.context_json,
                valence: m.valence,
                salience: m.salience,
                persona_uid: m.persona_uid,
                created_at: m.created_at,
            });
        }
        if all_l1.len() >= limit {
            break;
        }
    }

    // 按创建时间倒序
    all_l1.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    all_l1.truncate(limit);

    tracing::debug!(count = all_l1.len(), "get_l1_memories 完成");
    Ok(all_l1)
}

// =========================================================
// get_l2_events — 查询 L2 事件
// =========================================================

/// 查询 L2 离散事件记忆。
///
/// 参数:
/// - `persona_uid`: 可选，按人格过滤
/// - `limit`: 返回条数上限，默认 50
///
/// 返回:
/// - JSON 数组，每项为 MemoryEventView
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_l2_events(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<MemoryEventView>, String> {
    let limit = limit.unwrap_or(200).min(1000);

    // 如果有 persona_uid 过滤，使用带过滤的查询；否则获取全部
    let events = if let Some(ref uid) = persona_uid {
        state
            .app
            .storage()
            .list_events_by_persona(uid, 0, limit)
            .await
            .map_err(|e| format!("查询 L2 事件失败: {}", e))?
    } else {
        // 获取所有 persona 的事件（并发查询，避免串行 N+1）
        let all_personas = state
            .app
            .storage()
            .list_personas()
            .await
            .map_err(|e| format!("查询 persona 列表失败: {}", e))?;

        let storage = state.app.storage();
        let event_futures: Vec<_> = all_personas
            .iter()
            .map(|p| {
                let s = storage.clone();
                let uid = p.uid.clone();
                async move { s.list_events_by_persona(&uid, 0, limit).await }
            })
            .collect();
        let event_results = join_all(event_futures).await;

        let mut all_events = Vec::new();
        for result in event_results {
            let mut events = result.map_err(|e| format!("查询 L2 事件失败: {}", e))?;
            all_events.append(&mut events);
        }
        // 按创建时间倒序排列，截取 limit 条
        all_events.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        all_events.truncate(limit as usize);
        all_events
    };

    let views: Vec<MemoryEventView> = events
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
        })
        .collect();

    tracing::debug!(count = views.len(), "get_l2_events 完成");
    Ok(views)
}

// =========================================================
// get_l3_traits — 查询 L3 性格标签
// =========================================================

/// 查询 L3 结构化性格画像标签。
///
/// 参数:
/// - `persona_uid`: 可选，按人格过滤
///
/// 返回:
/// - JSON 数组，每项为 PersonalityTraitView
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_l3_traits(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
) -> Result<Vec<PersonalityTraitView>, String> {
    let traits = if let Some(ref uid) = persona_uid {
        state
            .app
            .storage()
            .list_traits_by_persona(uid)
            .await
            .map_err(|e| format!("查询 L3 性格标签失败: {}", e))?
    } else {
        // 获取所有 persona 的性格标签（并发查询，避免串行 N+1）
        let all_personas = state
            .app
            .storage()
            .list_personas()
            .await
            .map_err(|e| format!("查询 persona 列表失败: {}", e))?;

        let storage = state.app.storage();
        let trait_futures: Vec<_> = all_personas
            .iter()
            .map(|p| {
                let s = storage.clone();
                let uid = p.uid.clone();
                async move { s.list_traits_by_persona(&uid).await }
            })
            .collect();
        let trait_results = join_all(trait_futures).await;

        let mut all_traits = Vec::new();
        for result in trait_results {
            let mut t = result.map_err(|e| format!("查询 L3 性格标签失败: {}", e))?;
            all_traits.append(&mut t);
        }
        all_traits
    };

    let views: Vec<PersonalityTraitView> = traits
        .into_iter()
        .map(|t| PersonalityTraitView {
            id: t.id,
            persona_uid: t.persona_uid,
            layer: t.layer.as_str().to_string(),
            label: t.trait_label,
            meaning: t.meaning,
            confidence: t.confidence,
            evidence: t.evidence,
            consistency: t.consistency,
            status: t.status.as_str().to_string(),
            created_at: t.created_at,
        })
        .collect();

    tracing::debug!(count = views.len(), "get_l3_traits 完成");
    Ok(views)
}

// =========================================================
// trigger_memory_pipeline — 手动触发记忆管线
// =========================================================

/// 手动触发 L2 事件提取和 L3 性格推断管线。
///
/// 说明:
/// - 遍历所有 persona，检查未吸收 L1 是否 ≥ 5 条 → 触发 L2 事件提取。
/// - L2 提取成功后自动级联 L3 性格推断。
/// - 适用于快速导入后，用户手动启动深度处理。
/// - 此操作为异步后台任务，返回"已启动"即表示成功提交。
///
/// 返回:
/// - `"ok"`: 管线已触发，后台异步执行。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn trigger_memory_pipeline(state: State<'_, DesktopState>) -> Result<String, String> {
    tracing::info!("手动触发记忆管线（L2→L3）");

    let app = state.app.clone();
    tokio::spawn(async move {
        app.trigger_l2_check().await;
    });

    Ok("ok".to_string())
}

// =========================================================
// get_personality_profile — 查询 L3 三层性格画像（M5-C）
// =========================================================

/// L3 性格画像完整视图——按 base/primary/accent 三层分组。
///
/// 职责:
/// - 供前端 MemoryView L3 Tab 渲染三层分层展示。
/// - 每层包含该层的所有活跃 trait，含完整字段（trigger/suppress 等）。
///
/// 字段约定:
/// - `base`: 底色层 trait 列表（跨情境稳定，2-3 条）
/// - `primary`: 主色调层 trait 列表（日常最突出，1-2 条）
/// - `accent`: 点缀层 trait 列表（特定条件浮现，2-4 条）
#[derive(Debug, Clone, Serialize)]
pub struct PersonalityProfileView {
    /// 所属人格标识
    pub persona_uid: String,
    /// 底色层
    pub base: Vec<TraitDetailView>,
    /// 主色调层
    pub primary: Vec<TraitDetailView>,
    /// 点缀层
    pub accent: Vec<TraitDetailView>,
}

/// 单条性格标签的详细视图——用于三层分层展示。
///
/// 与 `PersonalityTraitView` 的区别:
/// - 包含 trigger/suppress/not_meaning/related 等前端三层展示所需字段。
/// - 包含 evidence 字段（有效证据量），供前端渲染置信度条。
#[derive(Debug, Clone, Serialize)]
pub struct TraitDetailView {
    /// 内部 ID（用于后续 get_trait_evidence 查询）
    pub id: i64,
    /// 标签词，如"温和""幽默"
    pub label: String,
    /// 在此人身上的具体含义
    pub meaning: String,
    /// 聚合置信度 0..1
    pub confidence: f64,
    /// 有效证据量
    pub evidence: f64,
    /// 一致度
    pub consistency: f64,
    /// 所属分层: base / primary / accent
    pub layer: String,
    /// 反向界定——它不是什么
    pub not_meaning: Option<String>,
    /// 浮现条件
    pub trigger: Option<String>,
    /// 抑制条件
    pub suppress: Option<String>,
    /// 与其他性格的关系
    pub related: Option<String>,
    /// 层内排序
    pub seq: i32,
    /// 性格来源
    pub source: String,
    /// 性格状态
    pub status: String,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
}

impl From<PersonalityTrait> for TraitDetailView {
    fn from(t: PersonalityTrait) -> Self {
        Self {
            id: t.id,
            label: t.trait_label,
            meaning: t.meaning,
            confidence: t.confidence,
            evidence: t.evidence,
            consistency: t.consistency,
            layer: t.layer.as_str().to_string(),
            not_meaning: t.not_meaning,
            trigger: t.trigger,
            suppress: t.suppress,
            related: t.related,
            seq: t.seq,
            source: t.source.as_str().to_string(),
            status: t.status.as_str().to_string(),
            created_at: t.created_at,
        }
    }
}

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
/// - ERROR: 存储层查询失败。
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

    // 验证 persona 存在
    let _persona = state
        .app
        .storage()
        .get_persona_by_uid(&persona_uid)
        .await
        .map_err(|e| format!("查询 persona 失败: {}", e))?
        .ok_or_else(|| format!("人格不存在: uid={persona_uid}"))?;

    // 查询该 persona 的所有 trait
    let traits = state
        .app
        .storage()
        .list_traits_by_persona(&persona_uid)
        .await
        .map_err(|e| format!("查询 L3 性格标签失败: {}", e))?;

    // 仅保留 Active 状态，按 layer 分组
    let mut base = Vec::new();
    let mut primary = Vec::new();
    let mut accent = Vec::new();

    for t in traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        match t.layer {
            TraitLayer::Base => base.push(TraitDetailView::from(t)),
            TraitLayer::Primary => primary.push(TraitDetailView::from(t)),
            TraitLayer::Accent => accent.push(TraitDetailView::from(t)),
            // TraitLayer 标记为 #[non_exhaustive]，未来新变体走此分支静默跳过
            _ => {}
        }
    }

    // 每层内按 seq 升序
    base.sort_by_key(|v| v.seq);
    primary.sort_by_key(|v| v.seq);
    accent.sort_by_key(|v| v.seq);

    tracing::info!(
        %persona_uid,
        base_count = base.len(),
        primary_count = primary.len(),
        accent_count = accent.len(),
        "get_personality_profile 完成"
    );

    Ok(PersonalityProfileView {
        persona_uid,
        base,
        primary,
        accent,
    })
}

// =========================================================
// get_trait_evidence — 查询性格标签的完整证据链（M5-C）
// =========================================================

/// 证据链中的 L1 摘要引用视图。
///
/// 职责:
/// - 承载事件溯源链中的 L1 层证据片段。
/// - 包含 evidence_notes（双层摘要中的证据片段层），供前端"展开证据"渲染。
#[derive(Debug, Clone, Serialize)]
pub struct L1SourceView {
    /// L1 摘要 ID（UUID）
    pub l1_id: String,
    /// L1 摘要文本
    pub summary: String,
    /// L1 证据片段（evidence_notes），可能为空数组
    pub evidence_notes: Vec<String>,
    /// L1 会话氛围
    pub atmosphere: Option<String>,
    /// 情绪效价
    pub valence: f64,
    /// L1 对事件的贡献权重
    pub weight: f64,
}

/// 证据链中的事件视图。
///
/// 职责:
/// - 承载 trait→event 证据链中单个事件的详细信息。
/// - 包含事件的完整推断信号（confidence/valence/salience/attitude/paraphrase）。
#[derive(Debug, Clone, Serialize)]
pub struct EventInEvidenceView {
    /// 事件 ID
    pub event_id: i64,
    /// 事件标题
    pub title: String,
    /// 事件摘要
    pub summary: String,
    /// 事实确凿度
    pub confidence: f64,
    /// 情绪效价
    pub valence: f64,
    /// 显著性
    pub salience: f64,
    /// 态度描述
    pub attitude: Option<String>,
    /// 态度的去情境化重述
    pub paraphrase: Option<String>,
    /// 底层动机标注
    pub motives: Option<String>,
    /// 事件所关联的 L1 溯源列表
    pub l1_sources: Vec<L1SourceView>,
}

/// 完整证据链视图——一条 trait 与其所有支撑/矛盾事件的完整溯源。
///
/// 职责:
/// - 供前端"展开证据"按钮渲染完整溯源链。
/// - 链结构: trait → 该 trait 的所有证据记录 → 每条证据的事件 → 事件的所有 L1 溯源 → L1 的 evidence_notes。
#[derive(Debug, Clone, Serialize)]
pub struct TraitEvidenceChainView {
    /// 性格标签 ID
    pub trait_id: i64,
    /// 标签词
    pub trait_label: String,
    /// 证据总数
    pub total_evidence: usize,
    /// 支持性证据数
    pub support_count: usize,
    /// 矛盾性证据数
    pub contradict_count: usize,
    /// 中性证据数
    pub neutral_count: usize,
    /// 按创建时间降序排列的证据事件链
    pub evidence_events: Vec<EventInEvidenceView>,
}

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
/// - 证据链层级：trait → trait_evidence → memory_events → event_sources → memory_l1 → evidence_notes。
/// - 每层查询均做错误隔离：单条记录查询失败不影响整体（记录 warn 后跳过）。
/// - 按 TraitEvidence.created_at 降序排列（最新证据在前）。
/// - 统计 evidence 中 support/contradict/neutral 的数量分布。
///
/// 边界处理:
/// - trait_id 不存在或无证据记录时返回空链（非错误）。
/// - 某条事件或 L1 记录查询失败时跳过该条，不阻塞整体。
/// - 某条 L1 无 evidence_notes 时返回空数组。
///
/// 日志:
/// - INFO: 记录查询的 trait_id 和证据总数。
/// - WARN: 单条事件/L1 查询失败时记录（非阻塞）。
/// - ERROR: 存储层查询失败。
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

    let storage = state.app.storage();

    // Step 1: 获取 trait 基本信息
    let traits = storage
        .list_traits_by_persona(&persona_uid)
        .await
        .map_err(|e| format!("查询 L3 性格标签失败: {}", e))?;

    let target_trait = traits
        .into_iter()
        .find(|t| t.id == trait_id)
        .ok_or_else(|| format!("性格标签不存在: trait_id={trait_id}, persona_uid={persona_uid}"))?;

    let trait_label = target_trait.trait_label.clone();

    // Step 2: 获取该 trait 的所有证据记录
    let evidence_records = storage
        .list_evidence_by_trait(trait_id)
        .await
        .map_err(|e| format!("查询 trait 证据记录失败: {}", e))?;

    if evidence_records.is_empty() {
        tracing::info!(trait_id, %persona_uid, "trait 无证据记录，返回空链");
        return Ok(vec![TraitEvidenceChainView {
            trait_id,
            trait_label,
            total_evidence: 0,
            support_count: 0,
            contradict_count: 0,
            neutral_count: 0,
            evidence_events: vec![],
        }]);
    }

    // 统计证据方向分布
    let support_count = evidence_records
        .iter()
        .filter(|e| matches!(e.direction, EvidenceDirection::Support))
        .count();
    let contradict_count = evidence_records
        .iter()
        .filter(|e| matches!(e.direction, EvidenceDirection::Contradict))
        .count();
    let neutral_count = evidence_records
        .len()
        .saturating_sub(support_count + contradict_count);

    // Step 3: 预加载该 persona 的所有事件（构建 event_id→event 查找表）
    // 使用较大 limit 以覆盖大多数场景；若事件量极大，后续可优化为批量查询
    const MAX_EVENTS_SCAN: i64 = 5000;
    let all_events = storage
        .list_events_by_persona(&persona_uid, 0, MAX_EVENTS_SCAN)
        .await
        .map_err(|e| format!("查询事件列表失败: {}", e))?;

    let event_map: HashMap<i64, &MemoryEvent> = all_events.iter().map(|e| (e.id, e)).collect();

    // Step 4: 逐条构建证据链
    let mut evidence_events: Vec<EventInEvidenceView> = Vec::with_capacity(evidence_records.len());

    for record in &evidence_records {
        // 查找事件
        let event = match event_map.get(&record.event_id) {
            Some(ev) => *ev,
            None => {
                tracing::warn!(
                    event_id = record.event_id,
                    trait_id,
                    "证据记录引用了不存在的事件，跳过"
                );
                continue;
            }
        };

        // 查询事件溯源 L1 记录
        let l1_sources = match storage.list_event_sources_by_event(event.id).await {
            Ok(sources) => sources,
            Err(e) => {
                tracing::warn!(
                    event_id = event.id,
                    error = %e,
                    "查询事件 L1 溯源失败，跳过该事件的 L1 溯源"
                );
                continue;
            }
        };

        // 逐条 L1 溯源构建视图
        let mut l1_source_views: Vec<L1SourceView> = Vec::with_capacity(l1_sources.len());

        for src in &l1_sources {
            match storage.get_memory_l1(src.l1_id).await {
                Ok(Some(l1)) => {
                    l1_source_views.push(L1SourceView {
                        l1_id: l1.id.to_string(),
                        summary: l1.summary,
                        evidence_notes: l1.evidence_notes.unwrap_or_default(),
                        atmosphere: l1.atmosphere,
                        valence: l1.valence,
                        weight: src.weight,
                    });
                }
                Ok(None) => {
                    tracing::warn!(
                        l1_id = %src.l1_id,
                        event_id = event.id,
                        "事件溯源引用了不存在的 L1 记录，跳过"
                    );
                    // 不阻塞：没有 L1 溯源也能展示事件本身的信息
                }
                Err(e) => {
                    tracing::warn!(
                        l1_id = %src.l1_id,
                        event_id = event.id,
                        error = %e,
                        "查询 L1 记录失败，跳过该条溯源"
                    );
                }
            }
        }

        evidence_events.push(EventInEvidenceView {
            event_id: event.id,
            title: event.title.clone(),
            summary: event.summary.clone(),
            confidence: event.confidence,
            valence: event.valence,
            salience: event.salience,
            attitude: event.attitude.clone(),
            paraphrase: event.paraphrase.clone(),
            motives: event.motives.clone(),
            l1_sources: l1_source_views,
        });
    }

    tracing::info!(
        trait_id,
        %persona_uid,
        total = evidence_records.len(),
        events_loaded = evidence_events.len(),
        support = support_count,
        contradict = contradict_count,
        neutral = neutral_count,
        "get_trait_evidence 完成"
    );

    Ok(vec![TraitEvidenceChainView {
        trait_id,
        trait_label,
        total_evidence: evidence_records.len(),
        support_count,
        contradict_count,
        neutral_count,
        evidence_events,
    }])
}

// =========================================================
// get_profile_status — 查询数据状态指示器（M5-C）
// =========================================================

/// 人格画像数据状态视图。
///
/// 职责:
/// - 供前端 MemoryView L3 Tab 顶部渲染数据状态指示器。
/// - 基于有效样本量判定当前画像的可信程度。
///
/// 状态约定:
/// - `insufficient`: 数据不足（n_total_eff < 5），画像不可信，建议继续对话积累数据。
/// - `preliminary`: 初步画像（5 ≤ n_total_eff < 20），画像有一定参考价值但需谨慎。
/// - `trusted`: 可信画像（n_total_eff ≥ 20），画像相对稳定可靠。
#[derive(Debug, Clone, Serialize)]
pub struct ProfileStatusView {
    /// 所属人格标识
    pub persona_uid: String,
    /// 有效样本总量（所有活跃 trait 的 evidence 字段之和）
    pub n_total_eff: f64,
    /// 活跃 trait 数量
    pub active_trait_count: usize,
    /// 状态标识: "insufficient" / "preliminary" / "trusted"
    pub status: String,
    /// 状态描述文本（中文，供前端直接展示）
    pub status_text: String,
}

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
/// - 仅统计 Active 状态的 trait（Deprecated/Historical 不计入）。
///
/// 日志:
/// - INFO: 记录 persona_uid 的 n_total_eff 和状态。
/// - ERROR: 存储层查询失败。
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

    // 验证 persona 存在
    let _persona = state
        .app
        .storage()
        .get_persona_by_uid(&persona_uid)
        .await
        .map_err(|e| format!("查询 persona 失败: {}", e))?
        .ok_or_else(|| format!("人格不存在: uid={persona_uid}"))?;

    // 查询该 persona 的所有 trait
    let traits = state
        .app
        .storage()
        .list_traits_by_persona(&persona_uid)
        .await
        .map_err(|e| format!("查询 L3 性格标签失败: {}", e))?;

    // 仅统计 Active 状态
    let active_traits: Vec<_> = traits
        .into_iter()
        .filter(|t| matches!(t.status, TraitStatus::Active))
        .collect();

    let n_total_eff: f64 = active_traits.iter().map(|t| t.evidence).sum();
    let active_count = active_traits.len();

    // 判定状态区间
    let (status, status_text) = if n_total_eff < 5.0 {
        (
            "insufficient",
            format!(
                "数据不足（有效样本量: {:.1}）—— 继续对话以积累更多数据",
                n_total_eff
            ),
        )
    } else if n_total_eff < 20.0 {
        (
            "preliminary",
            format!(
                "初步画像（有效样本量: {:.1}）—— 画像有一定参考价值，建议继续积累",
                n_total_eff
            ),
        )
    } else {
        (
            "trusted",
            format!(
                "可信画像（有效样本量: {:.1}，共 {} 项性格标签）",
                n_total_eff, active_count
            ),
        )
    };

    tracing::info!(
        %persona_uid,
        n_total_eff,
        active_count,
        status,
        "get_profile_status 完成"
    );

    Ok(ProfileStatusView {
        persona_uid,
        n_total_eff,
        active_trait_count: active_count,
        status: status.to_string(),
        status_text,
    })
}
