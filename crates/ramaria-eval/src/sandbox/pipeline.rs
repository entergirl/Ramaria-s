//! crates/ramaria-eval/src/sandbox/pipeline.rs - Ramaria 评测沙盒运行器
//!
//! 设计特点:
//! - 单次运行按固定顺序推进：目录 → 装配 → 后端 → 人格 → 写入 → 封存 → 级联 → 索引 →
//!   召回 → 回答
//! - 阶段计时与摘要统一由记录器推送，运行体只关心结果与降级分支
//! - 后端来源由运行选项决定（继承用户配置只读 / 独立配置），应用失败降级不中止
//! - 降级面固定：封存 / 级联 / 索引 / 回答失败只告警，保留报告其余部分
//! - 引擎在本模块内创建并显式关闭连接池，不留后台任务与文件句柄
//! - 阈值始终取产品默认快照，不随用户配置变化

use std::path::Path;
use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{
    CascadeOutcome, CascadeUntil, ChatRole, ChatSendRequest, ChatTurn, Engine, IngestRequest,
    PersonaFileAction, PersonaLoadMode, RecallRequest, RecallWithTrace, SealOutcome,
};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use super::backend::{BackendReport, apply_backend, resolve_backend, summary};
use super::report::{SandboxReport, StageKind, StageReport, StageStatus};
use super::{SandboxInput, SandboxOptions, root};
use crate::assembly;
use crate::measure::elapsed_ms;

/// 默认沙盒人格 uid。
pub const DEFAULT_SANDBOX_PERSONA_UID: &str = "sandbox-0001";

/// 默认沙盒人格名称。
pub const DEFAULT_SANDBOX_PERSONA_NAME: &str = "沙盒人格";

/// 沙盒会话来源通道（写入与生成共用）。
const SANDBOX_CHANNEL: &str = "sandbox";

/// 对话写入的外部对话标识。
const SANDBOX_CONVERSATION_ID: &str = "sandbox";

/// 提问会话的外部对话标识（与写入会话分开，避免问题污染对话库）。
const SANDBOX_QUESTION_CONVERSATION_ID: &str = "sandbox-question";

// =========================================================
// 运行入口
// =========================================================

/// 执行一次沙盒运行并组装报告。
pub(crate) async fn run(
    options: &SandboxOptions,
    input: SandboxInput,
    progress: Option<UnboundedSender<StageReport>>,
) -> RamariaResult<SandboxReport> {
    let started = Instant::now();
    let mut recorder = StageRecorder::new(progress);
    // 提前取出问题：目录准备失败时报告也不会产出，无需保留输入
    let outcome = run_with_root(options, input, &mut recorder).await;
    let run = match outcome {
        Ok(run) => run,
        Err(error) => return Err(error),
    };

    Ok(SandboxReport {
        root: options.root.clone(),
        backend: run.backend,
        persona_uid: run.persona_uid,
        session_id: run.session_id,
        message_count: run.message_count,
        seal: run.seal,
        cascade: run.cascade,
        index_docs: run.index_docs,
        recall: run.recall,
        question: run.question,
        answer: run.answer,
        answer_error: run.answer_error,
        warnings: recorder.warnings,
        stages: recorder.stages,
        total_elapsed_ms: elapsed_ms(started),
        recall_elapsed_ms: run.recall_elapsed_ms,
        answer_elapsed_ms: run.answer_elapsed_ms,
    })
}

/// 准备目录 → 装配引擎 → 应用后端 → 执行阶段 → 关闭连接池。
async fn run_with_root(
    options: &SandboxOptions,
    input: SandboxInput,
    recorder: &mut StageRecorder,
) -> RamariaResult<SandboxRun> {
    let root_dir = options.root.as_path();
    let started = Instant::now();
    root::prepare(root_dir).map_err(|error| {
        recorder.fail(
            StageKind::Prepare,
            started,
            format!("目录准备失败: {error}"),
        );
        error
    })?;
    recorder.ok(
        StageKind::Prepare,
        started,
        format!("已重建沙盒目录 {}", root_dir.display()),
    );

    let started = Instant::now();
    let engine = assembly::open_engine(root_dir).await.map_err(|error| {
        recorder.fail(StageKind::Open, started, format!("引擎装配失败: {error}"));
        error
    })?;
    recorder.ok(StageKind::Open, started, "引擎已装配".to_string());

    // ---- 后端（解析失败中止：云端缺项属校验错误；应用失败降级） ----
    let started = Instant::now();
    let resolved = match resolve_backend(&options.backend, options.user_db.as_deref()).await {
        Ok(resolved) => resolved,
        Err(error) => {
            recorder.fail(
                StageKind::Backend,
                started,
                format!("后端解析失败: {error}"),
            );
            close_engine(engine).await;
            return Err(error);
        }
    };
    let (backend, warnings) = apply_backend(&engine, resolved).await;
    let detail = summary(&backend);
    if warnings.is_empty() {
        recorder.ok(StageKind::Backend, started, detail);
    } else {
        // 后端摘要是阶段详情；告警列表只放具体降级原因，避免同一句出现两次。
        recorder.record(StageKind::Backend, StageStatus::Warn, started, detail);
        recorder.warnings.extend(warnings);
    }

    let outcome = run_stages(&engine, root_dir, input, recorder, backend).await;
    close_engine(engine).await;
    outcome
}

