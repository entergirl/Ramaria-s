//! crates/ramaria-eval/src/run/exec.rs - Ramaria 评测单题执行
//!
//! 设计特点:
//! - 一题一次执行：新会话 → 可选预置上下文 → 生成，重试只包住生成这一跳
//! - 预置上下文按管线分支：full-context 取全量摘要，oracle-l1 取 gold 覆盖到的摘要
//! - 失败分两档：会话 / 上下文阶段写 0 次尝试，生成阶段写到超限那一次
//! - 条件级上下文打包成结构体，避免长参数列表，也让库路径与缓存随条件走
//! - 日志只记题目标识 / 条件 / 重试次数 / 错误分类，不记问题与回复正文

use std::path::Path;
use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{ChatSendRequest, Engine, EvalChatOptions};

use crate::measure::elapsed_ms;

use super::context::build_full_context;
use super::item::{ConditionLine, RunItemRecord};
use super::matrix::{Condition, Pipeline};
use super::oracle_context::OracleCache;
use super::plan::RunItem;

/// 评测问答使用的通道标识（与运行器口径一致）。
const EVAL_CHANNEL: &str = "eval";

// =========================================================
// 单题执行
// =========================================================

/// 单题执行结果。
///
/// 字段约定:
/// - `record`: 要追加到逐题产物的那一行；
/// - `success`: 结果类别（成功后写入断点集合）；
/// - `attempts`: 本题累计尝试次数（含首次）；
/// - `error_kind`: 错误分类标识；成功为 `None`。
pub(super) struct ItemRun {
    pub(super) record: RunItemRecord,
    pub(super) success: bool,
    pub(super) attempts: u32,
    pub(super) error_kind: Option<&'static str>,
}

impl ItemRun {
    /// 会话或上下文阶段的失败（尚未进入生成）。
    fn failed(
        item: &RunItem,
        condition: &Condition,
        error: &RamariaError,
        effective: &str,
        line: &ConditionLine,
    ) -> Self {
        Self {
            record: RunItemRecord::failure(
                item,
                condition,
                &error.to_string(),
                0,
                0,
                effective,
                None,
                line.clone(),
            ),
            success: false,
            attempts: 0,
            error_kind: Some(error_kind(error)),
        }
    }
}

/// 单题执行的固定环境（同一条件内复用，避免长参数列表）。
///
/// 字段约定:
/// - `engine` / `condition` / `line` / `effective`: 条件级上下文；
/// - `retry_limit`: 该条件的重试上限；
/// - `work_db`: 该条件工作库路径，也是 oracle 索引缓存的键；
/// - `oracle`: 压缩层索引缓存；只有 oracle-l1 管线持有，其余为 `None`。
///
/// 说明: 除 `oracle` 外均为不可变借用，因此同一条件的多道题可以各建一份上下文并发执行。
pub(super) struct ItemContext<'a> {
    engine: &'a Engine,
    condition: &'a Condition,
    line: &'a ConditionLine,
    effective: &'a str,
    retry_limit: u32,
    work_db: &'a Path,
    oracle: Option<&'a mut OracleCache>,
}

impl<'a> ItemContext<'a> {
    /// 装配一个条件的单题执行环境。
    ///
    /// 参数:
    /// - `engine`: 该条件引擎；
    /// - `condition`: 条件（管线 / 预算 / 模型）；
    /// - `line`: 逐题产物里重复写入的条件行；
    /// - `effective`: 实际生效的答题模型标识；
    /// - `retry_limit`: 单题重试上限；
    /// - `work_db`: 该条件工作库路径（oracle 索引缓存键）；
    /// - `oracle`: 跨条件复用的索引缓存；非 oracle-l1 管线传 `None`。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        engine: &'a Engine,
        condition: &'a Condition,
        line: &'a ConditionLine,
        effective: &'a str,
        retry_limit: u32,
        work_db: &'a Path,
        oracle: Option<&'a mut OracleCache>,
    ) -> Self {
        Self {
            engine,
            condition,
            line,
            effective,
            retry_limit,
            work_db,
            oracle,
        }
    }
}

