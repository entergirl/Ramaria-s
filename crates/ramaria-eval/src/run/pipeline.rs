//! crates/ramaria-eval/src/run/pipeline.rs - Ramaria 评测条件内逐题执行
//!
//! 设计特点:
//! - 条件内按题并发：默认 4 路，环境变量可改，oracle-l1 恒串行
//! - 顺序写出：乱序完成先入缓冲，序号连续才落盘，产物顺序与题集顺序一致
//! - 写出点统一：追加产物、断点与条件计数、日志、批量存盘在同一处完成
//! - 并发只改变执行时机：单题上下文每题新建，条件级上下文只读共享
//! - 并发度非法值回退默认；1 路时退化为逐题串行

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use ramaria_core::error::RamariaResult;
use ramaria_service::Engine;

use crate::measure::elapsed_ms;

use super::exec::{ItemContext, ItemRun, execute_item};
use super::item::{self, ConditionLine};
use super::matrix::{Condition, Pipeline};
use super::oracle_context::OracleCache;
use super::plan::RunItem;
use super::state::RunState;

#[cfg(test)]
mod tests;

/// 默认并发度（同一条件的题目之间）。
const DEFAULT_CONCURRENCY: usize = 4;

/// 并发度上限。
const MAX_CONCURRENCY: usize = 16;

/// 并发度环境变量。
const CONCURRENCY_ENV: &str = "RAMARIA_EVAL_RUN_CONCURRENCY";

/// 状态落盘间隔（按题数）。
const SAVE_EVERY: usize = 10;

// =========================================================
// 并发度
// =========================================================

/// 解析条件内并发度。
///
/// 参数:
/// - `raw`: 环境变量原文；`None` 表示未设置。
///
/// 返回:
/// - 取值在 `1..=MAX_CONCURRENCY` 内时按原值；
/// - 未设置、空白、非数字、0 或超过上限时取 `DEFAULT_CONCURRENCY`。
pub(super) fn parse_concurrency(raw: Option<&str>) -> usize {
    raw.map(str::trim)
        .and_then(|text| text.parse::<usize>().ok())
        .filter(|value| (1..=MAX_CONCURRENCY).contains(value))
        .unwrap_or(DEFAULT_CONCURRENCY)
}

/// 从环境变量读取条件内并发度（未设置或非法时取默认值）。
pub(super) fn concurrency_from_env() -> usize {
    parse_concurrency(std::env::var(CONCURRENCY_ENV).ok().as_deref())
}

/// 按管线取条件内并发度。
///
/// 参数:
/// - `pipeline`: 该条件的注入管线。
///
/// 返回:
/// - oracle-l1 恒为 1：索引缓存按题目路径独占，首建成本也不随并发下降；
/// - 其余管线取环境变量配置值。
pub(super) fn concurrency_for(pipeline: Pipeline) -> usize {
    match pipeline {
        Pipeline::OracleL1 => 1,
        _ => concurrency_from_env(),
    }
}

// =========================================================
// 有序写出
// =========================================================

/// 按序号收口的写出缓冲。
///
/// 说明:
/// - 结果可以乱序到达，只有序号连续的队首才会被取出；
/// - 缺号时后续结果留在缓冲里，直到缺的那条到达。
#[derive(Debug)]
pub(super) struct OrderedSink<T> {
    next: usize,
    buffer: BTreeMap<usize, T>,
}

impl<T> OrderedSink<T> {
    /// 建立从序号 0 开始收口的空缓冲。
    pub(super) fn new() -> Self {
        Self {
            next: 0,
            buffer: BTreeMap::new(),
        }
    }

    /// 收下一个已完成结果（序号重复时后者覆盖前者）。
    pub(super) fn insert(&mut self, seq: usize, value: T) {
        self.buffer.insert(seq, value);
    }

    /// 取出当前连续可达的结果。
    ///
    /// 返回:
    /// - 从内部游标起连续存在的结果，按序号升序；游标推进到下一个缺号；
    /// - 队首缺号时返回空列表。
    pub(super) fn drain_ready(&mut self) -> Vec<T> {
        let mut ready = Vec::new();
        while let Some(value) = self.buffer.remove(&self.next) {
            ready.push(value);
            self.next += 1;
        }
        ready
    }
}

impl<T> Default for OrderedSink<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// 按并发度执行一批任务，并按序号顺序写出结果。
///
/// 参数:
/// - `jobs`: `(序号, 输入)`，序号须从 0 起连续；
/// - `concurrency`: 同时在飞的任务上限；
/// - `execute`: 单任务执行器（每个任务调用一次）；
/// - `write`: 写出回调，只在序号连续可达时逐条调用。
///
/// 返回:
/// - 全部结果写出后为 `Ok(())`；
/// - 写出回调的错误原样返回（在飞任务随之丢弃）。
async fn ordered_run<T, R, F, Fut, W>(
    jobs: Vec<(usize, T)>,
    concurrency: usize,
    execute: F,
    mut write: W,
) -> RamariaResult<()>
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = R>,
    W: FnMut(R) -> RamariaResult<()>,
{
    let mut sink = OrderedSink::new();
    let mut results = stream::iter(jobs)
        .map(|(seq, job)| {
            let future = execute(job);
            async move { (seq, future.await) }
        })
        .buffer_unordered(concurrency.max(1));
    while let Some((seq, result)) = results.next().await {
        sink.insert(seq, result);
        for result in sink.drain_ready() {
            write(result)?;
        }
    }
    Ok(())
}

