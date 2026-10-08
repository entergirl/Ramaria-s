//! crates/ramaria-desktop/src/commands/import_cmd.rs - QQ 聊天记录导入 Tauri Command
//!
//! 设计特点:
//! - `import_qq_chat`: 解析参数并委托服务层导入用例完成 L0 写入；
//!   随后在后台任务中调用服务层 L1 批量生成与深度处理触发，进度经 Tauri 事件转发
//! - `detect_qq_format`: 检测文件是否为 qq-chat-exporter v6.x JSON 格式
//! - 快速导入（fast）：L0 写入后生成 L1 摘要，不触发深度级联
//! - 深度导入（deep）：L0 → L1 摘要 → 触发 L2/L3 级联
//! - 双画像支持——分别为导出者和对方创建独立 persona（由服务层用例承担）
//! - 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
//! - 进度事件字段与前端契约一致；L1 起始事件由服务层用例发出，桌面只做转发
//! - privacy 回归断言（文件底部）静态扫描本文件的日志与 instrument 写法

use crate::DesktopState;
use crate::events::{EVENT_IMPORT_PROGRESS, ImportProgressPayload};
use ramaria_core::privacy::mask_id;
use ramaria_service::{
    AnalyzeRequest, ImportDoneSummary, ImportL1Progress, ImportMode, ImportPostPlan,
    ImportPostRequest, ImportProgressSink, ImportRequest,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

// =========================================================
// 导入结果结构体
// =========================================================

/// 导入操作的完整结果，序列化后返回给前端展示。
///
/// 包含双方 persona 标识与 L1 状态字段，供前端展示导入摘要与后续引导。
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
    /// 对话对象名称（群聊为群名称）
    pub chat_name: String,
    /// 对话类型（private / group）
    pub chat_type: String,
    /// 解析成员分布（消息数降序；私聊为双方，群聊为全部成员）
    pub members: Vec<ramaria_importer::ImportMemberStat>,
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

/// 文件分析报告（不含导入相关的统计，仅描述文件内容）。
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
    /// 解析成员分布（消息数降序；私聊为双方，群聊为全部成员）
    pub members: Vec<ramaria_importer::ImportMemberStat>,
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
/// - 先检查文件存在性，再委托服务层做格式检测。
/// - 格式检测基于文件内容（首字节判断 JSON vs 文本）而非扩展名。
#[tauri::command]
#[tracing::instrument(skip(state, file_path))]
pub async fn detect_qq_format(
    state: State<'_, DesktopState>,
    file_path: String,
) -> Result<bool, String> {
    // 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝）
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    state
        .engine
        .detect_qq_format(&real_path)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "格式检测失败"))
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
    // 目标列表：私聊为双方 persona 各生成一份 L1（单侧模式跳过侧不生成）；
    // 群聊走成员分发口径（每会话一次生成，目标由块内发言者决定，忽略 targets）。
    let group_fanout = outcome.chat_type == "group";
    let mut targets: Vec<Option<String>> = Vec::with_capacity(2);
    if !group_fanout {
        if let Some(uid) = &outcome.persona_uid {
            targets.push(Some(uid.clone()));
        }
        if let Some(uid) = &outcome.other_persona_uid {
            targets.push(Some(uid.clone()));
        }
    }
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
                        l1_targets: targets,
                        l1_prefix: Some((String::new(), String::new())),
                        group_fanout,
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
                    message: format!("L1 批量生成失败: {e}"),
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

// =========================================================
// 导入进度转发（服务层回调 → Tauri 事件）
// =========================================================

/// 导入进度回调：把服务层阶段进度转发为 Tauri `import-progress` 事件。
///
/// 说明:
/// - 事件负载字段与前端契约一致（phase / current / total / message /
///   各阶段预计总量 / ETA / done 统计）；
/// - 事件发射失败只记 warn，不中断导入批次。
struct TauriImportProgressSink {
    app_handle: AppHandle,
}

impl ImportProgressSink for TauriImportProgressSink {
    fn on_l1_progress(&self, p: &ImportL1Progress) {
        let payload = ImportProgressPayload::new(p.phase, p.current, p.total, &p.message)
            .with_estimates(p.l1_total, p.l2_total, p.l3_total, p.eta_seconds);
        if let Err(e) = self.app_handle.emit(EVENT_IMPORT_PROGRESS, &payload) {
            tracing::warn!(error = %e, "发射 import-progress 事件失败");
        }
    }

    fn on_done(&self, s: &ImportDoneSummary) {
        let payload = ImportProgressPayload::done_with_stats(
            s.l1_success,
            s.l1_failed,
            s.l2_triggered,
            s.l3_triggered,
            s.total_sessions,
            &s.message,
        );
        if let Err(e) = self.app_handle.emit(EVENT_IMPORT_PROGRESS, &payload) {
            tracing::warn!(error = %e, "发射 import-progress done 事件失败");
        }
    }
}

// =========================================================
// 隐私回归断言（日志脱敏 grep 审计，仅测试构建参与）
// =========================================================

