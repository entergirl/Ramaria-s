//! crates/ramaria-service/src/import.rs - QQ 聊天记录导入用例（解析 / L0 写入 / L1 批量生成 / 深度触发）
//!
//! 设计特点:
//! - 管线分段：解析预览（`analyze`）→ L0 写入（`write_l0`）→ L1 批量生成（`generate_l1`）
//!   → 深度处理触发（`trigger_deep`），宿主按导入模式组合调用，不写第二份导入实现
//! - 双画像：导出者与对方分别准备 `source="qq"` 的 persona（UID 生成策略与文件解析口径
//!   由 `ramaria-importer` 承担），L1 摘要按 persona 各生成一份
//! - 进度与 ETA：逐 session 经 `ImportProgressSink` 回调（分层 EMA 预估见 `crate::eta`），
//!   宿主负责把回调转发为自身事件通道
//! - 静默降级：单次 L1 生成失败只记 warn 并计入失败计数，不中断批量（完成提示由宿主汇总）
//! - 隐私：日志不落消息正文与昵称；个人标识一律经 `mask_id` 脱敏，文件路径只留文件名
//! - 宿主差异（快速 / 深度、节流间隔、进度通道）由请求参数与回调表达

use std::path::{Path, PathBuf};
use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_importer::ImportSource;
use ramaria_importer::qq::{
    ImportSide, PersonaSide, QqImporter, build_persona_uid, ensure_qq_persona,
};
use ramaria_importer::writer::ImportWriter;
use uuid::Uuid;

use crate::engine::Engine;
use crate::eta::{EtaEstimator, PhaseKind};

// =========================================================
// 请求与结果类型
// =========================================================

/// 导入模式。
///
/// 状态:
/// - `Fast`: 仅写入 L0（会话 + 消息），不触发后续记忆加工；
/// - `Deep`: L0 写入后由宿主继续调用 L1 批量生成与深度处理触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 快速导入：仅写入 L0 消息
    Fast,
    /// 深度导入：L0 之后继续 L1 摘要与 L2/L3 级联
    Deep,
}

impl ImportMode {
    /// 返回模式的稳定字符串标识（`fast` / `deep`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Deep => "deep",
        }
    }
}

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

/// L0 导入请求。
///
/// 字段约定:
/// - `persona_name` / `other_persona_name`: 画像显示名覆盖（None 时用文件内解析名）；
/// - `self_persona_uid` / `other_persona_uid`: 画像 UID 显式指定（None 时按平台策略生成）；
/// - `side`: 导入侧过滤（仅我方 / 仅对方 / 双方）。
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// 聊天记录文件路径（.json）
    pub file_path: PathBuf,
    /// 导入模式
    pub mode: ImportMode,
    /// session 切割时间间隔（分钟）
    pub gap_minutes: u32,
    /// 导入侧过滤
    pub side: ImportSide,
    /// 导出者画像显示名（None → 文件内解析名）
    pub persona_name: Option<String>,
    /// 导出者画像 UID（None → 按平台策略生成）
    pub self_persona_uid: Option<String>,
    /// 对方画像显示名（None → 文件内解析名）
    pub other_persona_name: Option<String>,
    /// 对方画像 UID（None → 按平台策略生成）
    pub other_persona_uid: Option<String>,
}

/// L0 写入结果（供宿主展示并继续触发 L1 / 深度处理）。
#[derive(Debug, Clone)]
pub struct ImportL0Outcome {
    /// 解析报告摘要（人类可读文本）
    pub report_summary: String,
    /// 写入的 session 数
    pub sessions_written: usize,
    /// 写入的消息总数
    pub messages_written: usize,
    /// 因画像缺失被丢弃的消息数
    pub messages_dropped: usize,
    /// 导出者画像 UID（导入侧过滤跳过时为 None）
    pub persona_uid: Option<String>,
    /// 导出者画像名称（与 `self_name` 同源，取文件解析名）
    pub persona_name: String,
    /// 对方画像 UID（导入侧过滤跳过时为 None）
    pub other_persona_uid: Option<String>,
    /// 对方画像名称
    pub other_persona_name: String,
    /// 导出者名称（从文件中解析）
    pub self_name: String,
    /// 对话对象名称
    pub chat_name: String,
    /// 消息时间范围（`起 ~ 止`；未知时为"未知"）
    pub time_range: String,
    /// 跳过的消息数（撤回 + 空 + 未知类型）
    pub skipped_count: usize,
    /// 写入的 session UUID 列表（供继续触发 L1 与深度处理）
    pub session_ids: Vec<Uuid>,
    /// 本次导入模式
    pub mode: ImportMode,
}

/// L1 批量生成计划。
///
/// 字段约定:
/// - `targets`: 每个目标对应一次 L1 生成（`None` = 不绑定画像，`persona_uid` 存 NULL）；
/// - `cascade`: `true` 时每次生成末尾触发 L2 检查（宿主自行汇总触发），`false` 为无级联口径；
/// - `throttle_ms`: 连续 LLM 调用之间的最小间隔（毫秒，0 = 不等待）。
#[derive(Debug, Clone)]
pub struct ImportL1Plan {
    /// L1 生成目标列表（每个目标一次调用）
    pub targets: Vec<Option<String>>,
    /// 是否在每次生成末尾触发 L2 检查
    pub cascade: bool,
    /// 连续 LLM 调用间最小间隔（毫秒）
    pub throttle_ms: u64,
}

