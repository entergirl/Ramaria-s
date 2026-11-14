//! crates/ramaria-eval/src/score/mod.rs - Ramaria 评测判分入口
//!
//! 设计特点:
//! - 只读判分：读运行状态与逐题产物，产物写运行根下 `score/`，不碰用户库
//! - 工作库按路径分组、每组只开一次引擎：默认模式全部条件共用一个库，严格模式每条件一个
//! - 逐题产物按复合键取末行，输出保持末次出现顺序，同一输入可重复且逐字节一致
//! - 检索列需要单元证据：库缺失时整列留 null 并记缺项，其余列照常产出
//! - 判分按矩阵定案：优先 `[judge]`，缺省回退首个答题模型；不可用只记缺项不中断
//! - 人格三维取该条件工作库的嵌入 provider：缺失时事实维降级为纯关键词
//! - 判分产物只放判分结果，用量单独落 `usage.json`（遥测）；不写时间戳与机器信息，可直接 diff

mod artifact;
mod artifact_index;
mod condition;
mod credibility;
mod judge;
mod persona;
mod prepare;
mod retrieval;
mod rubric;
mod source;

#[cfg(test)]
mod tests;

pub(crate) use artifact::write_atomic;
pub use artifact::{
    ArtifactRetrieval, CategoryScore, ConditionCredibility, ConditionScore, ConditionUsage,
    ConditionUsageEntry, ItemArtifactRetrieval, ItemCredibility, ItemRetrieval, JudgeRate,
    JudgeSource, JudgeStatus, MessageRetrieval, MissingEntry, PersonaSummary, RULESET_VERSION,
    RetrievalScore, RubricSummary, SCORE_ARTIFACT_KIND, SCORE_DIR, SCORE_FILE,
    SCORE_SCHEMA_VERSION, SCORED_FILE, ScoreReport, ScoredItem, USAGE_ARTIFACT_KIND, USAGE_FILE,
    USAGE_SCHEMA_VERSION, UsageReport, UsageSummary,
};
pub use artifact_index::ArtifactIndex;
pub use judge::TemplateHash;
pub use persona::PersonaScore;
pub use rubric::{RubricPointScore, RubricScore};

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::Engine;
use sha2::{Digest, Sha256};

use crate::run::{ITEMS_FILE, RUN_FILE, RunItemRecord, RunState, WORK_DIR};

use crate::run::Pipeline;
use artifact::{
    MISSING_ARTIFACT, MISSING_CD, MISSING_ITEMS, MISSING_JUDGE, MISSING_PRESET_RECALL,
    MISSING_RUBRIC, MISSING_WORK_DB,
};
use judge::Judge;

/// 运行根默认父目录（相对当前工作目录）。
const RUNS_RELATIVE: &str = ".eval/runs";

// =========================================================
// 选项与入口
// =========================================================

/// 判分选项。
///
/// 字段约定:
/// - `no_judge`: 跳过 judge 判分，只产出确定性指标；判分列整列留 `null` 并记缺项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScoreOptions {
    pub no_judge: bool,
}

impl ScoreOptions {
    /// 是否要求调用判分模型。
    pub fn wants_judge(&self) -> bool {
        !self.no_judge
    }
}

/// 运行 id → 运行根目录。
///
/// 参数:
/// - `cwd`: 当前工作目录；
/// - `run_id`: 运行 id，或直接指向运行根的路径。
///
/// 返回:
/// - 入参已是目录时按路径解析（支持任意绝对 / 相对路径）；
/// - 否则按 `cwd/.eval/runs/{run_id}` 解析。
pub fn resolve_run_root(cwd: &Path, run_id: &str) -> PathBuf {
    let direct = Path::new(run_id);
    if direct.is_dir() {
        return direct.to_path_buf();
    }
    cwd.join(RUNS_RELATIVE).join(run_id)
}

/// 对已有运行结果判分。
///
/// 参数:
/// - `run_root`: 运行根目录（`run.json` 与 `items.jsonl` 所在）；
/// - `options`: 判分选项。
///
/// 返回:
/// - 成功时返回判分汇总与运行用量，并把 `score.json`、`scored.jsonl` 与 `usage.json`
///   写到 `run_root/score/`；
/// - 运行状态缺失 / 不可解析、产物写出失败返回结构化错误；
/// - 工作库缺失只降级检索列并记缺项，判分模型不可用只降级判分列并记缺项，都不报错。
pub async fn run(
    run_root: &Path,
    options: ScoreOptions,
) -> RamariaResult<(ScoreReport, UsageReport)> {
    let state = read_state(run_root)?;
    let mut missing = Vec::new();
    let judge = if options.wants_judge() {
        prepare::prepare_judge(run_root, &state.matrix, &mut missing)
    } else {
        missing.push(MissingEntry::run_level(
            MISSING_JUDGE,
            "本次以 --no-judge 运行，只产出确定性指标",
        ));
        None
    };
    finish(run_root, state, missing, judge).await
}

