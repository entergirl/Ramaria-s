//! crates/ramaria-desktop/src/commands/import_cmd/analyze.rs - Ramaria 导入文件解析命令
//!
//! 设计特点:
//! - 只做格式检测与解析，不写入数据库（前端预览步骤）
//! - 路径安全校验复用 path_guard，日志只记脱敏后的路径标签
//! - 解析统计只记数量，不记 QQ 号 / UID / 昵称

use crate::DesktopState;
use ramaria_service::AnalyzeRequest;
use tauri::State;

use super::result::AnalysisReport;

// =========================================================
// analyze_qq_chat — 解析文件并返回报告（不写入数据库）
// =========================================================

/// 解析 QQ 聊天记录文件，返回诊断报告（不执行导入写入）。
///
/// 参数:
/// - `file_path`: 聊天记录文件的绝对路径。
/// - `gap_minutes`: session 切割时间间隔（分钟），默认 10。
///
/// 返回:
/// - `AnalysisReport` JSON，包含解析统计信息。
///
/// 说明:
/// - 仅执行格式检测和文件解析，不写入数据库。
/// - 用于前端"预览"步骤，让用户在导入前了解文件内容。
#[tauri::command]
#[tracing::instrument(skip(state, file_path))]
pub async fn analyze_qq_chat(
    state: State<'_, DesktopState>,
    file_path: String,
    gap_minutes: Option<u32>,
) -> Result<AnalysisReport, String> {
    let gap = gap_minutes.unwrap_or(10);

    // 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝）
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    tracing::info!(
        gap_minutes = gap,
        file = %crate::path_guard::redact_path_label(&real_path),
        "开始解析 QQ 聊天记录文件"
    );

    // 格式检测（内容层面；错误前缀与既有口径一致）
    let is_qq = state
        .engine
        .detect_qq_format(&real_path)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "格式检测失败"))?;

    if !is_qq {
        tracing::warn!("文件格式不是 QQ 聊天记录");
        return Err(format!("文件 '{}' 不是 QQ 聊天记录格式", file_path));
    }

    // 解析（不写入数据库）
    let report = state
        .engine
        .analyze_qq_import(AnalyzeRequest {
            file_path: real_path,
            gap_minutes: gap,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "文件解析失败"))?;

    // 隐私红线：QQ 号/UID 属个人标识，info 级日志不记录（仅记录量与统计信息）
    tracing::info!(
        total_raw = report.total_raw,
        success = report.total_success,
        degraded = report.total_degraded,
        skipped = report.total_skipped,
        sessions = report.session_count,
        "QQ 文件解析完成"
    );

    Ok(AnalysisReport {
        file_path,
        self_id: report.self_id,
        self_name: report.self_name,
        self_uin: report.self_uin,
        chat_name: report.chat_name,
        chat_type: report.chat_type,
        members: report.members,
        other_name: report.other_name,
        other_uid: report.other_uid,
        other_uin: report.other_uin,
        time_range: report.time_range,
        total_raw: report.total_raw,
        total_success: report.total_success,
        total_degraded: report.total_degraded,
        total_skipped: report.total_skipped,
        session_count: report.session_count,
        gap_minutes: report.gap_minutes,
    })
}