/// L1 批量生成结果。
#[derive(Debug, Clone)]
pub struct ImportL1Outcome {
    /// 生成成功数
    pub l1_success: usize,
    /// 生成失败数
    pub l1_failed: usize,
    /// 跳过数（会话无消息或已有同画像摘要）
    pub l1_skipped: usize,
    /// 实际处理次数（成功 + 跳过 + 失败）
    pub l1_processed: usize,
    /// 计划调用总数（session 数 × 目标数）
    pub l1_total: usize,
    /// 参与的 session UUID 列表
    pub session_ids: Vec<Uuid>,
}

/// 导入进度事件（宿主转发为自身事件通道）。
///
/// 字段约定:
/// - `phase`: 阶段字符串（`l1` / `l2` / `l3`）；
/// - `current` / `total`: 阶段内进度（总数 0 表示未知）；
/// - `eta_seconds`: 分层 EMA 估算的剩余秒数（None = 无样本，宿主可回退线性估算）；
/// - `l1_total` / `l2_total` / `l3_total`: 各阶段预计总量（None = 当前阶段未知）。
#[derive(Debug, Clone)]
pub struct ImportL1Progress {
    /// 阶段: "l1" | "l2" | "l3"
    pub phase: &'static str,
    /// 当前进度（已处理数）
    pub current: usize,
    /// 总数（0 表示未知）
    pub total: usize,
    /// 人类可读的阶段描述
    pub message: String,
    /// 分层 EMA 估算的剩余秒数
    pub eta_seconds: Option<u64>,
    /// L1 阶段预计总量
    pub l1_total: Option<usize>,
    /// L2 阶段预计总量
    pub l2_total: Option<usize>,
    /// L3 阶段预计总量
    pub l3_total: Option<usize>,
}

/// 导入完成摘要（宿主在完成事件中携带）。
#[derive(Debug, Clone)]
pub struct ImportDoneSummary {
    /// L1 生成成功数
    pub l1_success: usize,
    /// L1 生成失败数
    pub l1_failed: usize,
    /// 深度模式：L2 是否已触发
    pub l2_triggered: bool,
    /// 深度模式：L3 是否已触发
    pub l3_triggered: bool,
    /// 导入的 session 总数
    pub total_sessions: usize,
    /// 人类可读的完成消息
    pub message: String,
}

/// 导入进度回调（宿主实现并转发到自身事件通道）。
///
/// 实现要求:
/// - 回调在导入执行路径上同步调用，实现应尽快返回（不阻塞批量生成）；
/// - `on_done` 由宿主在汇总完成统计后调用（服务层不代为触发）。
pub trait ImportProgressSink: Send + Sync {
    /// L1 / L2 / L3 阶段进度。
    fn on_l1_progress(&self, p: &ImportL1Progress);

    /// 导入完成摘要。
    fn on_done(&self, s: &ImportDoneSummary);
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

    let importer = QqImporter::new();

    let is_qq = importer.detect_format(path).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录格式检测失败");
    })?;

    if !is_qq {
        tracing::warn!(file = %path_log_label(path), "文件不是 QQ 聊天记录格式");
        return Err(RamariaError::validation(format!(
            "文件 '{}' 不是 QQ 聊天记录格式",
            path.display()
        )));
    }

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

