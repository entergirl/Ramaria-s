//! crates/ramaria-eval/src/run/runner.rs - Ramaria 评测批量运行器
//!
//! 设计特点:
//! - 条件 → 题目两层循环：条件只跑本题集档位，题目按枚举顺序推进
//! - 失败不中断：单题重试到上限后写失败行继续；条件级失败写告警并把未完成题记为失败
//! - 断点续跑：已完成题跳过，失败题按末行尝试次数判断是否超限
//! - 分层抽样只过滤题集：报告落 sample.json，续跑复用同一批题号
//! - 状态按批落盘、逐题产物逐行追加，中断后两者都能就地恢复
//! - 云端答题模型可用进程内密钥覆盖（评测专用密钥），只在内存生效，不落库
//! - 日志只记题目标识 / 条件 / 耗时 / 错误分类，不记问题与回复正文与密钥内容

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::Engine;
use sha2::{Digest, Sha256};

use crate::baseline::MANIFEST_FILE;
use crate::measure::elapsed_ms;

use super::item::{self, ConditionLine, RunItemRecord};
use super::matrix::{Condition, Pipeline, RunMatrix};
use super::oracle_context::OracleCache;
use super::pipeline::{ConditionRun, ItemSink, run_condition};
use super::plan::{BaselineTier, RunItem, plan_items};
use super::state::{self, RunState, RunStatus, RunTierRef};
use super::workspace::{ITEMS_FILE, RunWorkspace};

#[cfg(test)]
mod tests;

/// 矩阵定义相对路径。
const MATRIX_RELATIVE: &str = ".eval/matrix.toml";

/// 基线产物根相对路径。
const BASELINES_RELATIVE: &str = ".eval/baselines";

/// 运行根相对路径。
const RUNS_RELATIVE: &str = ".eval/runs";

/// 默认运行标识前缀。
const RUN_ID_PREFIX: &str = "run-";

// =========================================================
// 报告
// =========================================================

/// 一次批量运行的结果。
///
/// 字段约定:
/// - `run_id`: 运行根目录名；
/// - `root`: 运行根目录（`run.json` 与 `items.jsonl` 所在）；
/// - `total`: 计划条目数（题 × 匹配条件）；
/// - `completed` / `failed`: 定案计数，两者互斥；
/// - `elapsed_ms`: 从进入命令到收尾的耗时；
/// - `warnings`: 条件级降级与跳过的提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub run_id: String,
    pub root: PathBuf,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
}

impl RunReport {
    /// 渲染为人读摘要（路径 + 计数 + 告警）。
    pub fn render(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "运行 {} 完成（{} ms）:",
            self.run_id, self.elapsed_ms
        );
        let _ = writeln!(output, "  路径: {}", self.root.display());
        let _ = writeln!(
            output,
            "  条目: 完成 {} / 失败 {} / 计划 {}",
            self.completed, self.failed, self.total
        );
        if !self.warnings.is_empty() {
            let _ = writeln!(output, "  告警 {} 条:", self.warnings.len());
            for warning in &self.warnings {
                let _ = writeln!(output, "    - {warning}");
            }
        }
        output
    }
}

// =========================================================
// 命令入口
// =========================================================

/// 按矩阵批量运行评测。
///
/// 参数:
/// - `corpus`: 语料名（缺省取矩阵的 `run.corpus`）；
/// - `matrix_path`: 矩阵文件（缺省 `cwd/.eval/matrix.toml`）；
/// - `output`: 运行根（缺省 `cwd/.eval/runs/{run_id}`，已存在则续跑）。
///
/// 返回:
/// - 成功时返回运行摘要；
/// - 矩阵缺失 / 基线档位缺失 / 状态不一致 / 写入失败返回结构化错误。
pub async fn run(
    corpus: Option<String>,
    matrix_path: Option<PathBuf>,
    output: Option<PathBuf>,
) -> RamariaResult<RunReport> {
    let cwd = current_dir()?;

    // ---- 矩阵 ----
    let matrix_path = matrix_path.unwrap_or_else(|| cwd.join(MATRIX_RELATIVE));
    if !matrix_path.is_file() {
        return Err(RamariaError::validation(matrix_missing(&matrix_path)));
    }
    let matrix = RunMatrix::load(&matrix_path)?;
    let tiers = load_tiers(&cwd, &matrix, corpus.as_deref())?;
    let root = run_root(&cwd, output);
    execute(matrix, root, tiers).await
}

