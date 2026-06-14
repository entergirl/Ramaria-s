//! rust/crates/ramaria-desktop/src/commands/import_cmd.rs - QQ 聊天记录导入 Tauri Command
//!
//! 设计特点:
//! - `import_qq_chat`: 接收文件路径、导入模式和 persona 参数，委托 ramaria-importer 执行解析与写入
//! - `detect_qq_format`: 检测文件是否为 QQ 聊天记录支持的格式（JSON 或 .txt）
//! - 快速导入（fast）：仅写入 messages 表（L0），适合快速预览历史对话
//! - 深度导入（deep）：创建历史 session → 写入 L0 → 关闭 session → 触发全管线
//! - Persona 归属：自动查找或创建 source="qq" 的 persona
//! - 路径安全校验：文件存在性检查 + 扩展名白名单
//! - 所有 Tauri Command 只做参数转换 + 委托业务逻辑，不直接操作数据库

use crate::DesktopState;
use crate::events::{EVENT_IMPORT_PROGRESS, ImportProgressPayload};
use ramaria_importer::ImportSource;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

// =========================================================
// 导入结果结构体
// =========================================================

/// 导入操作的完整结果，序列化后返回给前端展示。
#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    /// 是否成功
    pub success: bool,
    /// 导入模式：fast / deep
    pub mode: String,
    /// 解析报告摘要（人类可读文本）
    pub report_summary: String,
    /// 写入的 session 数
    pub sessions_written: usize,
    /// 写入的消息总数
    pub messages_written: usize,
    /// 使用的 persona_uid
    pub persona_uid: String,
    /// persona 名称
    pub persona_name: String,
    /// 导出者名称（从文件中解析）
    pub self_name: String,
    /// 对话对象名称
    pub chat_name: String,
    /// 对话时间范围（如 "2023-01-01 ~ 2024-06-30"）
    pub time_range: String,
    /// 跳过的消息数（撤回+空+未知类型）
    pub skipped_count: usize,
}

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
/// - 与 `import_qq_chat` 共享解析逻辑，但跳过 persona 创建和消息写入。
#[tauri::command]
#[tracing::instrument(skip(_state))]
pub async fn analyze_qq_chat(
    _state: State<'_, DesktopState>,
    file_path: String,
    gap_minutes: Option<u32>,
) -> Result<AnalysisReport, String> {
    use std::path::Path;

    let gap = gap_minutes.unwrap_or(10);
    let path = Path::new(&file_path);

    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    if !path.is_file() {
        return Err(format!("路径不是文件: {}", file_path));
    }

    let importer = ramaria_importer::qq::QqImporter::new();

    let is_qq = importer
        .detect_format(path)
        .map_err(|e| format!("格式检测失败: {}", e))?;

    if !is_qq {
        return Err(format!("文件 '{}' 不是 QQ 聊天记录格式", file_path));
    }

    let (_sessions, report) = importer
        .parse(path, gap)
        .map_err(|e| format!("文件解析失败: {}", e))?;

    let total_success = report.total_success();
    let total_degraded = report.total_degraded();
    let total_skipped = report.total_skipped();

    let time_range = if report.time_start.is_empty() || report.time_end.is_empty() {
        "未知".to_string()
    } else {
        format!("{} ~ {}", report.time_start, report.time_end)
    };

    Ok(AnalysisReport {
        file_path,
        self_id: report.self_id,
        self_name: report.self_name,
        chat_name: report.chat_name,
        chat_type: report.chat_type,
        time_range,
        total_raw: report.total_raw,
        total_success,
        total_degraded,
        total_skipped,
        session_count: report.session_count,
        gap_minutes: gap,
    })
}

/// 文件分析报告（不含导入相关的统计，仅描述文件内容）。
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    /// 文件路径
    pub file_path: String,
    /// 导出者 ID
    pub self_id: String,
    /// 导出者名称
    pub self_name: String,
    /// 对话对象名称
    pub chat_name: String,
    /// 对话类型
    pub chat_type: String,
    /// 时间范围
    pub time_range: String,
    /// 原始消息总数
    pub total_raw: usize,
    /// 成功解析数
    pub total_success: usize,
    /// 降级处理数
    pub total_degraded: usize,
    /// 跳过数
    pub total_skipped: usize,
    /// 切割后的 session 数
    pub session_count: usize,
    /// 切割间隔（分钟）
    pub gap_minutes: u32,
}

// =========================================================
// detect_qq_format — 检测文件是否为 QQ 聊天记录格式
// =========================================================

