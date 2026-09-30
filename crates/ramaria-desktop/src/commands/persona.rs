//! crates/ramaria-desktop/src/commands/persona.rs - 人格管理 Tauri Commands
//!
//! 设计特点:
//! - 提供完整的人格管理前端接口：列表（全字段）、编辑、刷新
//! - 委托服务层人格用例（列表 / 信息更新 / L1 重生成），桌面只做视图映射与后台级联触发
//! - 返回值经过序列化，隐藏内部 id，暴露业务字段
//! - 与 memory 模块的 `get_personas` 互补：前者返回摘要，本模块返回全字段
//! - `refresh_persona` 触发记忆管线（L2→L3），用于"重载"性格画像
//! - `regenerate_import_pipeline` 重新生成导入 session 的 L1 摘要 + 级联 L2/L3

use crate::DesktopState;
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// Persona 完整信息视图。
///
/// 与 `memory::PersonaView` 的区别:
/// - 包含 `ref_id`、`avatar`、`config`、`description`、`updated_at` 等完整字段
/// - 用于人格管理 GUI 的详情编辑页
#[derive(Debug, Clone, Serialize)]
pub struct PersonaFullView {
    /// 业务标识，如 `user-0001`、`rama-0001`
    pub uid: String,
    /// 显示名称
    pub name: String,
    /// 类型: user / rama / char / anim / oc / hist
    pub kind: String,
    /// 来源渠道: local / qq / wechat / telegram / manual / network
    pub source: String,
    /// 来源方原始 ID（跨渠道去重用）
    pub ref_id: Option<String>,
    /// 头像 URL 或路径
    pub avatar: Option<String>,
    /// JSON 个性配置（完整内容）
    pub config: Option<String>,
    /// 人格简要描述文本
    pub description: Option<String>,
    /// 是否启用
    pub is_active: bool,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 最后更新时间（Unix 毫秒）
    pub updated_at: i64,
}

// =========================================================
// list_personas_full — 列出所有人格（全字段）
// =========================================================

/// 列出所有已注册人格的完整信息。
///
/// 与 `get_personas` 命令对比:
/// - `get_personas`: 返回摘要视图 (uid/name/kind/source/is_active/created_at)
/// - `list_personas_full`: 返回完整视图 (含 ref_id/avatar/config/description/updated_at)
///
/// 返回:
/// - JSON 数组，每项为 PersonaFullView
///
/// 说明:
/// - 按 kind, seq 排序
/// - 适用于人格管理 GUI 的卡片网格展示
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_personas_full(
    state: State<'_, DesktopState>,
) -> Result<Vec<PersonaFullView>, String> {
    let personas = state
        .engine
        .persona_list_full()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 persona 完整列表失败"))?;

    let views: Vec<PersonaFullView> = personas.into_iter().map(full_view).collect();

    tracing::debug!(count = views.len(), "list_personas_full 完成");
    Ok(views)
}

// =========================================================
// update_persona_info — 更新人格基本信息
// =========================================================

/// 更新指定人格的基本信息（名称、头像、描述）。
///
/// 与 `update_persona` Storage trait 方法对比:
/// - 本命令面向前端，仅暴露用户可编辑的字段（name/avatar/description）
/// - `config` 由人格文件导入通道管理，不在本命令修改
///
/// 参数:
/// - `uid`: 人格业务标识（如 "rama-0001"），不可变更
/// - `request`: PersonaUpdateRequest，所有字段可选
///
/// 返回:
/// - 更新后的 PersonaFullView
///
/// 边界处理:
/// - `uid` 不存在时返回错误
/// - `description` 传空字符串视为清空描述（与 None 行为不同）
/// - `name` 未提供时沿用旧值
///
/// 日志:
/// - INFO: 记录更新操作的目标 persona_uid
#[tauri::command]
#[tracing::instrument(skip(state, request))]
pub async fn update_persona_info(
    state: State<'_, DesktopState>,
    uid: String,
    request: ramaria_service::PersonaUpdateRequest,
) -> Result<PersonaFullView, String> {
    // 参数校验: uid 不可为空
    if uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    let updated = state
        .engine
        .persona_update_info(&uid, request)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "更新 persona 失败"))?;

    tracing::info!(%uid, "update_persona_info 完成");
    Ok(full_view(updated))
}

// =========================================================
// refresh_persona — 刷新指定人格的记忆管线
// =========================================================

