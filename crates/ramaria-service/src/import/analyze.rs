//! crates/ramaria-service/src/import/analyze.rs - Ramaria QQ 聊天记录解析预览模块
//!
//! 设计特点:
//! - 只解析不写入：扩展名校验 → 格式检测 → 文件解析 → 统计报告，库表零改动
//! - 统计值在字段移动前计算（时间范围 / 成功 / 降级 / 跳过），供导入前预览对照
//! - 非 QQ 格式与解析失败分别给出结构化错误（warn 日志只记文件名）
//! - Engine 门面 `analyze_qq_import` 对外提供诊断报告

use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_importer::ImportSource;
use ramaria_importer::qq::QqImporter;

use crate::engine::Engine;

use super::detect::{detect_format, ensure_json_extension, path_log_label};

// =========================================================
// 请求与结果类型
// =========================================================

/// 解析预览请求（不写入数据库）。
///
/// 字段约定:
/// - `file_path`: qq-chat-exporter v6.x 导出的 .json 文件路径；
/// - `gap_minutes`: session 切割的时间间隔阈值（分钟）。
#[derive(Debug, Clone)]
pub struct AnalyzeRequest {
    /// 聊天记录文件路径
    pub file_path: PathBuf,
    /// session 切割时间间隔（分钟）
    pub gap_minutes: u32,
}

/// 文件分析报告（描述文件内容与解析统计，不含导入结果）。
#[derive(Debug, Clone)]
pub struct AnalysisReport {
    /// 解析的文件路径
    pub file_path: PathBuf,
    /// 导出者标识（平台内部 UID）
    pub self_id: String,
    /// 导出者名称
    pub self_name: String,
    /// 导出者 QQ 号
    pub self_uin: Option<String>,
    /// 对话对象名称
    pub chat_name: String,
    /// 对话类型（private / group）
    pub chat_type: String,
    /// 解析成员分布（消息数降序；私聊为双方）
    pub members: Vec<ramaria_importer::ImportMemberStat>,
    /// 对方名称
    pub other_name: String,
    /// 对方平台内部 UID
    pub other_uid: String,
    /// 对方 QQ 号
    pub other_uin: Option<String>,
    /// 消息时间范围（`起 ~ 止`；未知时为"未知"）
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
// 用例入口
// =========================================================

/// 解析 QQ 聊天记录文件，返回诊断报告（不写入数据库）。
///
/// 流程:
/// 1. 扩展名校验（仅 `.json`）；
/// 2. 格式检测（qq-chat-exporter v6.x JSON）；
/// 3. 文件解析 → 统计报告。
///
/// 参数:
/// - `_engine`: 服务层引擎（解析不依赖引擎，保留参数与其它用例入口一致）；
/// - `req`: 解析请求（文件路径 + 切割间隔）。
///
/// 返回:
/// - `AnalysisReport`：含统计信息与双方名称，供导入前预览。
pub(crate) async fn analyze(
    _engine: &Engine,
    req: AnalyzeRequest,
) -> RamariaResult<AnalysisReport> {
    let path = req.file_path.as_path();
    ensure_json_extension(path)?;

    tracing::info!(
        file = %path_log_label(path),
        gap_minutes = req.gap_minutes,
        "开始解析 QQ 聊天记录文件"
    );

    let is_qq = detect_format(_engine, path).await?;

    if !is_qq {
        tracing::warn!(file = %path_log_label(path), "文件不是 QQ 聊天记录格式");
        return Err(RamariaError::validation(format!(
            "文件 '{}' 不是 QQ 聊天记录格式",
            path.display()
        )));
    }

    let importer = QqImporter::new();

    let (_sessions, report) = importer.parse(path, req.gap_minutes).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录文件解析失败");
    })?;

    tracing::info!(
        total_raw = report.total_raw,
        success = report.total_success(),
        degraded = report.total_degraded(),
        skipped = report.total_skipped(),
        sessions = report.session_count,
        "QQ 文件解析完成"
    );

    // 统计值在字段移动前计算（避免对部分移动后的报告调用方法）
    let time_range = report_time_range(&report);
    let total_success = report.total_success();
    let total_degraded = report.total_degraded();
    let total_skipped = report.total_skipped();

    Ok(AnalysisReport {
        file_path: req.file_path,
        self_id: report.self_id,
        self_name: report.self_name,
        self_uin: report.self_uin,
        chat_name: report.chat_name,
        chat_type: report.chat_type,
        members: report.members,
        other_name: report.other_name,
        other_uid: report.other_uid,
        other_uin: report.other_uin,
        time_range,
        total_raw: report.total_raw,
        total_success,
        total_degraded,
        total_skipped,
        session_count: report.session_count,
        gap_minutes: req.gap_minutes,
    })
}

// =========================================================
// 内部工具
// =========================================================

/// 解析报告的时间范围文本（起止为空时为"未知"）。
pub(super) fn report_time_range(report: &ramaria_importer::ImportReport) -> String {
    if report.time_start.is_empty() || report.time_end.is_empty() {
        "未知".to_string()
    } else {
        format!("{} ~ {}", report.time_start, report.time_end)
    }
}
