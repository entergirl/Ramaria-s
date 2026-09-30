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

/// 知识事实视图（只读展示）。
#[derive(Debug, Clone, Serialize)]
pub struct PersonaFactView {
    /// 事实 id
    pub id: i64,
    /// 字段归属
    pub field: String,
    /// 事实内容（陈述句，非原文）
    pub content: String,
    /// 生命周期状态（active/superseded/candidate）
    pub status: String,
    /// 分层（stable/volatile/historical）
    pub tier: String,
    /// 置信度 0.0..1.0
    pub confidence: f64,
    /// 来源（event/manual/l1）
    pub source: String,
    /// 关键词（判重/检索提示）
    pub keyword_hint: Option<String>,
    /// 覆盖链：被替换事实 id（沿此可展开历史版本）
    pub version_of: Option<i64>,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
}

/// 知识事实查询响应（按 ProfileField 分组的 active 事实 + 版本链）。
#[derive(Debug, Clone, Serialize)]
pub struct FactListView {
    pub persona_uid: String,
    /// 按 field 分组：{ field_label: [PersonaFactView] }
    pub grouped: HashMap<String, Vec<PersonaFactView>>,
    /// 版本链查找：{ fact_id: [旧→新版本链] }（供历史版本折叠展示）
    pub versions: HashMap<i64, Vec<PersonaFactView>>,
}

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
/// - JSON 数组，每项为 PersonalityTraitView。
///
/// 接线状态（未接线/预留）:
/// - 前端记忆页改用 `get_personality_profile`（含三层分层与 trigger/suppress 等字段），
///   未调用本命令；
/// - 保留该命令以提供扁平标签列表，是否接入 UI 或下线由负责人裁定。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_l3_traits(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
) -> Result<Vec<PersonalityTraitView>, String> {
    let traits = state
        .engine
        .memory_l3(persona_uid.as_deref())
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 L3 性格标签失败"))?;

    let views: Vec<PersonalityTraitView> = traits
        .into_iter()
        .map(|t| PersonalityTraitView {
            id: t.id,
            persona_uid: t.persona_uid,
            layer: t.layer.as_str().to_string(),
            label: t.label,
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
/// - 服务层用例遍历所有 persona，检查未吸收 L1 是否达到阈值 → 触发 L2 事件提取。
/// - L2 提取成功后自动级联 L3 性格推断。
/// - 适用于快速导入后，用户手动启动深度处理。
/// - 此操作为异步后台任务，返回"已启动"即表示成功提交。
///
/// 返回:
/// - `"ok"`: 管线已触发，后台异步执行。
///
/// 接线状态（未接线/预留）:
/// - 前端当前经导入流程与封存后的自动触发进入 L2/L3，未提供手动触发入口；
/// - 保留该命令供人工补救（快速导入后手动启动深度处理），是否接入 UI 或下线由负责人裁定。
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

impl From<ramaria_service::TraitDetailView> for TraitDetailView {
    fn from(t: ramaria_service::TraitDetailView) -> Self {
        Self {
            id: t.id,
            label: t.label,
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

/// 将服务层事实视图转换为前端视图（不含内部字段）。
fn fact_to_view(f: ramaria_service::FactEntryView) -> PersonaFactView {
    PersonaFactView {
        id: f.id,
        field: f.field.as_str().to_string(),
        content: f.content,
        status: f.status.as_str().to_string(),
        tier: f.tier.as_str().to_string(),
        confidence: f.confidence,
        source: f.source.as_str().to_string(),
        keyword_hint: f.keyword_hint,
        version_of: f.version_of,
        created_at: f.created_at,
    }
}