/// 判分内核：矩阵定案与判分句柄已就绪。
///
/// 参数:
/// - `run_root`: 运行根目录；
/// - `state`: 已读取的运行状态；
/// - `missing`: 已收集的运行级缺项（本函数继续追加）；
/// - `judge`: 判分句柄；`None` 表示本次不判分。
///
/// 返回:
/// - 判分汇总与会话用量；用量只进 `usage.json`，不进判分汇总。
async fn finish(
    run_root: &Path,
    state: RunState,
    mut missing: Vec<MissingEntry>,
    judge: Option<Judge>,
) -> RamariaResult<(ScoreReport, UsageReport)> {
    let (records, items_sha256) = read_items(&run_root.join(ITEMS_FILE), &mut missing)?;
    if judge.is_none() {
        missing.push(MissingEntry::run_level(
            MISSING_RUBRIC,
            "judge 未启用，分项判分列留空",
        ));
        missing.push(MissingEntry::run_level(
            MISSING_CD,
            "judge 未启用，CD 判定列留空",
        ));
    }
    let entries = last_occurrences(records);
    let mut by_condition = group_by_condition(entries);

    // ---- 条件 × 工作库：同一库只装配一次引擎 ----
    let strict = state.matrix.run.strict_isolation;
    let tier_dirs: BTreeMap<String, PathBuf> = state
        .tiers
        .iter()
        .map(|tier| (tier.id.clone(), PathBuf::from(&tier.dir)))
        .collect();
    let indexes = load_artifact_indexes(&tier_dirs)?;
    let condition_ids: BTreeSet<String> = state
        .conditions
        .iter()
        .map(|condition| condition.id.clone())
        .chain(by_condition.keys().cloned())
        .collect();
    let mut groups: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for id in condition_ids {
        groups
            .entry(work_db_path(run_root, strict, &id))
            .or_default()
            .push(id);
    }

    let mut conditions = Vec::new();
    let mut scored_entries = Vec::new();
    let mut usage_entries: Vec<ConditionUsageEntry> = Vec::new();
    for (db, ids) in groups {
        let engine = open_work_engine(&db).await?;
        let embedder = engine.as_ref().and_then(|engine| engine.embedding());
        for id in ids {
            let entries = by_condition.remove(&id).unwrap_or_default();
            let tier = entries.first().map(|(_, record)| record.tier.clone());
            // 预置上下文条件不经过召回：full-context 由运行器拼全量摘要，oracle-l1 只拼 gold 摘要。
            let preset = entries.first().is_some_and(|(_, record)| {
                record.pipeline == Pipeline::OracleL1.as_str()
                    || record.pipeline == Pipeline::FullContext.as_str()
            });
            let mut resolver = match &engine {
                Some(engine) if !preset => {
                    Some(source::EngineSourceResolver::prefetch(engine, &labels_of(&entries)).await)
                }
                _ => None,
            };
            let handle = resolver
                .as_mut()
                .map(|source| source as &mut dyn retrieval::SourceResolver);
            let index = if preset {
                None
            } else {
                tier.as_deref()
                    .and_then(|tier| indexes.get(tier))
                    .and_then(|entry| entry.as_ref())
            };
            let (mut score, mut items, usage) = condition::score_condition(
                &id,
                entries,
                handle,
                index,
                embedder.as_deref(),
                judge.as_ref(),
            )
            .await?;
            if engine.is_none() {
                score.missing.push(MissingEntry::condition(
                    &id,
                    MISSING_WORK_DB,
                    format!("工作库不存在: {}", db.display()),
                ));
            }
            if preset {
                score.missing.push(MissingEntry::condition(
                    &id,
                    MISSING_PRESET_RECALL,
                    "预置上下文条件无召回轨迹",
                ));
            } else if let (Some(tier), None) = (tier.as_deref(), index) {
                let detail = match tier_dirs.get(tier) {
                    Some(dir) => format!(
                        "artifact 映射不存在: {}",
                        dir.join(crate::baseline::ARTIFACTS_FILE).display()
                    ),
                    None => format!("档位 {tier} 未登记产物目录，无法读取 artifact 映射"),
                };
                score
                    .missing
                    .push(MissingEntry::condition(&id, MISSING_ARTIFACT, detail));
            }
            conditions.push(score);
            scored_entries.append(&mut items);
            usage_entries.push(ConditionUsageEntry::new(&id, usage));
        }
        close_engine(engine).await;
    }

    conditions.sort_by(|left, right| left.condition_id.cmp(&right.condition_id));
    scored_entries.sort_by_key(|entry| entry.order);
    // 用量按条件标识字典序，与 score.json 的条件顺序一一对应。
    usage_entries.sort_by(|left, right| left.condition_id.cmp(&right.condition_id));

    // 运行级用量：各条件答案 / 判分之和（遥测，单独落盘）。
    let mut total = ConditionUsage::default();
    for entry in &usage_entries {
        total.add(ConditionUsage {
            answer: entry.answer,
            judge: entry.judge,
        });
    }

    let report = ScoreReport {
        artifact_kind: SCORE_ARTIFACT_KIND.to_string(),
        schema_version: SCORE_SCHEMA_VERSION,
        run_id: state.run_id,
        items_sha256,
        ruleset_version: RULESET_VERSION.to_string(),
        judge: judge
            .as_ref()
            .map(Judge::status)
            .unwrap_or(JudgeStatus::Disabled),
        conditions,
        missing,
    };
    let usage = UsageReport {
        artifact_kind: USAGE_ARTIFACT_KIND.to_string(),
        schema_version: USAGE_SCHEMA_VERSION,
        run_id: report.run_id.clone(),
        items_sha256: report.items_sha256.clone(),
        conditions: usage_entries,
        total,
    };
    write_products(run_root, &report, &usage, &scored_entries)?;
    Ok((report, usage))
}