/// 执行 QQ 聊天记录 L0 导入（双画像准备 + 会话 / 消息写入）。
///
/// 流程:
/// 1. 连接池与扩展名校验；
/// 2. 格式检测与文件解析（空会话显式报错）；
/// 3. 双画像准备（按导入侧过滤：UID 优先级 > QQ 号 > 平台 UID > 递增序号）；
/// 4. `ImportWriter::write_l0` 写入会话与消息（指纹去重，失败补偿删除会话）。
///
/// 参数:
/// - `engine`: 服务层引擎（须已附着 SQLite 连接池）；
/// - `req`: 导入请求（文件 / 模式 / 切割间隔 / 导入侧 / 画像覆盖）。
///
/// 返回:
/// - `ImportL0Outcome`：写入统计、双画像标识与 session UUID 列表。
pub(crate) async fn write_l0(
    engine: &Engine,
    req: ImportRequest,
) -> RamariaResult<ImportL0Outcome> {
    let Some(pool) = engine.sqlite_pool() else {
        return Err(RamariaError::unsupported(
            "导入用例需要 SQLite 连接池（Engine::attach_sqlite_pool）",
        ));
    };
    let path = req.file_path.as_path();
    ensure_json_extension(path)?;

    tracing::info!(
        file = %path_log_label(path),
        mode = %req.mode.as_str(),
        gap_minutes = req.gap_minutes,
        side = ?req.side,
        "开始 QQ 聊天记录导入"
    );

    // ---- 1. 格式检测与文件解析 ----
    let importer = QqImporter::new();

    let is_qq = importer.detect_format(path).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录格式检测失败");
    })?;

    if !is_qq {
        return Err(RamariaError::validation(format!(
            "文件 '{}' 不是 QQ 聊天记录格式。请确认文件来自 qq-chat-exporter v6.x 导出的 JSON。",
            path.display()
        )));
    }

    let (sessions, report) = importer.parse(path, req.gap_minutes).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录文件解析失败");
    })?;

    if sessions.is_empty() {
        return Err(RamariaError::validation(format!(
            "文件中没有可导入的消息。解析报告: 原始 {} 条，成功 0 条，跳过 {} 条。",
            report.total_raw,
            report.total_skipped()
        )));
    }

    tracing::info!(
        session_count = sessions.len(),
        total_success = report.total_success(),
        total_degraded = report.total_degraded(),
        total_skipped = report.total_skipped(),
        "文件解析完成"
    );

    // ---- 2. 双画像准备 ----
    // 查询已有 QQ persona 最大 seq（用于 UID 递增序号兜底）
    let all_personas = ramaria_storage::repo::personas::list_all(&pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "查询已有 persona 列表失败");
            e
        })?;
    let max_qq_seq: u32 = all_personas
        .iter()
        .filter(|p| p.source == "qq")
        .map(|p| p.seq as u32)
        .max()
        .unwrap_or(0);

    // 2a. 导出者（我方）：UID 前缀 user-（kind=user）；导入侧过滤跳过时不创建
    let self_name = req
        .persona_name
        .clone()
        .unwrap_or_else(|| report.self_name.clone());
    let self_default_uid = build_persona_uid(
        PersonaSide::Me,
        req.self_persona_uid.as_deref(),
        report.self_uin.as_deref(),
        &report.self_id,
        max_qq_seq + 1,
    );
    let self_persona_uid = if req.side.needs_persona(PersonaSide::Me) {
        let resolved =
            ensure_qq_persona(&pool, &self_default_uid, &self_name, Some(&report.self_id))
                .await
                .map_err(|e| {
                    tracing::error!(
                        error = %e,
                        persona_uid = %mask_id(&self_default_uid),
                        persona_name = %mask_id(&self_name),
                        "创建/查找导出者 persona 失败"
                    );
                    e
                })?;
        tracing::info!(
            persona_uid = %mask_id(&resolved),
            persona_name = %mask_id(&self_name),
            "导出者 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过导出者 persona");
        None
    };

    // 2b. 对方：UID 前缀 char-；导入侧过滤跳过时不创建
    let other_name = req.other_persona_name.clone().unwrap_or_else(|| {
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
        req.other_persona_uid.as_deref(),
        report.other_uin.as_deref(),
        &report.other_uid,
        max_qq_seq + 2,
    );
    tracing::debug!(
        other_uid = %mask_id(&other_default_uid),
        other_name = %mask_id(&other_name),
        other_ref_id = ?other_ref_id.map(mask_id),
        "准备创建对方 persona"
    );
    let other_persona_uid = if req.side.needs_persona(PersonaSide::Other) {
        let resolved = ensure_qq_persona(&pool, &other_default_uid, &other_name, other_ref_id)
            .await
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    other_uid = %mask_id(&other_default_uid),
                    other_name = %mask_id(&other_name),
                    "创建/查找对方 persona 失败"
                );
                e
            })?;
        tracing::info!(
            persona_uid = %mask_id(&resolved),
            persona_name = %mask_id(&other_name),
            "对方 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过对方 persona");
        None
    };

    // ---- 3. L0 写入（按导入侧过滤消息；跳过侧画像为 None） ----
    tracing::debug!(
        sessions_count = sessions.len(),
        self_persona = ?self_persona_uid.as_deref().map(mask_id),
        other_persona = ?other_persona_uid.as_deref().map(mask_id),
        side = ?req.side,
        "准备执行 L0 写入"
    );

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        self_persona_uid.as_deref(),
        other_persona_uid.as_deref(),
        &report.self_id,
        req.side,
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "导入写入失败");
        e
    })?;

    tracing::info!(
        sessions_written = outcome.sessions_written,
        messages_written = outcome.messages_written,
        messages_dropped = outcome.messages_dropped,
        "L0 写入完成"
    );
    if outcome.messages_dropped > 0 {
        tracing::warn!(
            messages_dropped = outcome.messages_dropped,
            "导入存在因画像缺失被丢弃的消息（不记录消息内容）"
        );
    }

    // ---- 4. 构建结果 ----
    let time_range = report_time_range(&report);
    let report_summary = report.summary();
    let skipped_count = report.total_skipped();
    let self_name = report.self_name;
    let chat_name = report.chat_name;

    Ok(ImportL0Outcome {
        report_summary,
        sessions_written: outcome.sessions_written,
        messages_written: outcome.messages_written,
        messages_dropped: outcome.messages_dropped,
        persona_uid: self_persona_uid,
        persona_name: self_name.clone(),
        other_persona_uid,
        other_persona_name: other_name,
        self_name,
        chat_name,
        time_range,
        skipped_count,
        session_ids: outcome.session_ids,
        mode: req.mode,
    })
}