// =========================================================
// 条件执行
// =========================================================

/// 一个条件的逐题执行环境（条件级不可变借用）。
///
/// 字段约定:
/// - `engine` / `condition` / `line` / `effective`: 与逐题产物一致的条件口径；
/// - `retry_limit`: 该条件的单题重试上限；
/// - `work_db`: 该条件工作库路径（oracle 索引缓存键）。
pub(super) struct ConditionRun<'a> {
    pub(super) engine: &'a Engine,
    pub(super) condition: &'a Condition,
    pub(super) line: &'a ConditionLine,
    pub(super) effective: &'a str,
    pub(super) retry_limit: u32,
    pub(super) work_db: &'a Path,
}

impl ConditionRun<'_> {
    /// 按题目与缓存装配一份单题执行环境。
    fn context<'a>(&'a self, oracle: Option<&'a mut OracleCache>) -> ItemContext<'a> {
        ItemContext::new(
            self.engine,
            self.condition,
            self.line,
            self.effective,
            self.retry_limit,
            self.work_db,
            oracle,
        )
    }
}

/// 逐题执行上下文：产物写入与计数更新都在写出点完成。
///
/// 字段约定:
/// - `done` / `attempts`: 断点集合，写出点按成功与否更新；
/// - `state`: 运行状态（条件计数与批量落盘）；
/// - `started`: 运行起点，批量落盘时回填累计耗时；
/// - `processed`: 本条件已写出题数，控制落盘节奏。
pub(super) struct ItemSink<'a> {
    items_path: &'a Path,
    root: &'a Path,
    condition_id: &'a str,
    done: &'a mut HashSet<(String, String)>,
    attempts: &'a mut HashMap<(String, String), u32>,
    state: &'a mut RunState,
    started: Instant,
    processed: usize,
}

impl<'a> ItemSink<'a> {
    /// 装配一个条件的写出上下文。
    pub(super) fn new(
        items_path: &'a Path,
        root: &'a Path,
        condition_id: &'a str,
        done: &'a mut HashSet<(String, String)>,
        attempts: &'a mut HashMap<(String, String), u32>,
        state: &'a mut RunState,
        started: Instant,
    ) -> Self {
        Self {
            items_path,
            root,
            condition_id,
            done,
            attempts,
            state,
            started,
            processed: 0,
        }
    }

    /// 写出一道题的结果：追加产物，再更新断点、计数、日志与批量存盘。
    ///
    /// 参数:
    /// - `run`: 单题执行结果。
    ///
    /// 返回:
    /// - 写出成功为 `Ok(())`；
    /// - 产物追加或状态落盘失败返回对应错误。
    pub(super) fn write(&mut self, run: ItemRun) -> RamariaResult<()> {
        item::append(self.items_path, &run.record)?;

        let key = (
            self.condition_id.to_string(),
            run.record.question_id.clone(),
        );
        if run.success {
            self.done.insert(key.clone());
            self.attempts.remove(&key);
            if let Some(entry) = self.state.condition_mut(self.condition_id) {
                entry.completed += 1;
            }
        } else {
            self.attempts.insert(key, run.attempts);
            if let Some(entry) = self.state.condition_mut(self.condition_id) {
                entry.failed += 1;
            }
        }
        tracing::info!(
            question = %run.record.question_id,
            condition = %self.condition_id,
            elapsed_ms = run.record.elapsed_ms,
            success = run.success,
            error_kind = run.error_kind.unwrap_or("none"),
            "评测题目完成"
        );

        self.processed += 1;
        if self.processed.is_multiple_of(SAVE_EVERY) {
            self.state.elapsed_ms = elapsed_ms(self.started);
            self.state.save(self.root)?;
        }
        Ok(())
    }
}

/// 执行一个条件内待跑的题目。
///
/// 参数:
/// - `run`: 条件级执行环境；
/// - `jobs`: `(序号, 题目)`，序号即题目在本题集内的顺序；
/// - `oracle`: 跨条件复用的压缩层索引缓存（仅 oracle-l1 使用）；
/// - `sink`: 逐题写出上下文。
///
/// 返回:
/// - 全部题目写出后为 `Ok(())`；
/// - 产物或状态写入失败返回对应错误。
///
/// 说明:
/// - oracle-l1 走串行：索引缓存首次构建成本高，且缓冲按题目路径独占；
/// - 其余管线按并发度执行，结果仍按题目原顺序写出。
pub(super) async fn run_condition(
    run: &ConditionRun<'_>,
    jobs: Vec<(usize, &RunItem)>,
    oracle: &mut OracleCache,
    sink: &mut ItemSink<'_>,
) -> RamariaResult<()> {
    let concurrency = concurrency_for(run.condition.pipeline);
    if concurrency <= 1 {
        let mut context = match run.condition.pipeline {
            Pipeline::OracleL1 => run.context(Some(oracle)),
            _ => run.context(None),
        };
        for (_, item) in &jobs {
            let outcome = execute_item(&mut context, item).await;
            sink.write(outcome)?;
        }
        return Ok(());
    }

    ordered_run(
        jobs,
        concurrency,
        |item| async move {
            let mut context = run.context(None);
            execute_item(&mut context, item).await
        },
        |outcome| sink.write(outcome),
    )
    .await
}