/// 运行写入到回答的全部阶段。
async fn run_stages(
    engine: &Engine,
    root_dir: &Path,
    input: SandboxInput,
    recorder: &mut StageRecorder,
    backend: BackendReport,
) -> RamariaResult<SandboxRun> {
    // ---- 人格 ----
    let started = Instant::now();
    let persona_uid = ensure_persona(engine, root_dir, input.persona_path.as_deref())
        .await
        .map_err(|error| {
            recorder.fail(
                StageKind::Persona,
                started,
                format!("人格准备失败: {error}"),
            );
            error
        })?;
    recorder.ok(
        StageKind::Persona,
        started,
        format!("人格 {persona_uid} 已就绪"),
    );

    // ---- 写入 ----
    let started = Instant::now();
    let ingest = engine
        .ingest(IngestRequest {
            messages: input.turns,
            persona: Some(persona_uid.clone()),
            conversation_id: Some(SANDBOX_CONVERSATION_ID.to_string()),
            channel: SANDBOX_CHANNEL.to_string(),
            finalize: false,
        })
        .await
        .map_err(|error| {
            recorder.fail(StageKind::Ingest, started, format!("写入失败: {error}"));
            error
        })?;
    let session_id = ingest.session_id;
    let message_count = engine.count_session_messages(session_id).await;
    recorder.ok(
        StageKind::Ingest,
        started,
        format!(
            "写入 {} 条消息（去重 {} 条），会话内共 {} 条",
            ingest.written, ingest.deduplicated, message_count
        ),
    );

    // ---- 封存（失败降级） ----
    let started = Instant::now();
    let seal = match engine.seal(session_id).await {
        Ok(outcome) => {
            recorder.ok(
                StageKind::Seal,
                started,
                format!("封存完成，L1 摘要 {} 条", outcome.l1_count),
            );
            outcome
        }
        Err(error) => {
            recorder.warn(
                StageKind::Seal,
                started,
                format!("封存失败，继续运行: {error}"),
            );
            SealOutcome {
                session_id,
                sealed: false,
                l1_count: 0,
            }
        }
    };

    // ---- 级联（失败降级；阈值取产品默认快照） ----
    let config = engine.config();
    let l2_trigger_count = config.thresholds.l2_trigger_count;
    let l2_trigger_days = config.thresholds.l2_trigger_days;
    let l3_trigger_count = config.thresholds.l3_trigger_count;
    let l3_trigger_days = config.thresholds.l3_trigger_days;
    drop(config);

    let started = Instant::now();
    let cascade = match engine.cascade(&persona_uid, CascadeUntil::L3).await {
        Ok(outcome) => {
            recorder.ok(
                StageKind::Cascade,
                started,
                format!(
                    "L1 摘要 {} 条（L2 阈值 {l2_trigger_count} 条 / {l2_trigger_days} 天）；L1 消费 {} 条、L2 事件 {} 条（L3 阈值 {l3_trigger_count} 条 / {l3_trigger_days} 天）；L3 标签 {} 个",
                    seal.l1_count, outcome.l1_consumed, outcome.l2_events, outcome.l3_traits
                ),
            );
            Some(outcome)
        }
        Err(error) => {
            recorder.warn(
                StageKind::Cascade,
                started,
                format!("级联失败，继续运行: {error}"),
            );
            None
        }
    };

    // ---- 索引（失败降级） ----
    let started = Instant::now();
    let index_docs = match engine.rebuild_index().await {
        Ok(total) => {
            recorder.ok(StageKind::Index, started, format!("索引文档 {total} 篇"));
            Some(total)
        }
        Err(error) => {
            recorder.warn(
                StageKind::Index,
                started,
                format!("索引重建失败，继续运行: {error}"),
            );
            None
        }
    };

    // ---- 召回（失败中止：没有轨迹无法交付） ----
    let question = input.question;
    let started = Instant::now();
    let recall = engine
        .recall_with_trace(RecallRequest {
            query: Some(question.clone()),
            persona: Some(persona_uid.clone()),
            messages: vec![ChatTurn {
                role: ChatRole::User,
                content: question.clone(),
            }],
            ..Default::default()
        })
        .await
        .map_err(|error| {
            recorder.fail(StageKind::Recall, started, format!("召回失败: {error}"));
            error
        })?;
    let recall_elapsed_ms = elapsed_ms(started);
    recorder.ok(
        StageKind::Recall,
        started,
        format!(
            "召回 {} 条，预算保留 {} 条",
            recall.result.items.len(),
            recall.trace.budget.kept
        ),
    );

    // ---- 回答（失败降级） ----
    let started = Instant::now();
    let (answer, answer_error) = match engine
        .chat_send(ChatSendRequest {
            message: question.clone(),
            persona: Some(persona_uid.clone()),
            session_id: None,
            conversation_id: Some(SANDBOX_QUESTION_CONVERSATION_ID.to_string()),
            channel: SANDBOX_CHANNEL.to_string(),
        })
        .await
    {
        Ok(outcome) => {
            recorder.ok(
                StageKind::Answer,
                started,
                format!("答案 {} 字符", outcome.chars),
            );
            (Some(outcome.reply), None)
        }
        Err(error) => {
            recorder.warn(StageKind::Answer, started, format!("回答失败: {error}"));
            (None, Some(error.to_string()))
        }
    };
    let answer_elapsed_ms = elapsed_ms(started);

    Ok(SandboxRun {
        backend,
        persona_uid,
        session_id,
        message_count,
        seal,
        cascade,
        index_docs,
        recall,
        question,
        answer,
        answer_error,
        recall_elapsed_ms,
        answer_elapsed_ms,
    })
}