// =========================================================
// 读取
// =========================================================

/// 读取运行状态（`run.json`）。
fn read_state(root: &Path) -> RamariaResult<RunState> {
    let path = root.join(RUN_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(
            format!("读取运行状态失败: {}（请确认运行 id）", path.display()),
            Some(error),
        )
    })?;
    serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!("解析运行状态失败: {}: {error}", path.display()))
    })
}

/// 读取逐题产物，返回记录与字节摘要。
///
/// 参数:
/// - `path`: `items.jsonl` 路径；
/// - `missing`: 运行级缺项收集器。
///
/// 返回:
/// - 按文件顺序返回全部行（不去重）；
/// - 文件缺失时返回空列表并记缺项，摘要取空内容摘要；
/// - 非 UTF-8 或坏行返回 `Validation`，文案带行号。
fn read_items(
    path: &Path,
    missing: &mut Vec<MissingEntry>,
) -> RamariaResult<(Vec<RunItemRecord>, String)> {
    if !path.is_file() {
        missing.push(MissingEntry::run_level(
            MISSING_ITEMS,
            format!("逐题产物不存在: {}", path.display()),
        ));
        return Ok((Vec::new(), format!("{:x}", Sha256::digest(b""))));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        RamariaError::io(format!("读取逐题产物失败: {}", path.display()), Some(error))
    })?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        RamariaError::validation(format!("逐题产物不是 UTF-8: {}: {error}", path.display()))
    })?;

    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let record: RunItemRecord = serde_json::from_str(line).map_err(|error| {
            RamariaError::validation(format!(
                "解析逐题产物失败: {} 第 {} 行: {error}",
                path.display(),
                index + 1
            ))
        })?;
        records.push(record);
    }
    Ok((records, digest))
}

// =========================================================
// 分组
// =========================================================

/// 同一复合键只保留最后一行，并按末次出现顺序排列。
fn last_occurrences(records: Vec<RunItemRecord>) -> Vec<(usize, RunItemRecord)> {
    let mut latest: HashMap<(String, String), (usize, RunItemRecord)> = HashMap::new();
    for (index, record) in records.into_iter().enumerate() {
        let key = (record.condition_id.clone(), record.question_id.clone());
        latest.insert(key, (index, record));
    }
    let mut ordered: BTreeMap<usize, RunItemRecord> = BTreeMap::new();
    for (index, record) in latest.into_values() {
        ordered.insert(index, record);
    }
    ordered.into_iter().collect()
}