/// 按管线装配预置上下文。
///
/// 参数:
/// - `context`: 单题执行环境；
/// - `item`: 当前题目（提供人格与 gold 消息）。
///
/// 返回:
/// - `full-context`: 该人格的全量 L1 摘要；
/// - `oracle-l1`: gold 覆盖到的 L1 摘要（空 gold 为空串，存在即跳过召回）；
/// - 其余管线为 `None`，走检索路径。
async fn preset_context(
    context: &mut ItemContext<'_>,
    item: &RunItem,
) -> RamariaResult<Option<String>> {
    let budget = context.condition.budget_chars as usize;
    match context.condition.pipeline {
        Pipeline::FullContext => Ok(Some(
            build_full_context(context.engine, &item.persona_uid, budget).await?,
        )),
        Pipeline::OracleL1 => {
            // 缓存由运行器按条件串行路径注入：拿到别的对象就等于换了索引口径。
            let Some(oracle) = context.oracle.as_deref_mut() else {
                return Err(RamariaError::validation(format!(
                    "oracle-l1 条件缺少索引缓存: {}",
                    item.row.question_id
                )));
            };
            Ok(Some(
                oracle
                    .context(
                        context.engine,
                        context.work_db,
                        &item.persona_uid,
                        &item.row.gold_message_ids,
                        budget,
                    )
                    .await?,
            ))
        }
        _ => Ok(None),
    }
}

/// 执行一道题：新会话 → 可选预置上下文 → 生成（含重试）。
pub(super) async fn execute_item(context: &mut ItemContext<'_>, item: &RunItem) -> ItemRun {
    let engine = context.engine;
    let condition = context.condition;
    let started = Instant::now();
    let session = match engine.create_session(Some(&item.persona_uid)).await {
        Ok(session) => session,
        Err(error) => {
            return ItemRun::failed(item, condition, &error, context.effective, context.line);
        }
    };

    let preset_context = match preset_context(context, item).await {
        Ok(text) => text,
        Err(error) => {
            return ItemRun::failed(item, condition, &error, context.effective, context.line);
        }
    };
    // 预置文本只来自预置管线；空串表示拼过上下文但无内容，按 0 记录。
    let preset_chars = preset_context.as_ref().map(|text| text.chars().count());

    let mut attempt = 1u32;
    loop {
        let request = ChatSendRequest {
            message: item.row.question.clone(),
            persona: Some(item.persona_uid.clone()),
            session_id: Some(session.id),
            conversation_id: None,
            channel: EVAL_CHANNEL.to_string(),
        };
        let options = EvalChatOptions {
            as_of_ms: Some(item.row.ask_time_ms),
            mask: condition.pipeline.mask(),
            touch: false,
            max_chars: Some(condition.budget_chars),
            preset_context: preset_context.clone(),
        };

        match engine.chat_answer_eval(request, options).await {
            Ok(outcome) => {
                return ItemRun {
                    record: RunItemRecord::success(
                        item,
                        condition,
                        outcome,
                        elapsed_ms(started),
                        attempt,
                        context.effective,
                        preset_chars,
                        context.line.clone(),
                    ),
                    success: true,
                    attempts: attempt,
                    error_kind: None,
                };
            }
            Err(error) => {
                if attempt > context.retry_limit {
                    return ItemRun {
                        record: RunItemRecord::failure(
                            item,
                            condition,
                            &error.to_string(),
                            elapsed_ms(started),
                            attempt,
                            context.effective,
                            preset_chars,
                            context.line.clone(),
                        ),
                        success: false,
                        attempts: attempt,
                        error_kind: Some(error_kind(&error)),
                    };
                }
                tracing::debug!(
                    question = %item.row.question_id,
                    condition = %condition.id,
                    attempt,
                    error_kind = error_kind(&error),
                    "评测题目重试"
                );
                attempt += 1;
            }
        }
    }
}

// =========================================================
// 辅助
// =========================================================

/// 错误分类标识（日志与产物排查共用，不写完整错误正文）。
fn error_kind(error: &RamariaError) -> &'static str {
    match error {
        RamariaError::Config { .. } => "config",
        RamariaError::Storage { .. } => "storage",
        RamariaError::Llm { .. } => "llm",
        RamariaError::Serialization { .. } => "serialization",
        RamariaError::Privacy { .. } => "privacy",
        RamariaError::Index { .. } => "index",
        RamariaError::Validation { .. } => "validation",
        RamariaError::Io { .. } => "io",
        RamariaError::Embedding { .. } => "embedding",
        RamariaError::Unsupported { .. } => "unsupported",
    }
}