// =========================================================
// 人格准备
// =========================================================

/// 准备沙盒人格：有预设则导入，否则创建内置沙盒人格。
///
/// 返回:
/// - 目标人格 uid；
/// - 预设未给出 uid、默认人格文件写入失败或导入失败返回对应错误。
pub(crate) async fn ensure_persona(
    engine: &Engine,
    root_dir: &Path,
    preset: Option<&Path>,
) -> RamariaResult<String> {
    let Some(preset) = preset else {
        return ensure_default_persona(engine, root_dir).await;
    };

    let outcome = engine.persona_import(preset, None, true).await?;
    let uid = outcome.preview.registration.uid;
    uid.ok_or_else(|| RamariaError::validation(format!("人格预设未给出 uid: {}", preset.display())))
}

/// 创建内置沙盒人格（写在沙盒根下，不入用户人格目录）。
///
/// 返回:
/// - 成功时返回 `sandbox-0001`；
/// - 文件写入失败返回 `Io` 错误；导入被标记为失败返回 `Validation` 错误。
pub(crate) async fn ensure_default_persona(
    engine: &Engine,
    root_dir: &Path,
) -> RamariaResult<String> {
    let dir = root_dir.join("persona");
    std::fs::create_dir_all(&dir).map_err(|error| {
        RamariaError::io(
            format!("创建沙盒人格目录失败: {}", dir.display()),
            Some(error),
        )
    })?;

    let path = dir.join(format!("{DEFAULT_SANDBOX_PERSONA_UID}.toml"));
    let content = format!("assistant_name = \"{DEFAULT_SANDBOX_PERSONA_NAME}\"\n");
    std::fs::write(&path, content).map_err(|error| {
        RamariaError::io(
            format!("写入沙盒人格文件失败: {}", path.display()),
            Some(error),
        )
    })?;

    let outcome = engine
        .persona_load_file(
            &path,
            DEFAULT_SANDBOX_PERSONA_UID,
            DEFAULT_SANDBOX_PERSONA_NAME,
            PersonaLoadMode::CreateMissing,
        )
        .await;
    if outcome.action == PersonaFileAction::Failed {
        return Err(RamariaError::validation(format!(
            "创建沙盒人格失败: {}",
            outcome.message
        )));
    }
    Ok(DEFAULT_SANDBOX_PERSONA_UID.to_string())
}

// =========================================================
// 引擎生命周期与阶段记录
// =========================================================

/// 关闭连接池并丢弃引擎，释放 SQLite 文件句柄。
async fn close_engine(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
}

/// 一次运行的中间结果（报告组装前）。
struct SandboxRun {
    backend: BackendReport,
    persona_uid: String,
    session_id: Uuid,
    message_count: usize,
    seal: SealOutcome,
    cascade: Option<CascadeOutcome>,
    index_docs: Option<usize>,
    recall: RecallWithTrace,
    question: String,
    answer: Option<String>,
    answer_error: Option<String>,
    recall_elapsed_ms: u64,
    answer_elapsed_ms: u64,
}

/// 阶段记录器：累计上报条目与告警，并逐条推送进度。
struct StageRecorder {
    stages: Vec<StageReport>,
    warnings: Vec<String>,
    progress: Option<UnboundedSender<StageReport>>,
}

impl StageRecorder {
    fn new(progress: Option<UnboundedSender<StageReport>>) -> Self {
        Self {
            stages: Vec::new(),
            warnings: Vec::new(),
            progress,
        }
    }

    fn record(&mut self, kind: StageKind, status: StageStatus, started: Instant, detail: String) {
        let stage = StageReport {
            kind,
            status,
            elapsed_ms: elapsed_ms(started),
            detail,
        };
        if let Some(sender) = &self.progress {
            // 订阅方提前退出不影响运行
            let _ = sender.send(stage.clone());
        }
        self.stages.push(stage);
    }

    fn ok(&mut self, kind: StageKind, started: Instant, detail: String) {
        self.record(kind, StageStatus::Ok, started, detail);
    }

    fn warn(&mut self, kind: StageKind, started: Instant, detail: String) {
        self.warnings.push(format!("{}: {}", kind.as_str(), detail));
        self.record(kind, StageStatus::Warn, started, detail);
    }

    fn fail(&mut self, kind: StageKind, started: Instant, detail: String) {
        self.record(kind, StageStatus::Failed, started, detail);
    }
}
