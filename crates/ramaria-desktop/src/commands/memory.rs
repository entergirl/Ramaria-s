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
use serde::Serialize;
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
