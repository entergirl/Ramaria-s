//! crates/ramaria-eval/src/sandbox/report.rs - Ramaria 评测沙盒报告与轨迹渲染
//!
//! 设计特点:
//! - 报告只存纯数据：路径、标识、计数、轨迹与耗时，不持有引擎或存储句柄
//! - 阶段记录分 ok / warn / failed / skipped，界面按状态着色由调用方决定
//! - 轨迹逐字段展开四通道、融合、丢弃、预算与原文通道，便于定位召回断点
//! - 文本渲染不读库、不算阈值，阈值进度由运行阶段写入 detail

use std::path::PathBuf;

use ramaria_service::{
    CascadeOutcome, DropReason, RecallTrace, RecallWithTrace, SealOutcome, TraceChannel,
};
use uuid::Uuid;

use super::backend::BackendReport;

// =========================================================
// 阶段上报
// =========================================================

/// 沙盒阶段标识（按执行顺序排列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    /// 目录准备
    Prepare,
    /// 引擎装配
    Open,
    /// 评测后端解析与应用
    Backend,
    /// 人格创建 / 导入
    Persona,
    /// 对话写入
    Ingest,
    /// 会话封存
    Seal,
    /// L2 / L3 级联
    Cascade,
    /// 检索索引重建
    Index,
    /// 带轨迹召回
    Recall,
    /// 生成回答
    Answer,
}

impl StageKind {
    /// 返回阶段的稳定小写标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Open => "open",
            Self::Backend => "backend",
            Self::Persona => "persona",
            Self::Ingest => "ingest",
            Self::Seal => "seal",
            Self::Cascade => "cascade",
            Self::Index => "index",
            Self::Recall => "recall",
            Self::Answer => "answer",
        }
    }
}

/// 阶段结果状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    /// 完成
    Ok,
    /// 降级完成（记入警告，继续后续阶段）
    Warn,
    /// 失败并中止
    Failed,
    /// 未执行
    Skipped,
}

impl StageStatus {
    /// 返回状态的稳定小写标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// 单个阶段的上报条目。
///
/// 字段约定:
/// - `kind`: 阶段标识；
/// - `status`: 阶段结果状态；
/// - `elapsed_ms`: 阶段耗时（毫秒）；
/// - `detail`: 可读摘要（计数、阈值进度或失败原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageReport {
    pub kind: StageKind,
    pub status: StageStatus,
    pub elapsed_ms: u64,
    pub detail: String,
}

// =========================================================
// 沙盒报告
// =========================================================

/// 一次沙盒运行的完整报告。
///
/// 字段约定:
/// - `root`: 沙盒根目录；
/// - `backend`: 本次运行的后端来源、模型、地址与连通性；
/// - `persona_uid` / `session_id` / `message_count`: 本次人格、写入会话与消息数；
/// - `seal` / `cascade` / `index_docs`: 封装、级联与索引结果（失败时按占位或 `None`）；
/// - `recall`: 召回结果与诊断轨迹；
/// - `question` / `answer` / `answer_error`: 问题与回答（回答失败时只留错误）；
/// - `warnings`: 降级阶段的告警文本；
/// - `stages`: 阶段上报序列；
/// - `total_elapsed_ms` / `recall_elapsed_ms` / `answer_elapsed_ms`: 耗时明细。
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxReport {
    pub root: PathBuf,
    pub backend: BackendReport,
    pub persona_uid: String,
    pub session_id: Uuid,
    pub message_count: usize,
    pub seal: SealOutcome,
    pub cascade: Option<CascadeOutcome>,
    pub index_docs: Option<usize>,
    pub recall: RecallWithTrace,
    pub question: String,
    pub answer: Option<String>,
    pub answer_error: Option<String>,
    pub warnings: Vec<String>,
    pub stages: Vec<StageReport>,
    pub total_elapsed_ms: u64,
    pub recall_elapsed_ms: u64,
    pub answer_elapsed_ms: u64,
}

// =========================================================
// 文本渲染
// =========================================================

/// 把报告渲染为终端文本（标题、阶段、警告、轨迹与答案）。
///
/// 说明:
/// - 只读报告字段，不重新开库；
/// - 无答案时显示 `answer_error`，两者都缺时显示原因未知。
pub fn render_text(report: &SandboxReport) -> String {
    let mut lines = Vec::new();
    lines.push("=== Ramaria 评测沙盒 ===".to_string());
    lines.push(format!("库路径: {}", report.root.display()));
    lines.push(format!(
        "后端: {}（{}）",
        report.backend.source,
        report.backend.kind.label()
    ));
    lines.push(format!("模型: {}", report.backend.model_text()));
    lines.push(format!("地址: {}", report.backend.base_url));
    lines.push(format!("连接: {}", report.backend.llm_status));
    lines.push(format!("向量: {}", report.backend.embedding_status));
    lines.push(format!("人格: {}", report.persona_uid));
    lines.push(format!(
        "会话: {} / 消息 {} 条",
        report.session_id, report.message_count
    ));
    lines.push(if report.seal.sealed {
        format!("封存: 已完成，L1 摘要 {} 条", report.seal.l1_count)
    } else if report
        .warnings
        .iter()
        .any(|warning| warning.starts_with("seal:"))
    {
        "封存: 失败（见告警）".to_string()
    } else {
        "封存: 未完成（本次调用未抢占到封存）".to_string()
    });
    lines.push(match &report.cascade {
        Some(outcome) => format!(
            "级联: L1 消费 {} 条 / L2 事件 {} 条 / L3 标签 {} 个",
            outcome.l1_consumed, outcome.l2_events, outcome.l3_traits
        ),
        None => "级联: 未执行（失败已记为告警）".to_string(),
    });
    lines.push(match report.index_docs {
        Some(total) => format!("索引: {total} 篇文档"),
        None => "索引: 未重建（失败已记为告警）".to_string(),
    });
    lines.push(format!(
        "耗时: 总计 {} ms（召回 {} ms / 回答 {} ms）",
        report.total_elapsed_ms, report.recall_elapsed_ms, report.answer_elapsed_ms
    ));

    lines.push(String::new());
    lines.push("阶段:".to_string());
    for stage in &report.stages {
        lines.push(format!(
            "  [{}] {} {}ms - {}",
            stage.status.as_str(),
            stage.kind.as_str(),
            stage.elapsed_ms,
            stage.detail
        ));
    }

    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("警告:".to_string());
        for warning in &report.warnings {
            lines.push(format!("  - {warning}"));
        }
    }

    lines.push(String::new());
    lines.push(format!("问题: {}", report.question));
    lines.push(String::new());
    lines.push("召回轨迹:".to_string());
    for line in trace_lines(&report.recall.trace) {
        lines.push(format!("  {line}"));
    }
    lines.push(String::new());
    lines.push("答案:".to_string());
    match &report.answer {
        Some(answer) => lines.push(answer.replace("||", "\n")),
        None => {
            let reason = report.answer_error.as_deref().unwrap_or("原因未知");
            lines.push(format!("（未生成）{reason}"));
        }
    }

    lines.join("\n")
}

