//! crates/ramaria-desktop/src/commands/import_cmd.rs - QQ 聊天记录导入 Tauri Command
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
    /// 使用的 persona_uid（导出者；side=other 导入侧过滤时为 null）
    pub persona_uid: Option<String>,
    /// persona 名称（导出者）
    pub persona_name: String,
    /// 对方 persona UID（side=self 导入侧过滤时为 null）
    pub other_persona_uid: Option<String>,
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

    // 隐私红线：QQ 号/UID 属个人标识，info 级日志不记录（仅记录量与统计信息）
    tracing::info!(
        total_raw = report.total_raw,
        success = report.total_success(),
        degraded = report.total_degraded(),
        skipped = report.total_skipped(),
        sessions = report.session_count,
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
    side: Option<String>,
) -> Result<ImportResult, String> {
    use ramaria_importer::qq::{PersonaSide, build_persona_uid};

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
    // 导入侧过滤：前端面板传入 "self"/"other"/"both"；缺失/非法回退 both
    let import_side = ramaria_importer::qq::ImportSide::parse_cli(side.as_deref())
        .unwrap_or(ramaria_importer::qq::ImportSide::Both);

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

    // 3a. 导出者（self）persona —— 我方，UID 前缀 user-（kind=user）；
    //     side=other 时该侧 persona 不创建（消息也不会入库）
    let self_name = persona_name.unwrap_or_else(|| report.self_name.clone());
    let self_uid = build_persona_uid(
        PersonaSide::Me,
        self_persona_uid.as_deref(),
        report.self_uin.as_deref(),
        &report.self_id,
        max_qq_seq + 1,
    );
    let self_persona_uid_resolved: Option<String> = if import_side.needs_persona(PersonaSide::Me) {
        let resolved = ramaria_importer::qq::ensure_qq_persona(
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
            persona_uid = %resolved,
            persona_name = %self_name,
            "导出者 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过导出者 persona（side=other）");
        None
    };

    // 3b. 对方（other）persona；side=self 时该侧 persona 不创建
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
        PersonaSide::Other,
        other_persona_uid.as_deref(),
        report.other_uin.as_deref(),
        &report.other_uid,
        max_qq_seq + 2,
    );

    tracing::debug!(
        other_uid = %other_default_uid,
        other_name = %other_name,
        other_ref_id = ?other_ref_id,
        "准备创建对方 persona"
    );

    let other_persona_uid_resolved: Option<String> = if import_side
        .needs_persona(PersonaSide::Other)
    {
        let resolved = ramaria_importer::qq::ensure_qq_persona(
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
            persona_uid = %resolved,
            persona_name = %other_name,
            "对方 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过对方 persona（side=self）");
        None
    };

    // Step 4: 执行导入（按 side 过滤消息；单侧模式下跳过侧 persona 为 None）
    tracing::debug!(
        sessions_count = sessions.len(),
        self_persona = ?self_persona_uid_resolved,
        other_persona = ?other_persona_uid_resolved,
        self_id = %report.self_id,
        side = ?import_side,
        "准备执行快速导入"
    );

    let (sessions_written, messages_written, session_ids) =
        ramaria_importer::qq::QqImporter::execute_fast_import(
            &state.pool,
            &sessions,
            self_persona_uid_resolved.as_deref(),
            other_persona_uid_resolved.as_deref(),
            &report.self_id,
            import_side,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "导入写入失败");
            format!("导入写入失败: {}", e)
        })?;

    tracing::info!(sessions_written, messages_written, "L0 写入完成");

    // Step 4.5: 为每个导入的 session 生成 L1 摘要（双方 persona 各一份）
    //
    // 导入对话是双人对话（self↔other），L1 摘要应同时归属两人。
    // D1 将 persona_uid 从 self→other 后，other 获得了 L1→L2→L3 全管线，
    // 但 self 的 L1 来源完全断裂，无法触发 L2/L3。
    //
    // 修复方案：对每个 session，分别以 self 和 other 的 persona_uid
    // 各调用一次 regenerate_l1_no_cascade。LLM 会根据对话内容为双方
    // 各生成语义上适当的摘要（因为 persona_uid 不同，存储为不同行）。
    //
    // 影响：LLM 调用量翻倍（N session × 2），但这是正确语义的必要代价。
    let app = state.app.clone();
    let sids = session_ids.clone();
    let is_deep = import_mode == ramaria_importer::ImportMode::Deep;
    let total_sids = sids.len();
    // 单侧模式（side=self/other）下，跳过侧 persona 为 None → 该侧 L1 摘要不生成
    let self_uid = self_persona_uid_resolved.clone();
    let other_uid = other_persona_uid_resolved.clone();
    let persona_count = usize::from(self_uid.is_some()) + usize::from(other_uid.is_some());
    // 批量 LLM 请求间最小间隔（毫秒）：读 `[thresholds].cluster_delay_ms`，
    // L1/L2 共用（`ramaria_memory::llm_gate::inter_llm_delay`）。
    // 导入会连续 N×2 次调用 LLM，无节流时易触发远程 API 速率限制
    // （典型表现：HTTP 200 + 空内容 → L1 摘要失败重试）。
    // 读取失败时降级为 0（不阻塞导入，等同旧行为）。
    let llm_delay_ms =
        ramaria_app::ConfigSyncService::new(state.app.storage().clone(), state.config_path.clone())
            .load_config_only()
            .await
            .map(|cfg| cfg.thresholds.cluster_delay_ms)
            .unwrap_or(0);
    tokio::spawn(async move {
        // ── 生成全部 L1 摘要（无级联）──
        // （由 QQ parser 的 make_role_content 嵌入），故传空前缀避免"用户：""助手："双重前缀。
        // 空前缀 → 对话格式为 "[张三] 消息内容" 而非 "用户：[张三] 消息内容"，
        // LLM 在 summary/evidence_notes 中自然使用实际名称。
        //
        // 阶段统计与 EMA：
        // - L1 预计总量可预知 = session × persona（self + other 各一条 L1）。
        // - L2/L3 预计总量在线估算：深度模式下以触发 persona 数为工作量单位
        //   （事件提取/性格推断均按 persona 批处理，导入双人场景上界为 2）。
        // - EtaEstimator 分层维护 EMA 单次耗时，剩余秒数随事件下发；
        //   首次无样本时 remaining_seconds() = None，前端回退线性估算（降级）。
        let started_at = std::time::Instant::now();
        let mut eta_est = ramaria_app::eta::EtaEstimator::new();
        // L1 调用数 = session × 实际处理 persona 数（单侧模式为 1）
        let l1_total = total_sids * persona_count;

        app_handle
            .emit(
                EVENT_IMPORT_PROGRESS,
                ImportProgressPayload::new(
                    "l1",
                    0,
                    l1_total,
                    "正在生成 L1 会话摘要（双方 persona）...",
                )
                .with_estimates(Some(l1_total), None, None, None),
            )
            .ok();

        let mut l1_success = 0usize;
        let mut l1_failed = 0usize;
        let mut l1_processed = 0usize; // 已处理次数（每个 session 2 次）
        for sid in &sids {
            // ── 为导出方（self）生成 L1（side=other 时跳过侧不生成）──
            if let Some(uid) = &self_uid {
                match app
                    .regenerate_l1_no_cascade(*sid, Some(uid), Some(""), Some(""))
                    .await
                {
                    Ok(Some(_)) => l1_success += 1,
                    Ok(None) => {
                        tracing::debug!(%sid, self_uid = %uid, "self: session 无消息，跳过 L1");
                    }
                    Err(e) => {
                        l1_failed += 1;
                        tracing::warn!(%sid, self_uid = %uid, error = %e, "L1 摘要生成失败 (self, 非致命)");
                    }
                }
                l1_processed += 1;

                // 请求间节流（L1/L2 共用，`[thresholds].cluster_delay_ms`）：
                // 连续 LLM 调用间保持最小间隔，避免触发远程 API 速率限制。
                ramaria_memory::llm_gate::inter_llm_delay(llm_delay_ms, "L1 导入批量摘要 (self)")
                    .await;
            }

            // ── 为对话方（other）生成 L1（side=self 时跳过侧不生成）──
            if let Some(uid) = &other_uid {
                match app
                    .regenerate_l1_no_cascade(*sid, Some(uid), Some(""), Some(""))
                    .await
                {
                    Ok(Some(_)) => l1_success += 1,
                    Ok(None) => {
                        tracing::debug!(%sid, other_uid = %uid, "other: session 无消息，跳过 L1");
                    }
                    Err(e) => {
                        l1_failed += 1;
                        tracing::warn!(%sid, other_uid = %uid, error = %e, "L1 摘要生成失败 (other, 非致命)");
                    }
                }
                l1_processed += 1;

                // 请求间节流（同上）：self 与 other 各一次 LLM 调用，间隔保持 ≥ delay_ms。
                ramaria_memory::llm_gate::inter_llm_delay(llm_delay_ms, "L1 导入批量摘要 (other)")
                    .await;
            }

            // 每处理一个 session（2 个 persona）就推送进度
            // 修复 v1.4 的 total 不一致：统一以 LLM 调用次数（l1_total）为分母，
            // current = 已处理调用次数（l1_processed）。
            eta_est.update(
                ramaria_app::eta::PhaseKind::L1,
                l1_processed,
                l1_total,
                started_at.elapsed().as_secs_f64(),
            );
            let eta_secs = eta_est.remaining_seconds().map(|s| s.round() as u64);
            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new(
                        "l1",
                        l1_processed,
                        l1_total,
                        &format!("L1 摘要 {}/{}（双方 persona）", l1_processed, l1_total),
                    )
                    .with_estimates(Some(l1_total), None, None, eta_secs),
                )
                .ok();
        }
        tracing::info!(
            l1_success,
            l1_failed,
            l1_processed,
            total_sessions = total_sids,
            self_uid = ?self_uid,
            other_uid = ?other_uid,
            "L1 摘要全部生成完成（双方 persona 各有独立副本）"
        );

        // ── 深度模式: 级联 L2→L3 ──
        // 双方均可在满足未吸收 L1 ≥ 5 条条件后各自触发 L2→L3 级联。
        let mut l2_triggered = false;
        let mut l3_triggered = false;
        if is_deep && l1_success > 0 {
            // L2/L3 预计总量在线估算：以触发处理的 persona 数为工作量单位
            // （事件提取/性格推断均按 persona 批处理，导入双人场景上界为 2）。
            let l2_expected = 2;
            let l3_expected = 2;
            // L2 阶段开始：总量 = 触发事件提取的 persona 数（在线估算）
            eta_est.update(
                ramaria_app::eta::PhaseKind::L2,
                0,
                l2_expected,
                started_at.elapsed().as_secs_f64(),
            );
            let eta_secs = eta_est.remaining_seconds().map(|s| s.round() as u64);
            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new(
                        "l2",
                        0,
                        l2_expected,
                        "正在提取 L2 事件（双方 persona）...",
                    )
                    .with_estimates(
                        Some(l1_total),
                        Some(l2_expected),
                        None,
                        eta_secs,
                    ),
                )
                .ok();

            app.trigger_l2_check().await;
            l2_triggered = true;

            // L3 阶段开始：L2 已完成（回填 L2 样本），总量 = 触发推断的 persona 数
            eta_est.update(
                ramaria_app::eta::PhaseKind::L2,
                l2_expected,
                l2_expected,
                started_at.elapsed().as_secs_f64(),
            );
            eta_est.update(
                ramaria_app::eta::PhaseKind::L3,
                0,
                l3_expected,
                started_at.elapsed().as_secs_f64(),
            );
            let eta_secs = eta_est.remaining_seconds().map(|s| s.round() as u64);
            app_handle
                .emit(
                    EVENT_IMPORT_PROGRESS,
                    ImportProgressPayload::new(
                        "l3",
                        0,
                        l3_expected,
                        "正在推断 L3 性格画像（双方 persona）...",
                    )
                    .with_estimates(
                        Some(l1_total),
                        Some(l2_expected),
                        Some(l3_expected),
                        eta_secs,
                    ),
                )
                .ok();
            l3_triggered = true;
        }

        // ── done 事件携带完整统计，供前端判断是否需要展示深度处理引导 ──
        let done_msg = if l1_failed > 0 {
            format!(
                "深度处理完成: L1 成功 {}/{}, 失败 {}。请确认 LLM 已连接后重试。",
                l1_success, l1_processed, l1_failed
            )
        } else {
            format!(
                "深度处理完成: L1 全部成功 ({}/{})",
                l1_success, l1_processed
            )
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
        self_persona = ?result.persona_uid,
        other_persona = ?result.other_persona_uid,
        "QQ 聊天记录导入完成"
    );

    Ok(result)
}