/// 触发指定 persona 的 L2→L3 记忆管线。
///
/// 说明:
/// - 对指定人格执行 L2 事件提取（如未吸收 L1 达到阈值）→ 级联 L3 性格推断。
/// - 与 `trigger_memory_pipeline` 不同：后者面向全部 persona 的补救入口；
///   本命令面向人格管理页的"重载"按钮（底层同一次全量检查，满足条件的 persona 才会被处理）。
/// - 此操作为异步后台任务，返回"ok"即表示已提交，不等待执行完成。
///
/// 参数:
/// - `uid`: 目标人格业务标识
///
/// 返回:
/// - `"ok"`: 管线已触发，后台异步执行
///
/// 边界处理:
/// - `uid` 不存在时返回错误
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn refresh_persona(
    state: State<'_, DesktopState>,
    uid: String,
) -> Result<String, String> {
    // 参数校验: uid 不可为空
    if uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    // 验证 persona 存在
    let personas = state
        .engine
        .persona_list()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 persona 失败"))?;
    if !personas.iter().any(|p| p.uid == uid) {
        return Err(format!("人格不存在: uid={uid}"));
    }

    tracing::info!(%uid, "手动触发 persona 记忆管线（L2→L3）");

    let engine = state.engine.clone();
    let uid_clone = uid.clone();
    tokio::spawn(async move {
        engine.trigger_l2_check().await;
        tracing::info!(%uid_clone, "persona 记忆管线后台任务已启动");
    });

    Ok("ok".to_string())
}

// =========================================================
// regenerate_import_pipeline — 重新生成导入消息的 L1 摘要并级联 L2/L3
// =========================================================

/// 对导入 persona 的所有 session 重新生成 L1 摘要，然后触发 L2→L3 级联。
///
/// 动机:
/// - 导入时若 LLM 不可用，L1 摘要生成会失败（静默 WARN）。
/// 用户连接 LLM 后，可通过记忆页面的"深度处理导入的消息"按钮调用本命令。
/// - 与 `trigger_memory_pipeline` 的区别：本命令先重新生成 L1，
/// 再触发 L2 检查，确保 LLM 失败场景下的 L0→L1→L2→L3 全管线可恢复。
///
/// 参数:
/// - `persona_uid`: 目标导入 persona 的 UID（如 "char-123456789"）。
///
/// 返回:
/// - JSON: `{ "l1_regenerated": N, "l1_failed": N, "message": "..." }`
///
/// 说明:
/// - 幂等：已存在的 L1 摘要按跳过处理（服务层用例负责计数）。
/// - L1 摘要 persona_uid 关联到目标导入 persona（不再存 NULL，令 L2/L3 可触发）。
/// - 此操作为异步后台任务：返回后 L1 已生成，L2/L3 后台继续执行。
///
/// 日志:
/// - INFO: 记录触发操作的目标 persona_uid
/// - WARN: 单条 L1 生成失败时由服务层记录（非阻塞）
/// - ERROR: persona 不存在或存储查询失败
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn regenerate_import_pipeline(
    state: State<'_, DesktopState>,
    persona_uid: String,
) -> Result<serde_json::Value, String> {
    // 参数校验
    if persona_uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    tracing::info!(%persona_uid, "重新生成导入 session 的 L1 摘要并级联 L2/L3");

    let outcome = state
        .engine
        .regenerate_persona_l1(&persona_uid)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 persona 失败"))?;

    // 级联 L2→L3（后台异步，避免阻塞前端）：
    // 无关联会话时无需级联；即使 L1 部分失败也触发——已有 L2/L3 数据不受影响。
    if outcome.total_sessions > 0 {
        let engine = state.engine.clone();
        let persona_uid_clone = persona_uid.clone();
        tokio::spawn(async move {
            engine.trigger_l2_check().await;
            tracing::info!(%persona_uid_clone, "导入消息深度处理管线（L2→L3）已触发");
        });
    }

    Ok(serde_json::json!({
        "l1_regenerated": outcome.l1_regenerated,
        "l1_failed": outcome.l1_failed,
        "total_sessions": outcome.total_sessions,
        "early_terminated": outcome.early_terminated,
        "remaining_skipped": outcome.remaining_skipped,
        "message": outcome.message,
    }))
}

/// 服务层人格全字段视图 → 前端视图（字段映射的唯一入口）。
fn full_view(persona: ramaria_service::PersonaFullView) -> PersonaFullView {
    PersonaFullView {
        uid: persona.uid,
        name: persona.name,
        kind: persona.kind,
        source: persona.source,
        ref_id: persona.ref_id,
        avatar: persona.avatar,
        config: persona.config,
        description: persona.description,
        is_active: persona.is_active,
        created_at: persona.created_at,
        updated_at: persona.updated_at,
    }
}