/// 读取矩阵引用的全部基线档位。
///
/// 参数:
/// - `cwd`: 当前工作目录（基线根相对它解析）；
/// - `matrix`: 已校验的矩阵；
/// - `corpus`: 语料名覆盖值，`None` 取矩阵声明值。
///
/// 返回:
/// - 按 `run.tiers` 顺序加载的档位；
/// - 未知语料或档位产物不完整返回结构化错误。
pub(crate) fn load_tiers(
    cwd: &Path,
    matrix: &RunMatrix,
    corpus: Option<&str>,
) -> RamariaResult<Vec<BaselineTier>> {
    let corpus_name = corpus.unwrap_or(matrix.run.corpus.as_str());
    let spec = crate::corpus::find(corpus_name).ok_or_else(|| {
        RamariaError::validation(format!(
            "未知语料: {corpus_name}（已登记: {}）",
            crate::corpus::lifebench().name
        ))
    })?;
    let baseline_root = cwd.join(BASELINES_RELATIVE).join(format!(
        "{}-{}-{}",
        spec.name, spec.version, matrix.run.language
    ));
    let mut tiers = Vec::new();
    for tier_id in &matrix.run.tiers {
        tiers.push(BaselineTier::load(&baseline_root.join(tier_id))?);
    }
    Ok(tiers)
}

/// 按矩阵与档位执行条件循环。
///
/// 参数:
/// - `matrix`: 本次冻结的矩阵（含管线与预算轴）；
/// - `root`: 运行根目录（已存在则续跑）；
/// - `tiers`: 已加载的档位，顺序即题目顺序。
///
/// 返回:
/// - 成功时返回运行摘要；
/// - 状态不一致 / 写入失败返回结构化错误；条件级失败写告警并继续。
///
/// 说明:
/// - 只驱动生成与落盘，路径解析留给调用方，使 oracle 子命令可复用同一循环；
/// - 矩阵声明抽样预算时先过滤题集，抽样报告在状态校验通过后落盘。
pub(crate) async fn execute(
    matrix: RunMatrix,
    root: PathBuf,
    tiers: Vec<BaselineTier>,
) -> RamariaResult<RunReport> {
    // ---- 运行根与抽样 ----
    let workspace = RunWorkspace::new(&root)?;
    // 档位引用描述基线产物本身，题数取抽样前的全量。
    let tier_refs = tier_refs(&tiers)?;
    let mut tiers = tiers;
    let sampled = super::sample::apply(&matrix, &root, &mut tiers)?;

    // ---- 题集与条件 ----
    let items = plan_items(&tiers, matrix.run.question_limit);
    let conditions = matrix.conditions();
    let condition_totals = condition_totals(&conditions, &items);
    // 评测专用密钥只读一次：为空表示沿用系统 keychain 口径，行为与默认一致。
    let eval_key = crate::eval_api_key();
    tracing::info!(has_eval_key = eval_key.is_some(), "评测答题模型密钥来源");

    // ---- 运行状态 ----
    let mut state = RunState::load_or_init(&root, &matrix, &tier_refs, &condition_totals)?;
    if let Some(report) = &sampled.report {
        super::sample::write(&root, report)?;
    }
    for line in &sampled.lines {
        eprintln!("{line}");
    }

    let items_path = root.join(ITEMS_FILE);
    let keys = state::scan_items(&items_path)?;
    state.recount(&records_of(&keys));
    let mut done: HashSet<(String, String)> = keys
        .iter()
        .filter(|key| key.success)
        .map(|key| (key.condition_id.clone(), key.question_id.clone()))
        .collect();
    let mut attempts = state::scan_attempts(&items_path)?;

    // ---- 条件循环 ----
    let started = Instant::now();
    let mut oracle = OracleCache::default();
    for condition in &conditions {
        let Some(tier) = tiers
            .iter()
            .find(|tier| tier.manifest.tier.id == condition.tier)
        else {
            state.warnings.push(format!(
                "条件 {} 的档位 {} 不在本次题集内，已跳过",
                condition.id, condition.tier
            ));
            continue;
        };
        let tier_items: Vec<&RunItem> = items
            .iter()
            .filter(|item| item.tier == condition.tier)
            .collect();
        if tier_items.is_empty() {
            // 题集被上限截断后可能没有该档位的题，不必装配引擎与重建索引。
            tracing::debug!(condition = %condition.id, "评测条件无题目，跳过");
            continue;
        }

        let (engine, backend) = match workspace
            .open_condition_engine(condition, matrix.run.strict_isolation, &tier.product())
            .await
        {
            Ok(pair) => pair,
            Err(error) => {
                let reason = format!("条件装配失败: {error}");
                tracing::warn!(condition = %condition.id, error = %error, "评测条件装配失败");
                state.warnings.push(format!("{}：{reason}", condition.id));
                let line = ConditionLine::from_manifest(
                    &tier.manifest,
                    &matrix.run.language,
                    "unavailable",
                );
                fail_condition(
                    &mut state,
                    &items_path,
                    condition,
                    &tier_items,
                    &line,
                    &done,
                    "unavailable",
                    &reason,
                )?;
                state.elapsed_ms = elapsed_ms(started);
                state.save(&root)?;
                continue;
            }
        };
        if let Some(key) = eval_key.as_deref() {
            apply_eval_key(&engine, &condition.id, key);
        }
        tracing::info!(
            condition = %condition.id,
            backend = %backend.model_text(),
            "评测条件开始"
        );

        // 预置上下文条件不经过召回：不重建索引，省去一次全库构建，也不让嵌入缺失拖垮条件。
        let index_status = if condition.pipeline == Pipeline::OracleL1 {
            engine.index_status().status.as_str().to_string()
        } else {
            match engine.rebuild_index().await {
                Ok(_) => {
                    let view = engine.index_status();
                    let status = view.status.as_str().to_string();
                    if status != "ready" && view.loaded != view.total {
                        state.warnings.push(format!(
                            "条件 {} 索引未就绪: {}（{}/{}）",
                            condition.id, status, view.loaded, view.total
                        ));
                    }
                    status
                }
                Err(error) => {
                    let reason = format!("索引重建失败: {error}");
                    tracing::warn!(condition = %condition.id, error = %error, "评测索引重建失败");
                    state.warnings.push(format!("{}：{reason}", condition.id));
                    let line = ConditionLine::from_manifest(
                        &tier.manifest,
                        &matrix.run.language,
                        "unavailable",
                    );
                    fail_condition(
                        &mut state,
                        &items_path,
                        condition,
                        &tier_items,
                        &line,
                        &done,
                        backend.model_text(),
                        &reason,
                    )?;
                    close_engine(engine).await;
                    state.elapsed_ms = elapsed_ms(started);
                    state.save(&root)?;
                    continue;
                }
            }
        };
        let line =
            ConditionLine::from_manifest(&tier.manifest, &matrix.run.language, &index_status);
        let effective_model = backend.model_text().to_string();

        let work_db = workspace.work_db(condition, matrix.run.strict_isolation);
        // 待跑题目按原顺序编号：并发只改变执行时机，写出仍按这个序号收口。
        let pending: Vec<&RunItem> = tier_items
            .iter()
            .copied()
            .filter(|item| {
                let key = (condition.id.clone(), item.row.question_id.clone());
                !should_skip(&done, &attempts, &key, matrix.run.retry_limit)
            })
            .collect();
        let jobs: Vec<(usize, &RunItem)> = pending.into_iter().enumerate().collect();
        {
            let run = ConditionRun {
                engine: &engine,
                condition,
                line: &line,
                effective: &effective_model,
                retry_limit: matrix.run.retry_limit,
                work_db: &work_db,
            };
            let mut sink = ItemSink::new(
                &items_path,
                &root,
                &condition.id,
                &mut done,
                &mut attempts,
                &mut state,
                started,
            );
            run_condition(&run, jobs, &mut oracle, &mut sink).await?;
        }
        close_engine(engine).await;
        state.elapsed_ms = elapsed_ms(started);
        state.save(&root)?;
    }

    // ---- 收尾 ----
    state.status = RunStatus::Completed;
    state.recount(&records_of(&state::scan_items(&items_path)?));
    state.elapsed_ms = elapsed_ms(started);
    state.save(&root)?;

    let total = state.conditions.iter().map(|entry| entry.total).sum();
    let completed = state.conditions.iter().map(|entry| entry.completed).sum();
    let failed = state.conditions.iter().map(|entry| entry.failed).sum();
    Ok(RunReport {
        run_id: state.run_id.clone(),
        root,
        total,
        completed,
        failed,
        elapsed_ms: state.elapsed_ms,
        warnings: state.warnings.clone(),
    })
}

