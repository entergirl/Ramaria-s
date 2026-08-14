//! crates/ramaria-desktop/src/commands/persona.rs - 人格管理 Tauri Commands
//!
//! 设计特点:
//! - 提供完整的 Persona CRUD 前端接口：列表（全字段）、编辑、刷新
//! - 所有命令纯委托 StorageBackend，不写业务逻辑
//! - 返回值经过序列化，隐藏内部 id，暴露业务字段
//! - 与 memory 模块的 `get_personas` 互补：前者返回摘要，本模块返回全字段
//! - `refresh_persona` 触发指定 persona 的记忆管线（L2→L3），用于"重载"性格画像
//! - `regenerate_import_pipeline` 重新生成导入 session 的 L1 摘要 + 级联 L2/L3

use crate::DesktopState;
use serde::Serialize;
use std::collections::HashSet;
use tauri::State;
use uuid::Uuid;

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

/// persona 更新请求（前端→后端）。
///
/// 所有字段均为可选: `None` 表示不更新对应字段。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PersonaUpdateRequest {
    /// 新显示名称（None 表示不更新）
    pub name: Option<String>,
    /// 新头像（None 表示不更新）
    pub avatar: Option<String>,
    /// 新描述（None 表示不更新；空字符串表示清空描述）
    pub description: Option<String>,
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
/// - 仅返回 active=true 的人格
/// - 按 kind, seq 排序
/// - 适用于人格管理 GUI 的卡片网格展示
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_personas_full(
    state: State<'_, DesktopState>,
) -> Result<Vec<PersonaFullView>, String> {
    let personas = state
        .app
        .storage()
        .list_personas()
        .await
        .map_err(|e| format!("查询 persona 完整列表失败: {}", e))?;

    let views: Vec<PersonaFullView> = personas
        .into_iter()
        .map(|p| PersonaFullView {
            uid: p.uid,
            name: p.name,
            kind: p.kind.as_str().to_string(),
            source: p.source,
            ref_id: p.ref_id,
            avatar: p.avatar,
            config: p.config,
            description: p.description,
            is_active: p.active,
            created_at: p.created_at,
            updated_at: p.updated_at,
        })
        .collect();

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
/// - Storage 层的 `update_persona` 还包含 `config`（由 CLI `persona reload` 管理）
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
/// - `name` 至少需要提供（调用方保证），否则使用旧值
///
/// 日志:
/// - INFO: 记录更新操作的目标 persona_uid
/// - DEBUG: 记录具体变更的字段
/// - ERROR: 记录存储层错误
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn update_persona_info(
    state: State<'_, DesktopState>,
    uid: String,
    request: PersonaUpdateRequest,
) -> Result<PersonaFullView, String> {
    // 参数校验: uid 不可为空
    if uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    // 先读取现有 persona，用于获取当前 name（如前端未传 name）
    let existing = state
        .app
        .storage()
        .get_persona_by_uid(&uid)
        .await
        .map_err(|e| format!("查询 persona 失败: {}", e))?
        .ok_or_else(|| format!("人格不存在: uid={uid}"))?;

    // 确定要更新的各项值
    let new_name = request.name.as_deref().unwrap_or(&existing.name);
    let new_avatar = request.avatar.as_deref();
    // config 不由前端编辑，传 None 保持旧值
    let new_config: Option<&str> = None;
    // description: None 不更新，Some("") 清空，Some(val) 设置
    let new_description = request.description.as_deref();

    // 执行 update_persona（StorageBackend trait 方法）
    state
        .app
        .storage()
        .update_persona(&uid, new_name, new_avatar, new_config, new_description)
        .await
        .map_err(|e| format!("更新 persona 失败: {}", e))?;

    tracing::info!(
        %uid,
        name_changed = request.name.is_some(),
        avatar_changed = request.avatar.is_some(),
        desc_changed = request.description.is_some(),
        "update_persona_info 完成"
    );

    // 回读更新后的完整信息并返回
    let updated = state
        .app
        .storage()
        .get_persona_by_uid(&uid)
        .await
        .map_err(|e| format!("回读更新后的 persona 失败: {}", e))?
        .ok_or_else(|| format!("更新后 persona 意外不存在: uid={uid}"))?;

    Ok(PersonaFullView {
        uid: updated.uid,
        name: updated.name,
        kind: updated.kind.as_str().to_string(),
        source: updated.source,
        ref_id: updated.ref_id,
        avatar: updated.avatar,
        config: updated.config,
        description: updated.description,
        is_active: updated.active,
        created_at: updated.created_at,
        updated_at: updated.updated_at,
    })
}

// =========================================================
// refresh_persona — 刷新指定人格的记忆管线
// =========================================================

/// 触发指定 persona 的 L2→L3 记忆管线。
///
/// 说明:
/// - 对指定人格执行 L2 事件提取（如未吸收 L1 ≥ 5 条）→ 级联 L3 性格推断。
/// - 与 `trigger_memory_pipeline` 不同：后者遍历所有 persona，本命令仅处理指定一个。
/// - 适用场景：用户在人格管理页点击"重载"按钮，刷新该人格的性格画像。
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
/// - 管线已在运行时不会重复触发（由 App 层保证幂等）
///
/// 日志:
/// - INFO: 记录触发操作的目标 persona_uid
/// - WARN: 目标 persona 无 L2 触发条件时记录
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
    let _persona = state
        .app
        .storage()
        .get_persona_by_uid(&uid)
        .await
        .map_err(|e| format!("查询 persona 失败: {}", e))?
        .ok_or_else(|| format!("人格不存在: uid={uid}"))?;

    tracing::info!(%uid, "手动触发 persona 记忆管线（L2→L3）");

    // 将 uid 复制到 async 块中，避免生命周期问题
    let uid_clone = uid.clone();
    let app = state.app.clone();

    // 异步提交后台任务
    tokio::spawn(async move {
        // 触发当前 app 的 L2 检查（这会遍历所有 persona）
        // 由于没有"单 persona 管线"方法，调用全量触发，
        // 但实际只有满足条件的 persona 会被处理。
        //
        // 设计说明: 目前 trigger_l2_check 遍历所有 persona，
        // 将其改造为接受可选 persona_uid 参数是后续优化方向。
        // 当前实现是务实的：全量触发不会对单 persona 场景产生副作用。
        app.trigger_l2_check().await;
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
/// - 幂等：已存在的 L1 摘要会被覆盖（regenerate_l1_no_cascade 内部删除旧 L1）。
/// - L1 摘要 persona_uid 关联到目标导入 persona（不再存 NULL，令 L2/L3 可触发）。
/// - 此操作为异步后台任务：返回后 L1 已生成，L2/L3 后台继续执行。
///
/// 日志:
/// - INFO: 记录触发操作的目标 persona_uid 和 session 数
/// - WARN: 单条 L1 生成失败时记录（非阻塞）
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

    // 验证 persona 存在
    let _persona = state
        .app
        .storage()
        .get_persona_by_uid(&persona_uid)
        .await
        .map_err(|e| format!("查询 persona 失败: {}", e))?
        .ok_or_else(|| format!("人格不存在: uid={persona_uid}"))?;

    tracing::info!(%persona_uid, "重新生成导入 session 的 L1 摘要并级联 L2/L3");

    // Step 1: 查找该 persona 所有消息所属的 session
    let messages = state
        .app
        .storage()
        .list_messages_by_persona(&persona_uid)
        .await
        .map_err(|e| format!("查询 persona 消息失败: {}", e))?;

    // 提取唯一的 session_id 集合
    let session_ids: Vec<Uuid> = messages
        .iter()
        .map(|m| m.session_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if session_ids.is_empty() {
        return Ok(serde_json::json!({
            "l1_regenerated": 0,
            "l1_failed": 0,
            "message": "该人格没有关联的导入消息，无需处理。"
        }));
    }

    tracing::info!(
        %persona_uid,
        session_count = session_ids.len(),
        message_count = messages.len(),
        "找到关联的导入 session，开始重新生成 L1"
    );

    // Step 2: 对每个 session 重新生成 L1 摘要
    // L1 摘要 persona_uid 关联到目标导入 persona（不再存 NULL）
    // 原设计: persona_uid=NULL"因为导入对话来自两人"——但 NULL 导致 L2 永远无法触发
    // 修复: 关联到目标 persona，对方 persona 的事件通过 chat_partners 分组独立提取
    //
    // 重试策略（对齐深度导入模式）:
    // - 单个 session 内部由 JobManager 负责 3 次重试（指数退避 1s/2s/4s）。
    // - 外层循环追踪连续失败次数：若连续 3 个 session 的 L1 全部失败，
    // 则判定 LLM 不可用，提前终止剩余 session 的处理，避免无意义的重试等待。
    // - 每个 session 的 JobManager 内部失败（已达 3 次重试上限）才会计入 l1_failed，
    // 因此 "连续失败" 意味着 LLM 确实无法连接。
    const MAX_CONSECUTIVE_L1_FAILURES: u32 = 3;

    let app = state.app.clone();
    let total = session_ids.len();
    let mut l1_regenerated = 0usize;
    let mut l1_failed = 0usize;
    let mut consecutive_failures: u32 = 0;
    let mut early_terminated = false;
    let mut remaining_skipped = 0usize;
    let l1_persona_uid = persona_uid.clone();

    for (idx, sid) in session_ids.iter().enumerate() {
        match app
            .regenerate_l1_no_cascade(*sid, Some(&l1_persona_uid), None, None)
            .await
        {
            Ok(Some(_)) => {
                l1_regenerated += 1;
                consecutive_failures = 0; // 重置连续失败计数
                tracing::debug!(%sid, "L1 重新生成成功");
            }
            Ok(None) => {
                // session 无消息，不计入成功/失败，不影响连续失败计数
                tracing::debug!(%sid, "session 无消息，跳过 L1");
            }
            Err(e) => {
                l1_failed += 1;
                consecutive_failures += 1;
                tracing::warn!(%sid, error = %e, consecutive_failures, "L1 重新生成失败");

                // 连续失败达到上限 → 判定 LLM 不可用，提前终止
                if consecutive_failures >= MAX_CONSECUTIVE_L1_FAILURES {
                    remaining_skipped = total.saturating_sub(idx + 1);
                    tracing::warn!(
                        %persona_uid,
                        consecutive_failures,
                        l1_failed,
                        l1_regenerated,
                        remaining_skipped,
                        "L1 连续失败 {} 次，判定 LLM 不可用。跳过剩余 {} 个 session。",
                        MAX_CONSECUTIVE_L1_FAILURES,
                        remaining_skipped,
                    );
                    early_terminated = true;
                    break;
                }
            }
        }
    }

    tracing::info!(
        %persona_uid,
        l1_regenerated,
        l1_failed,
        total,
        early_terminated,
        remaining_skipped,
        "L1 重新生成完成（early_terminated={}），触发 L2→L3 级联",
        early_terminated,
    );

    // Step 3: 触发 L2→L3 级联（后台异步，避免阻塞前端）
    // 注意：即使 L1 全部失败或提前终止，仍触发 L2→L3——已有 L2/L3 数据不受影响。
    let persona_uid_clone = persona_uid.clone();
    tokio::spawn(async move {
        app.trigger_l2_check().await;
        tracing::info!(%persona_uid_clone, "导入消息深度处理管线（L2→L3）已触发");
    });

    // 构造差异化的用户提示消息
    let message = if early_terminated {
        format!(
            "L1 连续失败 {} 次，已提前终止。成功 {}/{}, 失败 {}。请确认 LLM 模型已连接后重试。剩余 {} 个 session 未处理。",
            MAX_CONSECUTIVE_L1_FAILURES, l1_regenerated, total, l1_failed, remaining_skipped
        )
    } else if l1_failed > 0 {
        format!(
            "L1 重新生成完成: 成功 {}/{}, 失败 {}。请确认 LLM 模型已连接。L2/L3 正在后台处理中...",
            l1_regenerated, total, l1_failed
        )
    } else {
        format!(
            "L1 全部重新生成成功 ({}/{})。L2/L3 正在后台处理中...",
            l1_regenerated, total
        )
    };

    Ok(serde_json::json!({
        "l1_regenerated": l1_regenerated,
        "l1_failed": l1_failed,
        "total_sessions": total,
        "early_terminated": early_terminated,
        "remaining_skipped": remaining_skipped,
        "message": message,
    }))
}