/// 批量生成导入会话的 L1 摘要。
///
/// 流程:
/// - 循环前先发一条起始进度（`current = 0`、`total = l1_total`），宿主只做转发；
/// - 逐 session × 逐目标调用 L1 生成（`cascade=true` 走带级联口径，否则无级联口径）；
/// - 每次调用后按 `plan.throttle_ms` 执行请求间节流；
/// - 每完成一个 session 更新分层 EMA 估算并经进度回调发送一条 `l1` 阶段进度。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `session_ids`: 待生成 L1 的会话（取 L0 导入结果）；
/// - `plan`: 目标列表与节流 / 级联选项；
/// - `progress`: 可选进度回调。
///
/// 返回:
/// - `ImportL1Outcome`：成功 / 跳过 / 失败计数与处理总数。
pub(crate) async fn generate_l1(
    engine: &Engine,
    session_ids: &[Uuid],
    plan: ImportL1Plan,
    progress: Option<&dyn ImportProgressSink>,
) -> RamariaResult<ImportL1Outcome> {
    let started_at = Instant::now();
    let mut eta = EtaEstimator::new();
    // 预计总量 = session 数 × 目标数（每个目标一次 LLM 调用）
    let l1_total = session_ids.len() * plan.targets.len();

    let mut l1_success = 0usize;
    let mut l1_failed = 0usize;
    let mut l1_skipped = 0usize;
    let mut l1_processed = 0usize;

    // 起始进度：总量已知，先给出 0/总数 的起点（宿主不再自行计算 L1 总量）
    if let Some(sink) = progress {
        sink.on_l1_progress(&ImportL1Progress {
            phase: "l1",
            current: 0,
            total: l1_total,
            message: "正在生成 L1 会话摘要（双方 persona）...".to_string(),
            eta_seconds: None,
            l1_total: Some(l1_total),
            l2_total: None,
            l3_total: None,
        });
    }

    for session_id in session_ids {
        for target in &plan.targets {
            let result = if plan.cascade {
                engine
                    .regenerate_l1(*session_id, target.as_deref(), Some(""), Some(""))
                    .await
            } else {
                engine
                    .regenerate_l1_no_cascade(*session_id, target.as_deref(), Some(""), Some(""))
                    .await
            };

            match result {
                Ok(Some(_)) => l1_success += 1,
                Ok(None) => {
                    // 会话无消息或已有同画像摘要：跳过不影响连续失败计数
                    l1_skipped += 1;
                    tracing::debug!(
                        session_id = %session_id,
                        persona_uid = ?target.as_deref().map(mask_id),
                        "L1 无内容可生成，跳过"
                    );
                }
                Err(e) => {
                    l1_failed += 1;
                    tracing::warn!(
                        session_id = %session_id,
                        persona_uid = ?target.as_deref().map(mask_id),
                        error = %e,
                        "L1 摘要生成失败（非致命）"
                    );
                }
            }
            l1_processed += 1;

            // 请求间节流：连续 LLM 调用间保持最小间隔，避免触发远端速率限制
            ramaria_memory::llm_gate::inter_llm_delay(plan.throttle_ms, "L1 导入批量摘要").await;
        }

        // 每完成一个 session 推送一次进度（分母为 LLM 调用总次数）
        eta.update(
            PhaseKind::L1,
            l1_processed,
            l1_total,
            started_at.elapsed().as_secs_f64(),
        );
        if let Some(sink) = progress {
            sink.on_l1_progress(&ImportL1Progress {
                phase: "l1",
                current: l1_processed,
                total: l1_total,
                message: format!("L1 摘要 {l1_processed}/{l1_total}（双方 persona）"),
                eta_seconds: eta.remaining_seconds().map(|s| s.round() as u64),
                l1_total: Some(l1_total),
                l2_total: None,
                l3_total: None,
            });
        }
    }

    tracing::info!(
        l1_success,
        l1_failed,
        l1_skipped,
        l1_processed,
        total_sessions = session_ids.len(),
        "L1 摘要批量生成完成"
    );

    Ok(ImportL1Outcome {
        l1_success,
        l1_failed,
        l1_skipped,
        l1_processed,
        l1_total,
        session_ids: session_ids.to_vec(),
    })
}