// =========================================================
// 断点判定
// =========================================================

/// 判断一题是否应跳过。
///
/// 参数:
/// - `done`: 已成功的 `(条件, 题目)` 集合；
/// - `attempts_by_key`: 每题最后一次尝试次数；
/// - `item_key`: 待判定条目；
/// - `retry_limit`: 允许的重试次数。
///
/// 返回:
/// - 已完成为 `true`；
/// - 失败且末次尝试次数大于上限为 `true`（不再重跑）；
/// - 未跑、失败未超限、无尝试记录为 `false`。
pub fn should_skip(
    done: &HashSet<(String, String)>,
    attempts_by_key: &HashMap<(String, String), u32>,
    item_key: &(String, String),
    retry_limit: u32,
) -> bool {
    if done.contains(item_key) {
        return true;
    }
    attempts_by_key
        .get(item_key)
        .is_some_and(|attempts| *attempts > retry_limit)
}

/// 用评测专用密钥重建答题 provider 并热替换（不落库、不写配置文件）。
///
/// 参数:
/// - `engine`: 已按条件装配好的引擎；
/// - `condition_id`: 条件标识（只用于日志）；
/// - `key`: 进程内密钥明文，不写日志。
///
/// 说明:
/// - 只对云端模型生效：本地 provider 不需要密钥，传了也不使用；
/// - 复用引擎当前后端配置（provider / base_url / model），只换密钥来源；
/// - 构造失败只记 warn 并沿用原 provider，不中断条件。
fn apply_eval_key(engine: &Engine, condition_id: &str, key: &str) {
    let current = engine.llm();
    let config = current.config().clone();
    if !config.provider.is_online() {
        tracing::debug!(condition = %condition_id, "本地答题模型，评测密钥不适用");
        return;
    }
    match ramaria_service::eval_llm_with_key(&config, Some(key)) {
        Ok(provider) => {
            engine.update_llm(provider);
            tracing::info!(
                condition = %condition_id,
                model = %config.capability.model_id,
                "评测专用密钥已接管答题模型"
            );
        }
        Err(error) => tracing::warn!(
            condition = %condition_id,
            error = %error,
            "评测专用密钥构造答题 provider 失败，沿用原 provider"
        ),
    }
}