/// 展开召回轨迹为可读文本行。
///
/// 说明:
/// - 四通道固定按 vector / bm25 / graph / keyword 输出，缺席通道按未参与显示；
/// - 丢弃原因用 snake_case 文本，与轨迹产物口径一致；
/// - 只输出标识、分数与字符数，不含查询文本与记忆原文。
pub fn trace_lines(trace: &RecallTrace) -> Vec<String> {
    let mut lines = Vec::new();
    let dimension = match trace.query_vector_dim {
        Some(dim) => dim.to_string(),
        None => "未生成".to_string(),
    };
    lines.push(format!(
        "查询: {} 字符 / 向量维度 {}",
        trace.query_chars, dimension
    ));

    for channel in [
        TraceChannel::Vector,
        TraceChannel::Bm25,
        TraceChannel::Graph,
        TraceChannel::Keyword,
    ] {
        let found = trace
            .retrieval
            .channels
            .iter()
            .find(|item| item.channel == channel);
        let (active, candidates) = match found {
            Some(item) => (item.active, item.candidates.as_slice()),
            None => (false, &[][..]),
        };
        lines.push(format!(
            "通道 {}: {}，候选 {} 条",
            channel_name(channel),
            if active { "参与" } else { "未参与" },
            candidates.len()
        ));
        for candidate in candidates {
            lines.push(format!(
                "  #{} {} raw={:.4}",
                candidate.rank, candidate.id, candidate.raw_score
            ));
        }
    }

    lines.push(format!("融合结果: {} 条", trace.retrieval.fused.len()));
    for entry in &trace.retrieval.fused {
        lines.push(format!(
            "  {} layer={} rrf={:.4} decay={:.4} decayed={:.4} raw[vector={} bm25={} graph={} keyword={}]",
            entry.id,
            entry.layer,
            entry.rrf_score,
            entry.decay_factor,
            entry.decayed_score,
            score_text(entry.vector_raw),
            score_text(entry.bm25_raw),
            score_text(entry.graph_raw),
            score_text(entry.keyword_raw)
        ));
    }

    lines.push(format!("检索丢弃: {} 条", trace.retrieval.dropped.len()));
    for item in &trace.retrieval.dropped {
        lines.push(format!(
            "  {} reason={} score={:.4}",
            item.id,
            drop_reason_name(item.reason),
            item.score
        ));
    }

    lines.push(format!("装配丢弃: {} 条", trace.dropped.len()));
    for item in &trace.dropped {
        lines.push(format!(
            "  {} reason={} score={:.4}",
            item.id,
            drop_reason_name(item.reason),
            item.score
        ));
    }

    lines.push(format!(
        "条数预算: 上限 {} / 候选 {} / 保留 {} / tokens {} -> {}",
        trace.budget.max_memories,
        trace.budget.candidates,
        trace.budget.kept,
        trace.budget.tokens_before,
        trace.budget.tokens_after
    ));
    lines.push(format!(
        "原文通道: {}，候选 {} 条，渲染 {} 字符 / 预算 {} 字符",
        if trace.utt.active { "启用" } else { "关闭" },
        trace.utt.candidates.len(),
        trace.utt.rendered_chars,
        trace.utt.budget_chars
    ));
    for candidate in &trace.utt.candidates {
        lines.push(format!(
            "  #{} {} raw={:.4}",
            candidate.rank, candidate.id, candidate.raw_score
        ));
    }

    lines
}

/// 通道的稳定小写标识。
fn channel_name(channel: TraceChannel) -> &'static str {
    match channel {
        TraceChannel::Vector => "vector",
        TraceChannel::Bm25 => "bm25",
        TraceChannel::Graph => "graph",
        TraceChannel::Keyword => "keyword",
    }
}

/// 丢弃原因的稳定 snake_case 文本。
fn drop_reason_name(reason: DropReason) -> &'static str {
    match reason {
        DropReason::FusionCut => "fusion_cut",
        DropReason::LayerFiltered => "layer_filtered",
        DropReason::PersonaShare => "persona_share",
        DropReason::MaxMemories => "max_memories",
        DropReason::DocMissing => "doc_missing",
        DropReason::PersonaMismatch => "persona_mismatch",
        DropReason::Future => "future",
    }
}

/// 可选分数的显示文本（缺省显示 `-`）。
fn score_text(score: Option<f64>) -> String {
    match score {
        Some(value) => format!("{value:.4}"),
        None => "-".to_string(),
    }
}