/// 按条件标识分组，组内保持题目顺序。
fn group_by_condition(
    entries: Vec<(usize, RunItemRecord)>,
) -> BTreeMap<String, Vec<(usize, RunItemRecord)>> {
    let mut grouped: BTreeMap<String, Vec<(usize, RunItemRecord)>> = BTreeMap::new();
    for (index, record) in entries {
        grouped
            .entry(record.condition_id.clone())
            .or_default()
            .push((index, record));
    }
    grouped
}

/// 条件工作库路径（与运行器的工作区口径一致）。
fn work_db_path(root: &Path, strict: bool, condition_id: &str) -> PathBuf {
    let work = root.join(WORK_DIR);
    if strict {
        work.join(condition_id.replace('/', "_"))
            .join(crate::assembly::EVAL_DB_FILE)
    } else {
        work.join(crate::assembly::EVAL_DB_FILE)
    }
}

/// 按档位目录加载 artifact 反查表。
///
/// 参数:
/// - `dirs`: 档位标识 → 档位产物目录（取自运行状态的档位引用）。
///
/// 返回:
/// - 每个档位一项；映射文件缺失记 `None`（该条件 artifact 列留空），文件不自洽返回 `Validation`。
fn load_artifact_indexes(
    dirs: &BTreeMap<String, PathBuf>,
) -> RamariaResult<BTreeMap<String, Option<ArtifactIndex>>> {
    let mut indexes = BTreeMap::new();
    for (tier, dir) in dirs {
        indexes.insert(tier.clone(), ArtifactIndex::load(dir)?);
    }
    Ok(indexes)
}

/// 一题集涉及的全部单元标签（预取用；可重复、可含不可映射前缀）。
fn labels_of(entries: &[(usize, RunItemRecord)]) -> Vec<String> {
    let mut labels = Vec::new();
    for (_, record) in entries {
        let Some(recall) = record.recall.as_ref() else {
            continue;
        };
        labels.extend(retrieval::labels_before(&recall.trace.retrieval));
        labels.extend(retrieval::labels_after(recall));
    }
    labels
}

// =========================================================
// 写盘
// =========================================================

/// 写出 `score.json`、`scored.jsonl` 与 `usage.json`（无时间戳，同输入同字节）。
///
/// 说明: 前两份是判分结果，同输入逐字节一致；`usage.json` 是运行遥测，
/// 缓存命中的再次判分只让它变小，不影响判分结果。
fn write_products(
    run_root: &Path,
    report: &ScoreReport,
    usage: &UsageReport,
    entries: &[condition::ScoredEntry],
) -> RamariaResult<()> {
    let dir = ScoreReport::dir_of(run_root);
    let summary =
        serde_json::to_string_pretty(report).map_err(|error| RamariaError::Serialization {
            context: format!("序列化判分汇总失败: {}", report.run_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;

    let mut body = String::new();
    for entry in entries {
        let line =
            serde_json::to_string(&entry.item).map_err(|error| RamariaError::Serialization {
                context: format!("序列化逐题判分失败: {}", entry.item.question_id),
                source: Some(Box::new(error)),
                trace_id: None,
            })?;
        body.push_str(&line);
        body.push('\n');
    }

    let usage_body =
        serde_json::to_string_pretty(usage).map_err(|error| RamariaError::Serialization {
            context: format!("序列化判分用量失败: {}", usage.run_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;

    artifact::write_atomic(&dir, SCORE_FILE, &summary)?;
    artifact::write_atomic(&dir, SCORED_FILE, &body)?;
    artifact::write_atomic(&dir, USAGE_FILE, &format!("{usage_body}\n"))?;
    Ok(())
}

// =========================================================
// 辅助
// =========================================================

/// 按需装配工作库引擎；库不存在返回 `None`。
async fn open_work_engine(db: &Path) -> RamariaResult<Option<Engine>> {
    if !db.is_file() {
        tracing::warn!(db = %db.display(), "判分缺少工作库，检索列降级为空");
        return Ok(None);
    }
    Engine::open(db).await.map(Some)
}

/// 关闭引擎连接池（Windows 上及时释放文件句柄）。
async fn close_engine(engine: Option<Engine>) {
    if let Some(engine) = engine {
        if let Some(pool) = engine.sqlite_pool() {
            pool.close().await;
        }
        drop(engine);
    }
}