#[cfg(test)]
mod privacy_regression_tests {
    //! 断言本文件的日志不直接落 QQ 号/昵称明文。
    //!
    //! 说明:
    //! - 以源码文本静态断言，拦截"把 persona_uid/persona_name/self_id 等原值
    //!   直接塞回 tracing 字段"的回归；不误伤已包 `mask_id` 的脱敏写法。
    //! - 通过 `env!("CARGO_MANIFEST_DIR")` 定位本文件，不依赖测试运行时目录。
    //! - 仅在 `cfg(test)` 参与，不影响生产构建。

    /// 读取本文件源码，并统一行尾为 LF。
    ///
    /// 说明:
    /// - Windows 工作区可能以 CRLF 检出；统一为 LF 可避免行尾差异造成静态匹配假失败。
    fn self_source() -> String {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/import_cmd.rs");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("无法读取本文件用于隐私断言: {path:?} 错误: {e}"))
            .replace("\r\n", "\n")
    }

    /// 取 `s` 在 `start` 起至多 `max_chars` 个字符的子串（保证不切开多字节 UTF-8）。
    fn take_chars(s: &str, start: usize, max_chars: usize) -> String {
        s[start..].chars().take(max_chars).collect()
    }

    /// 禁止出现的"字段直接 = 裸原值"日志形态（含其后的分隔逗号），
    /// 若被重新引入即泄露 QQ 号/昵称明文。
    ///
    /// 字段名与值的 sigil/名称分开存储，运行时拼接，避免字面量出现在本文件
    /// 源码中而干扰对"目标文件是否含原值日志"的静态判定。
    const FORBIDDEN_FIELD_VALUE_PAIRS: &[(&str, char, &str)] = &[
        ("persona_uid", '%', "resolved"),
        ("persona_uid", '%', "self_uid"),
        ("persona_uid", '%', "other_uid"),
        ("persona_name", '%', "self_name"),
        ("persona_name", '%', "other_name"),
        ("self_uid", '%', "self_uid"),
        ("other_uid", '%', "other_uid"),
        ("other_uid", '%', "other_default_uid"),
        ("self_name", '%', "self_name"),
        ("other_name", '%', "other_name"),
        ("self_id", '%', "report.self_id"),
        ("other_ref_id", '?', "other_ref_id"),
        ("self_uid", '%', "uid"),
        ("other_uid", '%', "uid"),
        ("self_persona", '?', "self_persona_uid_resolved"),
        ("other_persona", '?', "other_persona_uid_resolved"),
    ];

    /// import_qq_chat 入口 span 必须 skip 的人参（否则 instrument 在 info 级
    /// 自动记录 nickname / 显式 QQ UID 参数）。
    const MUST_SKIP_ARGS: &[&str] = &[
        "persona_name",
        "self_persona_uid",
        "other_persona_name",
        "other_persona_uid",
    ];

    /// 禁止出现的"路径原值"日志形态。
    ///
    /// 字段名前缀与值表达式后缀分开存储、运行时拼接，避免完整字面量出现在
    /// 本文件源码中而干扰对"目标文件是否含路径原值日志"的静态判定。
    const FORBIDDEN_PATH_LOG_FRAGMENTS: &[(&str, &str)] =
        &[("= %real_path", ".display()"), ("= %file_path", ",")];

    #[test]
    fn no_raw_personal_field_in_logs() {
        let src = self_source();
        for &(field, sigil, value) in FORBIDDEN_FIELD_VALUE_PAIRS {
            let forbidden = format!("{field} = {sigil}{value},");
            assert!(
                !src.contains(&forbidden),
                "检测到日志把个人标识原值直接写入 tracing 字段（禁止形态 {forbidden:?}）——QQ 号/昵称不得明文落日志，须经 mask_id"
            );
        }
    }

    #[test]
    fn no_raw_path_in_logs() {
        let src = self_source();
        for &(prefix, suffix) in FORBIDDEN_PATH_LOG_FRAGMENTS {
            let forbidden = format!("{prefix}{suffix}");
            assert!(
                !src.contains(&forbidden),
                "检测到日志把绝对路径原值直接写入 tracing 字段（禁止形态 {forbidden:?}）——路径须经 redact_path_label 脱敏"
            );
        }
    }

    #[test]
    fn instrument_skips_personal_args() {
        let src = self_source();
        // 以 import_qq_chat 函数签名定位其正上方的 #[tracing::instrument(skip(...))]
        // 属性块：在签名前 400 字符内回溯最近一次 "instrument(skip(" 的起点即该
        // skip 列表。与首参顺序/缩进/行尾无关，避免 rustfmt 或 CRLF 造成静态匹配假失败。
        let fn_start = src
            .find("pub async fn import_qq_chat(")
            .expect("本文件必须包含 import_qq_chat 定义（用于定位其 instrument skip 块）");
        let probe_start = fn_start.saturating_sub(400);
        let skip_start = src[probe_start..fn_start]
            .rfind("instrument(skip(")
            .map(|off| probe_start + off)
            .unwrap_or_else(|| panic!("未找到 import_qq_chat 的 instrument skip 块"));
        // 按字符取 skip 块文本（前 ~240 字符已覆盖整个 skip(...) 列表），避免字节切片切多字节
        let skip_block = take_chars(&src, skip_start, 240);
        for arg in MUST_SKIP_ARGS {
            assert!(
                skip_block.contains(arg),
                "instrument skip 块必须包含 {arg}，否则该参数会在 info 级被自动记录（泄露昵称/QQ UID）"
            );
        }
    }
}