/// 触发导入后的深度处理（L2 事件提取 → L3 性格画像级联）。
///
/// 流程（与宿主事件序列一致）:
/// 1. 更新 L2 阶段 EMA 并发送 `l2` 阶段进度（总量按双方 persona 在线估算 = 2）；
/// 2. 触发全 persona L2 检查（内部失败只记日志，不阻塞）；
/// 3. 回填 L2 进度、发送 `l3` 阶段进度（总量 = 2）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `l1_total`: L1 阶段的调用总数（来自 [`ImportL1Outcome`]；`None` 时进度事件不带 L1 总量）；
/// - `progress`: 可选进度回调（L2 / L3 各一条阶段进度）。
///
/// 说明:
/// - 由调用方在深度模式且至少一条 L1 生成成功时调用；
/// - 级联是否真正产生由引擎内部阈值决定，本用例只负责阶段进度与触发。
pub(crate) async fn trigger_deep(
    engine: &Engine,
    l1_total: Option<usize>,
    progress: Option<&dyn ImportProgressSink>,
) -> RamariaResult<()> {
    let started_at = Instant::now();
    let mut eta = EtaEstimator::new();
    // L2/L3 预计总量在线估算：事件提取与性格推断均按 persona 批处理，导入双人场景上界为 2
    let l2_expected = 2usize;
    let l3_expected = 2usize;

    // ---- L2 阶段开始 ----
    eta.update(
        PhaseKind::L2,
        0,
        l2_expected,
        started_at.elapsed().as_secs_f64(),
    );
    if let Some(sink) = progress {
        sink.on_l1_progress(&ImportL1Progress {
            phase: "l2",
            current: 0,
            total: l2_expected,
            message: "正在提取 L2 事件（双方 persona）...".to_string(),
            eta_seconds: eta.remaining_seconds().map(|s| s.round() as u64),
            l1_total,
            l2_total: Some(l2_expected),
            l3_total: None,
        });
    }

    engine.trigger_l2_check().await;

    // ---- L3 阶段开始（L2 已完成，回填 L2 样本） ----
    eta.update(
        PhaseKind::L2,
        l2_expected,
        l2_expected,
        started_at.elapsed().as_secs_f64(),
    );
    eta.update(
        PhaseKind::L3,
        0,
        l3_expected,
        started_at.elapsed().as_secs_f64(),
    );
    if let Some(sink) = progress {
        sink.on_l1_progress(&ImportL1Progress {
            phase: "l3",
            current: 0,
            total: l3_expected,
            message: "正在推断 L3 性格画像（双方 persona）...".to_string(),
            eta_seconds: eta.remaining_seconds().map(|s| s.round() as u64),
            l1_total,
            l2_total: Some(l2_expected),
            l3_total: Some(l3_expected),
        });
    }

    Ok(())
}

/// 构造导入完成摘要（两种分支的文案与桌面 done 事件一致）。
///
/// 参数:
/// - `l1`: L1 批量生成结果（提供成功 / 失败 / 处理次数三个计数，成组传入避免同型参数错位）；
/// - `l2_triggered` / `l3_triggered`: 深度阶段是否已触发；
/// - `total_sessions`: 导入会话总数。
///
/// 返回:
/// - `ImportDoneSummary`：宿主在完成事件中直接使用 `message` 与统计字段。
pub fn done_summary(
    l1: &ImportL1Outcome,
    l2_triggered: bool,
    l3_triggered: bool,
    total_sessions: usize,
) -> ImportDoneSummary {
    let message = if l1.l1_failed > 0 {
        format!(
            "深度处理完成: L1 成功 {}/{}, 失败 {}。请确认 LLM 已连接后重试。",
            l1.l1_success, l1.l1_processed, l1.l1_failed
        )
    } else {
        format!(
            "深度处理完成: L1 全部成功 ({}/{})",
            l1.l1_success, l1.l1_processed
        )
    };

    ImportDoneSummary {
        l1_success: l1.l1_success,
        l1_failed: l1.l1_failed,
        l2_triggered,
        l3_triggered,
        total_sessions,
        message,
    }
}

// =========================================================
// 引擎门面
// =========================================================

impl Engine {
    /// 解析 QQ 聊天记录文件并返回诊断报告（不写入数据库）。
    pub async fn analyze_qq_import(&self, req: AnalyzeRequest) -> RamariaResult<AnalysisReport> {
        analyze(self, req).await
    }

    /// 执行 QQ 聊天记录 L0 导入（双画像准备 + 会话 / 消息写入，不含 L1 生成）。
    ///
    /// 说明:
    /// - 需要引擎已附着 SQLite 连接池（`open_with` 装配自动携带；
    ///   `from_parts` 注入构造需先 [`Engine::attach_sqlite_pool`]）。
    pub async fn import_qq_l0(&self, req: ImportRequest) -> RamariaResult<ImportL0Outcome> {
        write_l0(self, req).await
    }

    /// 批量生成导入会话的 L1 摘要（可选级联与进度回调）。
    ///
    /// 参数:
    /// - `session_ids`: 待生成 L1 的会话（取 L0 导入结果）；
    /// - `plan`: 目标列表与节流 / 级联选项（见 [`ImportL1Plan`]）；
    /// - `progress`: 可选进度回调（逐 session 回调，含 ETA 估算）。
    pub async fn generate_import_l1(
        &self,
        session_ids: &[Uuid],
        plan: ImportL1Plan,
        progress: Option<&dyn ImportProgressSink>,
    ) -> RamariaResult<ImportL1Outcome> {
        generate_l1(self, session_ids, plan, progress).await
    }

