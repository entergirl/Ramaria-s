//! rust/crates/ramaria-desktop/src/commands/import_cmd.rs - QQ 聊天记录导入 Tauri Command
//!
//! 设计特点:
//! - `import_qq_chat`: 接收文件路径、导入模式和双画像参数，委托 ramaria-importer 执行解析与写入
//! - `detect_qq_format`: 检测文件是否为 qq-chat-exporter v6.x JSON 格式
//! - 快速导入（fast）：仅写入 messages 表（L0），按发送者分配 persona_uid
//! - 深度导入（deep）：创建历史 session → 写入 L0 → 关闭 session → 触发全管线
//! - 双画像支持——分别为导出者和对方创建独立 persona
//! - L1 摘要 persona_uid 存 NULL，不绑定特定画像
//! - 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
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
///
/// 新增 `other_persona_uid` 和 `other_persona_name` 字段。
/// 新增 `l1_success` / `l1_failed`，前端据此展示 L1 生成状态警告。
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
    /// 使用的 persona_uid（导出者）
    pub persona_uid: String,
    /// persona 名称（导出者）
    pub persona_name: String,
    /// 对方 persona UID
    pub other_persona_uid: String,
    /// 对方 persona 名称
    pub other_persona_name: String,
    /// 导出者名称（从文件中解析）
    pub self_name: String,
    /// 对话对象名称
    pub chat_name: String,
    /// 对话时间范围（如 "2023-01-01 ~ 2024-06-30"）
    pub time_range: String,
    /// 跳过的消息数（撤回+空+未知类型）
    pub skipped_count: usize,
    /// 写入的 session_id 列表（供前端导航查看导入消息）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
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
#[tracing::instrument(skip(_state), fields(file = %file_path))]
pub async fn analyze_qq_chat(
    _state: State<'_, DesktopState>,
    file_path: String,
    gap_minutes: Option<u32>,
) -> Result<AnalysisReport, String> {
    let gap = gap_minutes.unwrap_or(10);

    // 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝）
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    tracing::info!(gap_minutes = gap, path = %real_path.display(), "开始解析 QQ 聊天记录文件");

    let importer = ramaria_importer::qq::QqImporter::new();

    let is_qq = importer.detect_format(&real_path).map_err(|e| {
        tracing::error!(error = %e, "格式检测失败");
        format!("格式检测失败: {}", e)
    })?;

    if !is_qq {
        tracing::warn!("文件格式不是 QQ 聊天记录");
        return Err(format!("文件 '{}' 不是 QQ 聊天记录格式", file_path));
    }

    let (_sessions, report) = importer.parse(&real_path, gap).map_err(|e| {
        tracing::error!(error = %e, "文件解析失败");
        format!("文件解析失败: {}", e)
    })?;

    tracing::info!(
        total_raw = report.total_raw,
        success = report.total_success(),
        degraded = report.total_degraded(),
        skipped = report.total_skipped(),
        sessions = report.session_count,
        self_uin = ?report.self_uin,
        other_uid = %report.other_uid,
        "QQ 文件解析完成"
    );

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
        self_uin: report.self_uin,
        chat_name: report.chat_name,
        chat_type: report.chat_type,
        other_name: report.other_name,
        other_uid: report.other_uid,
        other_uin: report.other_uin,
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
///
/// 新增 `self_uin`、`other_name`、`other_uid`、`other_uin`。
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    /// 文件路径
    pub file_path: String,
    /// 导出者 ID
    pub self_id: String,
    /// 导出者名称
    pub self_name: String,
    /// 导出者 QQ 号
    pub self_uin: Option<String>,
    /// 对话对象名称（chatInfo.name）
    pub chat_name: String,
    /// 对话类型
    pub chat_type: String,
    /// 对方名称
    pub other_name: String,
    /// 对方 QQ UID
    pub other_uid: String,
    /// 对方 QQ 号
    pub other_uin: Option<String>,
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
/// - `true`: 文件格式匹配 qq-chat-exporter v6.x JSON
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
    // 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝）
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    // 委托 ramaria-importer 检测格式
    let importer = ramaria_importer::qq::QqImporter::new();
    importer
        .detect_format(&real_path)
        .map_err(|e| format!("格式检测失败: {}", e))
}

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
///
/// 返回:
/// - `ImportResult` JSON 对象，包含报告摘要、统计信息和双画像标识。
#[tauri::command]
#[tracing::instrument(skip(state, app_handle))]
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
) -> Result<ImportResult, String> {
    use ramaria_importer::qq::build_persona_uid;

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

    // Step 1: 路径安全校验（三层防御：canonicalize + 白名单 + 符号链接拒绝）
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    // 扩展名白名单（在 canonicalize 之后执行，使用真实路径的扩展名）
    let ext = real_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" {
        return Err(format!(
            "不支持的文件类型: .{}（仅支持 qq-chat-exporter v6.x 导出的 .json）",
            ext
        ));
    }

    tracing::info!(
        file = %file_path,
        real = %real_path.display(),
        mode = %mode_str,
        gap_minutes = gap,
        "开始 QQ 聊天记录导入"
    );

    // Step 2: 格式检测与文件解析
    let importer = ramaria_importer::qq::QqImporter::new();

    let is_qq = importer
        .detect_format(&real_path)
        .map_err(|e| format!("格式检测失败: {}", e))?;

    if !is_qq {
        return Err(format!(
            "文件 '{}' 不是 QQ 聊天记录格式。请确认文件来自 qq-chat-exporter v6.x 导出的 JSON。",
            file_path
        ));
    }

    let (sessions, report) = importer
        .parse(&real_path, gap)
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

    // Step 3: 双画像 Persona 准备
    // 查询已有 QQ persona 最大 seq（用于 fallback 级别 4）
    let all_personas = ramaria_storage::repo::personas::list_all(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "查询已有 persona 列表失败");
            format!("查询已有 persona 列表失败: {}", e)
        })?;
    let max_qq_seq: u32 = all_personas
        .iter()
        .filter(|p| p.source == "qq")
        .map(|p| p.seq as u32)
        .max()
        .unwrap_or(0);

    // 3a. 导出者（self）persona
    let self_name = persona_name.unwrap_or_else(|| report.self_name.clone());
    let self_uid = build_persona_uid(
        self_persona_uid.as_deref(),
        report.self_uin.as_deref(),
        &report.self_id,
        max_qq_seq + 1,
    );
    let self_persona_uid_resolved = ramaria_importer::qq::ensure_qq_persona(
        &state.pool,
        &self_uid,
        &self_name,
        Some(&report.self_id),
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, self_uid = %self_uid, self_name = %self_name, "创建/查找导出者 persona 失败");
        format!("创建/查找导出者 persona 失败: {}", e)
    })?;

    tracing::info!(
        persona_uid = %self_persona_uid_resolved,
        persona_name = %self_name,
        "导出者 Persona 已准备"
    );

    // 3b. 对方（other）persona
    let other_name = other_persona_name.unwrap_or_else(|| {
        if report.other_name.is_empty() {
            report.chat_name.clone()
        } else {
            report.other_name.clone()
        }
    });
    let other_ref_id = if report.other_uid.is_empty() {
        None
    } else {
        Some(report.other_uid.as_str())
    };
    let other_default_uid = build_persona_uid(
        other_persona_uid.as_deref(),
        report.other_uin.as_deref(),
        &report.other_uid,
        max_qq_seq + 2,
    );

    tracing::debug!(
        other_uid = %other_default_uid,
        other_name = %other_name,
        other_ref_id = ?other_ref_id,
        other_uin = ?report.other_uin,
        "准备创建对方 persona"
    );

    let other_persona_uid_resolved = ramaria_importer::qq::ensure_qq_persona(
        &state.pool,
        &other_default_uid,
        &other_name,
        other_ref_id,
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, other_uid = %other_default_uid, other_name = %other_name, "创建/查找对方 persona 失败");
        format!("创建/查找对方 persona 失败: {}", e)
    })?;

    tracing::info!(
        persona_uid = %other_persona_uid_resolved,
        persona_name = %other_name,
        "对方 Persona 已准备"
    );

    // Step 4: 执行导入
    tracing::debug!(
        sessions_count = sessions.len(),
        self_persona = %self_persona_uid_resolved,
        other_persona = %other_persona_uid_resolved,
        self_id = %report.self_id,
        "准备执行快速导入"
    );

    let (sessions_written, messages_written, session_ids) =
        ramaria_importer::qq::QqImporter::execute_fast_import(
            &state.pool,
            &sessions,
            &self_persona_uid_resolved,
            &other_persona_uid_resolved,
            &report.self_id,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "导入写入失败");
            format!("导入写入失败: {}", e)
        })?;

    tracing::info!(sessions_written, messages_written, "L0 写入完成");

    // Step 4.5: 为每个导入的 session 生成 L1 摘要
    // v1.2 修复: L1 摘要 persona_uid 设为导出者（self）人格 UID
    // 原设计: persona_uid=NULL 以"避免记忆视图污染"（T-V11-5B-010）
    // 问题: NULL 导致 list_unabsorbed_l1(uid) 永远查不到 → L2 永远无法触发
    // 修复: 关联到导出者 persona，L2/L3 管线可正常级联；对方 persona 的
    //       事件提取通过 chat_partners 分组独立进行，不交叉污染
    let app = state.app.clone();
    let sids = session_ids.clone();
    let is_deep = import_mode == ramaria_importer::ImportMode::Deep;
    let total_sids = sids.len();
    let l1_persona_uid = self_persona_uid_resolved.clone();
    tokio::spawn(async move {
        // ── 生成全部 L1 摘要（无级联，persona_uid=导出者）──
        app_handle
            .emit(
                EVENT_IMPORT_PROGRESS,
                ImportProgressPayload::new("l1", 0, total_sids, "正在生成 L1 会话摘要..."),
            )
            .ok();

        let mut l1_success = 0usize;
        let mut l1_failed = 0usize;
        for (i, sid) in sids.iter().enumerate() {
            match app
                .regenerate_l1_no_cascade(*sid, Some(&l1_persona_uid))
                .await
            {
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
            persona_uid = %l1_persona_uid,
            "L1 摘要全部生成完成"
        );

        // ── 深度模式: 级联 L2→L3 ──
        let mut l2_triggered = false;
        let mut l3_triggered = false;
        if is_deep && l1_success > 0 {
            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new("l2", 0, 0, "正在提取 L2 事件..."),
                )
                .ok();

            app.trigger_l2_check().await;
            l2_triggered = true;

            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new("l3", 0, 0, "正在推断 L3 性格画像..."),
                )
                .ok();
            l3_triggered = true;
        }

        // ── done 事件携带完整统计，供前端判断是否需要展示深度处理引导 ──
        let done_msg = if l1_failed > 0 {
            format!(
                "深度处理完成: L1 成功 {}/{}, 失败 {}。请确认 LLM 已连接后重试。",
                l1_success, total_sids, l1_failed
            )
        } else {
            format!("深度处理完成: L1 全部成功 ({}/{})", l1_success, total_sids)
        };
        app_handle
            .emit(
                EVENT_IMPORT_PROGRESS,
                ImportProgressPayload::done_with_stats(
                    l1_success,
                    l1_failed,
                    l2_triggered,
                    l3_triggered,
                    total_sids,
                    &done_msg,
                ),
            )
            .ok();
    });

    // Step 5: 构建返回结果
    let time_range = if report.time_start.is_empty() || report.time_end.is_empty() {
        "未知".to_string()
    } else {
        format!("{} ~ {}", report.time_start, report.time_end)
    };

    // 提前提取 String 字段
    let report_summary = report.summary();
    let skipped_count = report.total_skipped();
    let self_name = report.self_name;
    let chat_name = report.chat_name;

    // 将 session_ids (Vec<Uuid>) 转为 Vec<String> 供前端使用
    let session_id_strings: Vec<String> = session_ids.iter().map(|id| id.to_string()).collect();

    let result = ImportResult {
        success: true,
        mode: mode_str.clone(),
        report_summary,
        sessions_written,
        messages_written,
        persona_uid: self_persona_uid_resolved,
        persona_name: self_name.clone(),
        other_persona_uid: other_persona_uid_resolved,
        other_persona_name: other_name,
        self_name,
        chat_name,
        time_range,
        skipped_count,
        session_ids: session_id_strings,
    };

    tracing::info!(
        sessions = sessions_written,
        messages = messages_written,
        mode = %result.mode,
        self_persona = %result.persona_uid,
        other_persona = %result.other_persona_uid,
        "QQ 聊天记录导入完成"
    );

    Ok(result)
}