/// 检测文件是否为 QQ 聊天记录支持的格式。
///
/// 参数:
/// - `file_path`: 待检测的文件绝对路径。
///
/// 返回:
/// - `true`: 文件格式匹配 QQ 聊天记录（JSON 或 .txt）
/// - `false`: 格式不匹配，应提示用户选择正确的文件
///
/// 说明:
/// - 先检查文件存在性，再调用 ramaria-importer 的格式检测。
/// - 格式检测基于文件内容（首字节判断 JSON vs 文本）而非扩展名。
#[tauri::command]
#[tracing::instrument(skip(_state))]
pub async fn detect_qq_format(
    _state: State<'_, DesktopState>,
    file_path: String,
) -> Result<bool, String> {
    let path = std::path::Path::new(&file_path);

    // 安全检查：文件必须存在
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    if !path.is_file() {
        return Err(format!("路径不是文件: {}", file_path));
    }

    // 委托 ramaria-importer 检测格式
    let importer = ramaria_importer::qq::QqImporter::new();
    importer
        .detect_format(path)
        .map_err(|e| format!("格式检测失败: {}", e))
}

// =========================================================
// import_qq_chat — 执行 QQ 聊天记录导入
// =========================================================

/// 执行 QQ 聊天记录导入。
///
/// 参数:
/// - `file_path`: 聊天记录文件的绝对路径（JSON 或 .txt 格式）。
/// - `mode`: 导入模式，"fast"（仅 L0）或 "deep"（全管线）。
/// - `persona_name`: 可选，导入关联的 persona 显示名称。如果不提供，使用导出者名称。
/// - `gap_minutes`: session 切割时间间隔（分钟），默认 10。
///
/// 返回:
/// - `ImportResult` JSON 对象，包含报告摘要和统计信息。
///
/// 说明:
/// - 快速模式：调用 `QqImporter::execute_fast_import()`，仅写入 messages 表。
/// - 深度模式：在快速模式基础上，对每个 session 触发 L1 摘要生成。
/// - Persona 自动管理：`ensure_qq_persona()` 查找或创建 source="qq" 的 persona。
/// - 指纹去重：已导入的消息（相同 fingerprint）会被跳过。
/// - 错误处理：解析失败返回含上下文的错误消息，便于前端展示。
#[tauri::command]
#[tracing::instrument(skip(state, app_handle))]
pub async fn import_qq_chat(
    state: State<'_, DesktopState>,
    app_handle: AppHandle,
    file_path: String,
    mode: Option<String>,
    persona_name: Option<String>,
    gap_minutes: Option<u32>,
) -> Result<ImportResult, String> {
    use std::path::Path;

    let mode_str = mode.unwrap_or_else(|| "fast".to_string());
    let import_mode = match mode_str.as_str() {
        "fast" => ramaria_importer::ImportMode::Fast,
        "deep" => ramaria_importer::ImportMode::Deep,
        other => {
            return Err(format!(
                "不支持的导入模式: {}（仅支持 fast 或 deep）",
                other
            ));
        }
    };
    let gap = gap_minutes.unwrap_or(10);

    // Step 1: 文件安全性校验
    let path = Path::new(&file_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    if !path.is_file() {
        return Err(format!("路径不是文件: {}", file_path));
    }

    // 扩展名白名单
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" && ext != "txt" {
        return Err(format!(
            "不支持的文件类型: .{}（仅支持 .json 和 .txt）",
            ext
        ));
    }

    tracing::info!(
        file = %file_path,
        mode = %mode_str,
        gap_minutes = gap,
        "开始 QQ 聊天记录导入"
    );

    // Step 2: 格式检测与文件解析
    let importer = ramaria_importer::qq::QqImporter::new();

    let is_qq = importer
        .detect_format(path)
        .map_err(|e| format!("格式检测失败: {}", e))?;

    if !is_qq {
        return Err(format!(
            "文件 '{}' 不是 QQ 聊天记录格式。请确认文件来自 QQ 聊天记录导出（shuakami/qq-chat-exporter JSON 或 PCQQ .txt）。",
            file_path
        ));
    }

    let (sessions, report) = importer
        .parse(path, gap)
        .map_err(|e| format!("文件解析失败: {}", e))?;

    if sessions.is_empty() {
        return Err(format!(
            "文件中没有可导入的消息。解析报告: 原始 {} 条，成功 0 条，跳过 {} 条。",
            report.total_raw,
            report.total_skipped()
        ));
    }

    tracing::info!(
        session_count = sessions.len(),
        total_success = report.total_success(),
        total_degraded = report.total_degraded(),
        total_skipped = report.total_skipped(),
        "文件解析完成"
    );

    // Step 3: Persona 准备
    let effective_persona_name = persona_name.unwrap_or_else(|| report.self_name.clone());
    let persona_uid = ramaria_importer::qq::ensure_qq_persona(
        &state.pool,
        &format!("char-{}", &report.self_id),
        &effective_persona_name,
        Some(&report.self_id),
    )
    .await
    .map_err(|e| format!("创建/查找 persona 失败: {}", e))?;

    tracing::info!(
        persona_uid = %persona_uid,
        persona_name = %effective_persona_name,
        "Persona 已准备"
    );

    // Step 4: 执行导入
    let (sessions_written, messages_written, session_ids) = match import_mode {
        ramaria_importer::ImportMode::Fast => {
            let (sw, mw, sids) = ramaria_importer::qq::QqImporter::execute_fast_import(
                &state.pool,
                &sessions,
                &persona_uid,
            )
            .await
            .map_err(|e| format!("快速导入失败: {}", e))?;
            (sw, mw, sids)
        }
        ramaria_importer::ImportMode::Deep => {
            let (sw, mw, sids) = ramaria_importer::qq::QqImporter::execute_fast_import(
                &state.pool,
                &sessions,
                &persona_uid,
            )
            .await
            .map_err(|e| format!("深度导入 L0 写入失败: {}", e))?;

            tracing::info!(
                sessions_written = sw,
                messages_written = mw,
                "深度导入 L0 完成"
            );

            (sw, mw, sids)
        }
    };

    // Step 4.5: 为每个导入的 session 生成 L1 摘要
    // 关键：使用 regenerate_l1_no_cascade 避免每个 L1 后立即触发 L2（防止 L1 被提前吸收）
    // 快速模式：仅生成 L1，不触发 L2/L3（留给用户稍后手动触发）
    // 深度模式：全部 L1 完成后统一触发 L2→L3 级联，通过 Tauri Event 推送进度
    let app = state.app.clone();
    let persona = persona_uid.clone();
    let sids = session_ids.clone();
    let is_deep = import_mode == ramaria_importer::ImportMode::Deep;
    let total_sids = sids.len();
    tokio::spawn(async move {
        // ── Phase 1: 生成全部 L1 摘要（无级联）──
        app_handle
            .emit(
                EVENT_IMPORT_PROGRESS,
                ImportProgressPayload::new("l1", 0, total_sids, "正在生成 L1 会话摘要..."),
            )
            .ok();

        let mut l1_success = 0u32;
        let mut l1_failed = 0u32;
        for (i, sid) in sids.iter().enumerate() {
            match app.regenerate_l1_no_cascade(*sid, Some(&persona)).await {
                Ok(Some(_)) => l1_success += 1,
                Ok(None) => {
                    tracing::debug!(%sid, "session 无消息，跳过 L1");
                }
                Err(e) => {
                    l1_failed += 1;
                    tracing::warn!(%sid, error = %e, "L1 摘要生成失败（非致命）");
                }
            }
            // 每处理一个 session 就推送进度
            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new(
                        "l1",
                        i + 1,
                        total_sids,
                        &format!("L1 摘要 {}/{}", i + 1, total_sids),
                    ),
                )
                .ok();
        }
        tracing::info!(
            l1_success,
            l1_failed,
            total = total_sids,
            "L1 摘要全部生成完成"
        );
        app_handle
            .emit(
                EVENT_IMPORT_PROGRESS,
                ImportProgressPayload::new("l1", total_sids, total_sids, "L1 摘要生成完成"),
            )
            .ok();

        // ── 深度模式: 级联 L2→L3 ──
        if is_deep && l1_success > 0 {
            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new("l2", 0, 0, "正在提取 L2 事件..."),
                )
                .ok();

            app.trigger_l2_check().await;

            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new("l2", 0, 0, "L2 事件提取完成"),
                )
                .ok();

            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new("l3", 0, 0, "正在推断 L3 性格画像..."),
                )
                .ok();

            // L3 在 trigger_l2_check 内部已经级联触发（通过 check_l3_trigger）
        }

        app_handle
            .emit(
                EVENT_IMPORT_PROGRESS,
                ImportProgressPayload::new("done", 0, 0, "深度处理完成"),
            )
            .ok();
    });

    // Step 5: 构建返回结果
    let time_range = if report.time_start.is_empty() || report.time_end.is_empty() {
        "未知".to_string()
    } else {
        format!("{} ~ {}", report.time_start, report.time_end)
    };

    // 提前提取 String 字段，避免后续 report.summary() / total_skipped() 时部分移动冲突
    let report_summary = report.summary();
    let skipped_count = report.total_skipped();
    let self_name = report.self_name;
    let chat_name = report.chat_name;

    let result = ImportResult {
        success: true,
        mode: mode_str.clone(),
        report_summary,
        sessions_written,
        messages_written,
        persona_uid,
        persona_name: effective_persona_name,
        self_name,
        chat_name,
        time_range,
        skipped_count,
    };

    tracing::info!(
        sessions = sessions_written,
        messages = messages_written,
        mode = %result.mode,
        "QQ 聊天记录导入完成"
    );

    Ok(result)
}
