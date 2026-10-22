//! crates/ramaria-desktop/src/commands/import_cmd/run.rs - Ramaria QQ 聊天记录导入执行命令
//!
//! 设计特点:
//! - L0 写入同步完成，L1 摘要与 L2/L3 级联在后台任务执行，进度经事件推送
//! - 导入模式 fast 只生成 L1，deep 追加深度级联
//! - 批量 LLM 请求间按配置节流，读取失败降级为不节流
//! - 日志只记数量与脱敏后的 persona 标识，不记 QQ 号 / 昵称 / 路径原值

use crate::DesktopState;
use ramaria_core::privacy::mask_id;
use ramaria_service::{
    ImportDoneSummary, ImportMode, ImportPostPlan, ImportPostRequest, ImportProgressSink,
    ImportRequest,
};
use tauri::{AppHandle, State};

use super::progress::TauriImportProgressSink;
use super::result::ImportResult;

// =========================================================
// import_qq_chat — 执行 QQ 聊天记录导入
// =========================================================

/// 执行 QQ 聊天记录导入。
///
/// Tauri Command 参数由前端逐个传递，参数数膨胀是合理的架构取舍。
///
/// 参数:
/// - `file_path`: 聊天记录文件的绝对路径（qq-chat-exporter v6.x JSON）。
/// - `mode`: 导入模式，"fast"（仅 L0）或 "deep"（全管线）。
/// - `persona_name`: 可选，导出者 persona 显示名称。如果不提供，使用导出者名称。
/// - `self_persona_uid`: 可选，导出者 persona UID（留空按优先级自动生成）。
/// - `other_persona_name`: 可选，对方 persona 显示名称。如果不提供，使用文件中解析的对方名称。
/// - `other_persona_uid`: 可选，对方 persona UID（留空按优先级自动生成）。
/// - `gap_minutes`: session 切割时间间隔（分钟），默认 10。
/// - `side`: 导入侧过滤（"self" / "other" / "both"，缺失或非法回退 both）。
///
/// 返回:
/// - `ImportResult` JSON 对象，包含报告摘要、统计信息和双画像标识。
///
/// 说明:
/// - L0 写入同步完成；L1 摘要与 L2/L3 级联在后台任务中执行，进度经
///   `import-progress` 事件推送（前端契约不变）。
#[tauri::command]
#[tracing::instrument(skip(
    state,
    app_handle,
    file_path,
    persona_name,
    self_persona_uid,
    other_persona_name,
    other_persona_uid
))]
#[allow(clippy::too_many_arguments)]
pub async fn import_qq_chat(
    state: State<'_, DesktopState>,
    app_handle: AppHandle,
    file_path: String,
    mode: Option<String>,
    persona_name: Option<String>,
    self_persona_uid: Option<String>,
    other_persona_name: Option<String>,
    other_persona_uid: Option<String>,
    gap_minutes: Option<u32>,
    side: Option<String>,
) -> Result<ImportResult, String> {
    // ---- 参数解析 ----
    let mode_str = mode.unwrap_or_else(|| "fast".to_string());
    let import_mode = match mode_str.as_str() {
        "fast" => ImportMode::Fast,
        "deep" => ImportMode::Deep,
        other => {
            return Err(format!(
                "不支持的导入模式: {}（仅支持 fast 或 deep）",
                other
            ));
        }
    };
    let gap = gap_minutes.unwrap_or(10);
    // 导入侧过滤：前端面板传入 "self"/"other"/"both"；缺失/非法回退 both
    let import_side = ramaria_importer::qq::ImportSide::parse_cli(side.as_deref())
        .unwrap_or(ramaria_importer::qq::ImportSide::Both);

    // ---- Step 1: 路径安全校验（三层防御：canonicalize + 白名单 + 符号链接拒绝） ----
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    tracing::info!(
        file = %crate::path_guard::redact_path_label(&real_path),
        mode = %mode_str,
        gap_minutes = gap,
        "开始 QQ 聊天记录导入"
    );

    // ---- Step 2: 格式检测（内容层面；错误前缀与既有口径一致，快速失败） ----
    let is_qq = state
        .engine
        .detect_qq_format(&real_path)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "格式检测失败"))?;

    if !is_qq {
        return Err(format!(
            "文件 '{}' 不是 QQ 聊天记录格式。请确认文件来自 qq-chat-exporter v6.x 导出的 JSON。",
            file_path
        ));
    }

    // 附件定位根（供 L0 后图片理解使用）：导出 JSON 所在目录
    let export_root = real_path.parent().map(|dir| dir.to_path_buf());

    // ---- Step 3: L0 导入（解析 → 双画像准备 → 会话与消息写入） ----
    let outcome = state
        .engine
        .import_qq_l0(ImportRequest {
            file_path: real_path,
            mode: import_mode,
            gap_minutes: gap,
            side: import_side,
            persona_name,
            self_persona_uid,
            other_persona_name,
            other_persona_uid,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "导入失败"))?;

    tracing::info!(
        sessions_written = outcome.sessions_written,
        messages_written = outcome.messages_written,
        "L0 写入完成"
    );

    // ---- Step 4: L1 批量生成与深度处理（后台异步，避免阻塞前端） ----
    // 分发口径：恒含"我"（导出者）在内、块内参与者各一行；每会话一次生成（忽略 targets）。
    // 批量 LLM 请求间最小间隔（毫秒）：读当前生效配置的 `[thresholds].cluster_delay_ms`，
    // 导入会连续多次调用 LLM，无节流时易触发远程 API 速率限制。
    // 读取失败时降级为 0（不阻塞导入）。
    let throttle_ms = state
        .engine
        .load_full_config()
        .await
        .map(|cfg| cfg.thresholds.cluster_delay_ms)
        .unwrap_or(0);

    let engine = state.engine.clone();
    let sids = outcome.session_ids.clone();
    let total_sids = sids.len();
    let is_deep = import_mode == ImportMode::Deep;
    let handle = app_handle;

    tokio::spawn(async move {
        let sink = TauriImportProgressSink { app_handle: handle };

        // 导入后处理单一编排：图片理解（先理解后 L1）→ L1 批量生成 → 深度触发。
        // 图片理解失败自动降级；起始进度与逐 session 进度由服务层用例发出，宿主只转发。
        let post = match engine
            .run_import_post(
                ImportPostRequest {
                    session_ids: sids,
                    export_root,
                    plan: ImportPostPlan {
                        l1_targets: Vec::new(),
                        l1_prefix: None,
                        group_fanout: true,
                        fanout_include_self: true,
                        l1_cascade: false,
                        cascade_deep: is_deep,
                        throttle_ms,
                    },
                },
                Some(&sink),
            )
            .await
        {
            Ok(post) => post,
            Err(e) => {
                // 理论不可达：服务层把单次生成失败转为计数，不抛错；防御性收束
                tracing::error!(error = %e, "导入 L1 批量生成失败");
                let summary = ImportDoneSummary {
                    l1_success: 0,
                    l1_failed: 0,
                    l2_triggered: false,
                    l3_triggered: false,
                    total_sessions: total_sids,
                    message: crate::commands::service_error_message(&e, "L1 批量生成失败"),
                };
                sink.on_done(&summary);
                return;
            }
        };

        // 完成摘要（文案由服务层提供，与既有 done 事件口径一致）
        let summary = ramaria_service::import::done_summary(
            &post.l1,
            post.l2_triggered,
            post.l3_triggered,
            total_sids,
        );
        sink.on_done(&summary);
    });

    // ---- Step 5: 构建返回结果 ----
    let session_id_strings: Vec<String> = outcome
        .session_ids
        .iter()
        .map(|id| id.to_string())
        .collect();

    let result = ImportResult {
        success: true,
        mode: mode_str.clone(),
        report_summary: outcome.report_summary,
        sessions_written: outcome.sessions_written,
        messages_written: outcome.messages_written,
        persona_uid: outcome.persona_uid,
        persona_name: outcome.persona_name,
        other_persona_uid: outcome.other_persona_uid,
        other_persona_name: outcome.other_persona_name,
        self_name: outcome.self_name,
        chat_name: outcome.chat_name,
        chat_type: outcome.chat_type,
        members: outcome.members,
        time_range: outcome.time_range,
        skipped_count: outcome.skipped_count,
        session_ids: session_id_strings,
    };

    tracing::info!(
        sessions = result.sessions_written,
        messages = result.messages_written,
        mode = %result.mode,
        chat_type = %result.chat_type,
        member_count = result.members.len(),
        self_persona = ?result.persona_uid.as_deref().map(mask_id),
        other_persona = ?result.other_persona_uid.as_deref().map(mask_id),
        "QQ 聊天记录导入完成"
    );

    Ok(result)
}