/// 条件级失败：为该条件未完成的题目各写一条失败行。
#[allow(clippy::too_many_arguments)]
fn fail_condition(
    state: &mut RunState,
    items_path: &Path,
    condition: &Condition,
    tier_items: &[&RunItem],
    line: &ConditionLine,
    done: &HashSet<(String, String)>,
    effective: &str,
    reason: &str,
) -> RamariaResult<()> {
    let mut written = 0usize;
    for item in tier_items {
        let key = (condition.id.clone(), item.row.question_id.clone());
        if done.contains(&key) {
            continue;
        }
        let record =
            RunItemRecord::failure(item, condition, reason, 0, 0, effective, None, line.clone());
        item::append(items_path, &record)?;
        written += 1;
    }
    if let Some(entry) = state.condition_mut(&condition.id) {
        entry.failed = written;
    }
    Ok(())
}

// =========================================================
// 辅助
// =========================================================

/// 读取当前工作目录。
fn current_dir() -> RamariaResult<PathBuf> {
    std::env::current_dir().map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))
}

/// 矩阵缺失时的提示文案（带期望路径与字段示例）。
fn matrix_missing(path: &Path) -> String {
    format!(
        "矩阵定义不存在: {}（请创建该文件；必需字段: [run] corpus / language / tiers / pipelines / budgets，[[answer_models]] kind / base_url / model）",
        path.display()
    )
}

/// 运行根：显式路径优先，否则按时间戳生成。
fn run_root(cwd: &Path, output: Option<PathBuf>) -> PathBuf {
    match output {
        Some(path) => path,
        None => {
            let run_id = format!("{RUN_ID_PREFIX}{}", Utc::now().format("%Y%m%d-%H%M%S"));
            cwd.join(RUNS_RELATIVE).join(run_id)
        }
    }
}

/// 档位引用：清单路径、内容摘要与题目数。
fn tier_refs(tiers: &[BaselineTier]) -> RamariaResult<Vec<RunTierRef>> {
    let mut refs = Vec::new();
    for tier in tiers {
        let manifest_path = tier.dir.join(MANIFEST_FILE);
        let bytes = std::fs::read(&manifest_path).map_err(|error| {
            RamariaError::io(
                format!("读取基线清单失败: {}", manifest_path.display()),
                Some(error),
            )
        })?;
        refs.push(RunTierRef {
            id: tier.manifest.tier.id.clone(),
            dir: tier.dir.to_string_lossy().into_owned(),
            manifest_sha256: format!("{:x}", Sha256::digest(&bytes)),
            rows: tier.rows.len(),
        });
    }
    Ok(refs)
}

/// 每个条件的计划题目数（按题目档位统计）。
fn condition_totals(conditions: &[Condition], items: &[RunItem]) -> Vec<(String, usize)> {
    let mut totals: HashMap<&str, usize> = HashMap::new();
    for item in items {
        *totals.entry(item.tier.as_str()).or_default() += 1;
    }
    conditions
        .iter()
        .map(|condition| {
            let total = totals.get(condition.tier.as_str()).copied().unwrap_or(0);
            (condition.id.clone(), total)
        })
        .collect()
}

/// 逐题扫描结果转计数输入。
fn records_of(keys: &[state::ItemKey]) -> Vec<(String, String, bool)> {
    keys.iter()
        .map(|key| {
            (
                key.condition_id.clone(),
                key.question_id.clone(),
                key.success,
            )
        })
        .collect()
}

/// 关闭引擎连接池（Windows 上及时释放文件句柄）。
async fn close_engine(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
}