    /// 触发导入后的深度处理（L2 事件提取 → L3 性格画像级联）。
    ///
    /// 用法:
    /// - 调用方在深度模式且至少一条 L1 生成成功时调用；
    /// - `l1_total` 传 L1 批量生成结果的总量（`None` 时进度事件不带 L1 总量）；
    /// - 阶段进度经 `progress` 回调（L2 / L3 各一条），内部失败只记日志。
    pub async fn trigger_import_deep(
        &self,
        l1_total: Option<usize>,
        progress: Option<&dyn ImportProgressSink>,
    ) -> RamariaResult<()> {
        trigger_deep(self, l1_total, progress).await
    }
}

// =========================================================
// 内部工具
// =========================================================

/// 校验导入文件扩展名（仅支持 qq-chat-exporter v6.x 导出的 .json）。
fn ensure_json_extension(path: &Path) -> RamariaResult<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" {
        return Err(RamariaError::validation(format!(
            "不支持的文件类型: .{ext}（仅支持 qq-chat-exporter v6.x 导出的 .json）"
        )));
    }
    Ok(())
}

/// 解析报告的时间范围文本（起止为空时为"未知"）。
fn report_time_range(report: &ramaria_importer::ImportReport) -> String {
    if report.time_start.is_empty() || report.time_end.is_empty() {
        "未知".to_string()
    } else {
        format!("{} ~ {}", report.time_start, report.time_end)
    }
}

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::{StorageBackend, StoreCrud};
    use ramaria_storage::SqliteStorage;
    use sqlx::SqlitePool;

    use crate::test_support::{L1_JSON_REPLY, MockLlm};

    // ---- 测试脚手架 ----

    /// 最小 qq-chat-exporter v6.x 导出（5 分钟切割线下两条 session）。
    ///
    /// 时间戳分布:
    /// - session 1: base 与 base+60s（双方各一条）；
    /// - session 2: base+3600s 与 base+3660s（双方各一条）。
    fn qq_export_json() -> String {
        let base = 1_700_000_000_000i64;
        let messages = [
            (base, "u_self", "小明", "早上好"),
            (base + 60_000, "u_peer", "小红", "早上好呀"),
            (base + 3_600_000, "u_self", "小明", "中午吃什么"),
            (base + 3_660_000, "u_peer", "小红", "吃面吧"),
        ];
        let body = messages
            .iter()
            .enumerate()
            .map(|(i, (ts, uid, name, text))| {
                format!(
                    r#"{{"id":"m_{i}","timestamp":{ts},"type":"text","recalled":false,"system":false,"content":{{"text":"{text}","elements":[]}},"sender":{{"uid":"{uid}","name":"{name}"}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"chatInfo":{{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"}},"messages":[{body}]}}"#
        )
    }

    /// 构造 L0 导入请求（默认双方、无画像覆盖、切割间隔 10 分钟）。
    fn import_request(file_path: &Path) -> ImportRequest {
        ImportRequest {
            file_path: file_path.to_path_buf(),
            mode: ImportMode::Fast,
            gap_minutes: 10,
            side: ImportSide::Both,
            persona_name: None,
            self_persona_uid: None,
            other_persona_name: None,
            other_persona_uid: None,
        }
    }

    /// 装配导入用例测试引擎（真实临时库 + mock LLM + 已附着连接池）。
    ///
    /// 说明:
    /// - 使用 `init_pool` 建库（含 migration），不经 `Engine::open_with`
    ///   （避免装配真实 provider）；
    /// - 嵌入 provider 注入 None（导入用例不依赖向量通道）。
    async fn import_engine(tag: &str) -> (Engine, SqlitePool, Arc<SqliteStorage>, PathBuf) {
        let dir = crate::test_support::temp_dir(tag);
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("测试库初始化应成功");
        let storage = Arc::new(SqliteStorage::new(pool.clone()));
        let engine = Engine::from_parts(
            storage.clone() as Arc<dyn StorageBackend>,
            Arc::new(MockLlm::with_reply(L1_JSON_REPLY)),
            None,
            RamariaConfig::default(),
        );
        engine.attach_sqlite_pool(pool.clone());
        (engine, pool, storage, dir)
    }

    /// 记录进度事件的测试 sink。
    struct RecordingSink {
        events: Mutex<Vec<ImportL1Progress>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<ImportL1Progress> {
            self.events.lock().expect("进度事件锁不应中毒").clone()
        }
    }

    impl ImportProgressSink for RecordingSink {
        fn on_l1_progress(&self, p: &ImportL1Progress) {
            self.events
                .lock()
                .expect("进度事件锁不应中毒")
                .push(p.clone());
        }

        fn on_done(&self, _summary: &ImportDoneSummary) {}
    }

    // ---- 解析 ----

    #[tokio::test]
    async fn analyze_reports_statistics_and_names() {
        let (engine, _pool, _storage, dir) = import_engine("import-analyze").await;
        let file_path = dir.join("export.json");
        std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

        let report = engine
            .analyze_qq_import(AnalyzeRequest {
                file_path: file_path.clone(),
                gap_minutes: 10,
            })
            .await
            .expect("解析用例应成功");

        assert_eq!(report.file_path, file_path);
        assert_eq!(report.self_name, "小明");
        assert_eq!(report.chat_name, "小红");
        assert_eq!(report.other_name, "小红");
        assert_eq!(report.total_raw, 4);
        assert_eq!(report.total_success, 4, "4 条 text 消息应全部成功解析");
        assert_eq!(report.total_skipped, 0);
        assert_eq!(
            report.session_count, 2,
            "同 session 内 60 秒间隔、跨 session 1 小时间隔"
        );
        assert_eq!(report.gap_minutes, 10);
        assert!(!report.time_range.is_empty(), "时间范围应非空");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- L0 写入 ----

    #[tokio::test]
    async fn write_l0_creates_sessions_messages_and_personas() {
        let (engine, pool, storage, dir) = import_engine("import-l0").await;
        let file_path = dir.join("export.json");
        std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

        let outcome = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect("L0 导入应成功");

        assert_eq!(outcome.mode, ImportMode::Fast);
        assert_eq!(outcome.sessions_written, 2);
        assert_eq!(outcome.messages_written, 4);
        assert_eq!(outcome.messages_dropped, 0);
        assert_eq!(outcome.skipped_count, 0);
        assert_eq!(outcome.session_ids.len(), 2);
        assert_eq!(outcome.persona_uid.as_deref(), Some("user-10001"));
        assert_eq!(outcome.other_persona_uid.as_deref(), Some("char-90002"));
        assert_eq!(outcome.self_name, "小明");
        assert_eq!(outcome.chat_name, "小红");
        assert!(!outcome.report_summary.is_empty());

        // 两个 source="qq" persona 各创建一次（导出者 user- / 对方 char-）
        let personas = ramaria_storage::repo::personas::list_all(&pool)
            .await
            .expect("读取 persona 列表应成功");
        let qq_personas: Vec<_> = personas.iter().filter(|p| p.source == "qq").collect();
        assert_eq!(qq_personas.len(), 2, "双画像应各创建一个 qq persona");
        assert!(qq_personas.iter().any(|p| p.uid == "user-10001"));
        assert!(qq_personas.iter().any(|p| p.uid == "char-90002"));

        // 消息按 session 落库（每个 session 2 条）
        let mut total_messages = 0usize;
        for session_id in &outcome.session_ids {
            total_messages += storage
                .list_messages(*session_id)
                .await
                .expect("读取会话消息应成功")
                .len();
        }
        assert_eq!(total_messages, 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同文件二次导入：指纹去重（跨批次查重命中 → 新增消息为 0）。
    ///
    /// 依据:
    /// - 消息指纹由 `ramaria-importer` 生成，写入 `messages.import_fingerprint`（全局 UNIQUE）；
    /// - `ImportWriter::write_l0` 写入前经 `find_by_fingerprint` 查重跳过，因此第二次导入
    ///   的消息新增数为 0；会话容器仍会创建（去重只跳过消息，不回滚会话）。
    #[tokio::test]
    async fn write_l0_second_import_deduplicates_fingerprints() {
        let (engine, pool, _storage, dir) = import_engine("import-dedup").await;
        let file_path = dir.join("export.json");
        std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

        let first = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect("首次导入应成功");
        assert_eq!(first.messages_written, 4);

        let second = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect("二次导入应成功");
        assert_eq!(second.messages_written, 0, "同指纹消息应被跨批次查重跳过");
        assert_eq!(second.messages_dropped, 0);
        assert_eq!(
            second.sessions_written, 2,
            "会话容器仍创建（去重只作用于消息）"
        );
        assert_eq!(second.session_ids.len(), 2);

        // 库内消息总数保持 4；persona 仍是 2 个（按 uid 命中复用）
        let message_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool)
            .await
            .expect("统计消息应成功");
        assert_eq!(message_count, 4);
        let personas = ramaria_storage::repo::personas::list_all(&pool)
            .await
            .expect("读取 persona 列表应成功");
        assert_eq!(personas.iter().filter(|p| p.source == "qq").count(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- L1 批量生成 ----

    #[tokio::test]
    async fn generate_l1_generates_for_both_personas_with_eta_progress() {
        let (engine, _pool, storage, dir) = import_engine("import-l1").await;
        let file_path = dir.join("export.json");
        std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

        let outcome = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect("L0 导入应成功");
        let self_uid = outcome
            .persona_uid
            .clone()
            .expect("导出者 persona 应已创建");
        let other_uid = outcome
            .other_persona_uid
            .clone()
            .expect("对方 persona 应已创建");

        let sink = RecordingSink::new();
        let plan = ImportL1Plan {
            targets: vec![Some(self_uid), Some(other_uid)],
            cascade: false,
            throttle_ms: 0,
        };
        let l1 = engine
            .generate_import_l1(&outcome.session_ids, plan, Some(&sink))
            .await
            .expect("L1 批量生成应成功");

        assert_eq!(l1.l1_total, 4, "每 session 双方 persona 各一次");
        assert_eq!(l1.l1_success, 4);
        assert_eq!(l1.l1_failed, 0);
        assert_eq!(l1.l1_skipped, 0);
        assert_eq!(l1.l1_processed, 4);
        assert_eq!(l1.session_ids.len(), 2);

        // 库内 L1 行数与调用数一致（每 session 两份：self / other 各一）
        let mut l1_rows = 0usize;
        for session_id in &outcome.session_ids {
            l1_rows += storage
                .list_memory_l1(*session_id)
                .await
                .expect("读取 L1 应成功")
                .len();
        }
        assert_eq!(l1_rows, 4, "每 session 应落两份 L1（双方 persona）");

        // ETA 进度回调：起始一条（0/4）+ 每 session 一条，阶段均为 l1，末条为完成态
        let events = sink.events();
        assert_eq!(events.len(), 3, "起始进度 + 每 session 一条");
        assert!(events.iter().all(|e| e.phase == "l1"));
        assert_eq!(events[0].current, 0);
        assert_eq!(events[0].total, 4);
        assert_eq!(events[0].l1_total, Some(4));
        let last = events.last().expect("应有进度事件");
        assert_eq!(last.current, 4);
        assert_eq!(last.total, 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 边界与降级 ----

    #[tokio::test]
    async fn write_l0_rejects_empty_messages_file() {
        let (engine, _pool, _storage, dir) = import_engine("import-empty").await;
        let file_path = dir.join("empty.json");
        let empty = r#"{"chatInfo":{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"},"messages":[]}"#;
        std::fs::write(&file_path, empty).expect("写入空导出文件应成功");

        let err = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect_err("空消息文件应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.context().contains("没有可导入的消息"),
            "错误应说明没有可导入消息: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_l0_rejects_non_json_extension() {
        let (engine, _pool, _storage, dir) = import_engine("import-ext").await;
        let file_path = dir.join("export.txt");
        std::fs::write(&file_path, qq_export_json()).expect("写入非 .json 文件应成功");

        let err = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect_err("非 .json 扩展名应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.context().contains("不支持的文件类型"),
            "错误应说明扩展名不支持: {err}"
        );

        // 解析用例同样执行扩展名校验
        let err = engine
            .analyze_qq_import(AnalyzeRequest {
                file_path,
                gap_minutes: 10,
            })
            .await
            .expect_err("非 .json 扩展名应报错");
        assert_eq!(err.category(), "validation");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_l0_requires_attached_pool() {
        let dir = crate::test_support::temp_dir("import-no-pool");
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("测试库初始化应成功");
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));
        // 注入构造不携带连接池：导入用例应显式报错而不是 panic
        let engine = Engine::from_parts(
            storage,
            Arc::new(MockLlm::with_reply(L1_JSON_REPLY)),
            None,
            RamariaConfig::default(),
        );
        let file_path = dir.join("export.json");
        std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

        let err = engine
            .import_qq_l0(import_request(&file_path))
            .await
            .expect_err("未附着连接池应显式报错");
        assert_eq!(err.category(), "unsupported");
        assert!(
            err.context().contains("attach_sqlite_pool"),
            "错误应指向连接池附着入口: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 深度触发与完成摘要 ----

    #[tokio::test]
    async fn trigger_deep_emits_l2_then_l3_progress() {
        let (engine, _pool, _storage, dir) = import_engine("import-deep").await;
        let sink = RecordingSink::new();

        engine
            .trigger_import_deep(Some(4), Some(&sink))
            .await
            .expect("深度触发应成功");

        let events = sink.events();
        assert_eq!(events.len(), 2, "应发送 L2 / L3 两条阶段进度");
        assert_eq!(events[0].phase, "l2");
        assert_eq!(events[0].current, 0);
        assert_eq!(events[0].total, 2);
        assert_eq!(events[0].l1_total, Some(4), "应回填调用方传入的 L1 总量");
        assert_eq!(events[0].l2_total, Some(2));
        assert_eq!(events[1].phase, "l3");
        assert_eq!(events[1].current, 0);
        assert_eq!(events[1].total, 2);
        assert_eq!(events[1].l1_total, Some(4));
        assert_eq!(events[1].l2_total, Some(2));
        assert_eq!(events[1].l3_total, Some(2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn done_summary_matches_desktop_branches() {
        let success = l1_outcome(4, 0, 0, 4);
        let ok = done_summary(&success, true, true, 2);
        assert_eq!(ok.message, "深度处理完成: L1 全部成功 (4/4)");
        assert!(ok.l2_triggered && ok.l3_triggered);
        assert_eq!(ok.total_sessions, 2);
        assert_eq!(ok.l1_success, 4);

        let partial = l1_outcome(2, 1, 1, 4);
        let failed = done_summary(&partial, true, true, 2);
        assert_eq!(
            failed.message,
            "深度处理完成: L1 成功 2/4, 失败 1。请确认 LLM 已连接后重试。"
        );
        assert_eq!(failed.l1_failed, 1);
    }

    /// 构造 L1 批量生成结果（完成摘要用例的最小输入）。
    fn l1_outcome(
        success: usize,
        failed: usize,
        skipped: usize,
        processed: usize,
    ) -> ImportL1Outcome {
        ImportL1Outcome {
            l1_success: success,
            l1_failed: failed,
            l1_skipped: skipped,
            l1_processed: processed,
            l1_total: processed,
            session_ids: Vec::new(),
        }
    }
}
