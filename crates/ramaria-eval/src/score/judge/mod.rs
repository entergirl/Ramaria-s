//! crates/ramaria-eval/src/score/judge/mod.rs - Ramaria 评测判分
//!
//! 设计特点:
//! - 固定三次投票、温度 0：多数判定可复现，有效票少于两票即判分失败，不产分也不写缓存
//! - 语气维单票定案：只调用一次、走同一套缓存键，不参与三次投票
//! - 逐要点取多数：两票命中即算命中，reason 取首个与多数同向的票
//! - 解析失败只重试一次：system 不变，只在用户消息末尾追加 JSON 约束
//! - 缓存按输入摘要命中，用量按调用累加：命中缓存零模型调用、零用量，产物逐字节一致
//! - 投票之间可配间隔：默认不留间隔，环境变量可加；单票入口与末票之后不等待
//! - 同一趟内可按并发度共享句柄：缓存与用量线程安全，四趟之间仍顺序执行

mod cache;
mod client;
pub(super) mod prompt;
mod verdict;

#[cfg(test)]
mod tests;

pub use cache::{CACHE_FILE, JudgeCache, JudgeCacheEntry};
pub use client::JudgeClient;
pub use prompt::TemplateHash;
pub use verdict::{JudgeValue, PointVote, Verdict};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{LlmProvider, LlmUsage};

use crate::run::AnswerModelKind;

use super::artifact::{JudgeSource, JudgeStatus};

/// 每题的投票次数（语气维走单票入口，固定 1 票）。
pub const JUDGE_VOTES: usize = 3;

/// 判分采样温度。
pub const JUDGE_TEMPERATURE: f64 = 0.0;

/// 单票最大输出 tokens。
pub const JUDGE_MAX_TOKENS: u32 = 512;

/// 投票间隔的环境变量名。
pub const VOTE_DELAY_ENV: &str = "RAMARIA_EVAL_JUDGE_VOTE_DELAY_MS";

/// 默认投票间隔（毫秒）：环境变量未设置或非法时使用。
pub const DEFAULT_VOTE_DELAY_MS: u64 = 0;

/// 判分并发度的环境变量名。
pub const CONCURRENCY_ENV: &str = "RAMARIA_EVAL_JUDGE_CONCURRENCY";

/// 默认并发度：同一趟内部同时判分的题目数。
pub const DEFAULT_JUDGE_CONCURRENCY: usize = 4;

/// 并发度上限：超过上限按默认值处理。
pub const MAX_JUDGE_CONCURRENCY: usize = 16;

// =========================================================
// 投票间隔
// =========================================================

/// 解析投票间隔。
///
/// 参数:
/// - `raw`: 环境变量原文；`None` 表示未设置。
///
/// 返回:
/// - 未设置、空白或非法（负数、小数、非数字）取 `DEFAULT_VOTE_DELAY_MS`；
/// - `0` 返回零间隔，正整数返回对应毫秒数。
pub fn parse_vote_delay(raw: Option<&str>) -> Duration {
    let millis = raw
        .map(str::trim)
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(DEFAULT_VOTE_DELAY_MS);
    Duration::from_millis(millis)
}

/// 从环境变量读取投票间隔（未设置或非法时取默认值）。
pub fn vote_delay_from_env() -> Duration {
    parse_vote_delay(std::env::var(VOTE_DELAY_ENV).ok().as_deref())
}

// =========================================================
// 并发度
// =========================================================

/// 解析判分并发度。
///
/// 参数:
/// - `raw`: 环境变量原文；`None` 表示未设置。
///
/// 返回:
/// - 取值在 `1..=MAX_JUDGE_CONCURRENCY` 内时按原值；
/// - 未设置、空白、非数字、0 或超过上限时取 `DEFAULT_JUDGE_CONCURRENCY`。
pub fn parse_concurrency(raw: Option<&str>) -> usize {
    raw.map(str::trim)
        .and_then(|text| text.parse::<usize>().ok())
        .filter(|value| (1..=MAX_JUDGE_CONCURRENCY).contains(value))
        .unwrap_or(DEFAULT_JUDGE_CONCURRENCY)
}

/// 从环境变量读取并发度（未设置或非法时取默认值）。
pub fn concurrency_from_env() -> usize {
    parse_concurrency(std::env::var(CONCURRENCY_ENV).ok().as_deref())
}

/// 一次判分要解决的问题。
///
/// 状态:
/// - `RubricPoints` / `Unanswerable` / `Conflict` 走三次投票（`ask`）；
/// - `Tone` 只走单票入口（`ask_once`），三次投票入口对它返回空判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JudgeTask {
    /// 分项要点逐条判命中
    RubricPoints,
    /// 不可回答题是否明确拒答
    Unanswerable,
    /// 冲突题是否接受错误前提
    Conflict,
    /// 人格表达的语气维（1~5 分，单票）
    Tone,
}

impl JudgeTask {
    /// 稳定任务标识（缓存键与产物共用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RubricPoints => "rubric_points",
            Self::Unanswerable => "unanswerable",
            Self::Conflict => "conflict",
            Self::Tone => "tone",
        }
    }

    /// 该任务的系统提示文本。
    pub fn system(&self) -> &'static str {
        match self {
            Self::RubricPoints => prompt::RUBRIC_SYSTEM,
            Self::Unanswerable => prompt::UNANSWERABLE_SYSTEM,
            Self::Conflict => prompt::CONFLICT_SYSTEM,
            Self::Tone => prompt::TONE_SYSTEM,
        }
    }
}

// =========================================================
// 判分入口
// =========================================================

/// 判分句柄：provider + 缓存 + 产物要记录的口径。
///
/// 字段约定:
/// - `client`: 判分模型调用封装；用量与调用计数在客户端内部按原子 / 互斥累加，克隆共享；
/// - `cache`: 判分缓存（同一运行内复用），锁只覆盖内存命中与追加写，不含模型调用；
/// - `model` / `kind` / `source`: 写进 `score.json` 的判分模型口径；
/// - `vote_delay`: 三票之间的等待时长，零间隔表示不等待；
/// - `concurrency`: 同一趟内部同时判分的题目数，跨趟仍顺序。
///
/// 说明: 全部判分入口取 `&self`，同一句柄可被同一趟的多个题目共享；
/// 同键并发请求允许各自调用一次模型，缓存插入按键去重且串行落盘。
pub struct Judge {
    client: JudgeClient,
    cache: Mutex<JudgeCache>,
    model: String,
    kind: AnswerModelKind,
    source: JudgeSource,
    vote_delay: Duration,
    concurrency: usize,
}

impl Judge {
    /// 构造判分句柄。
    ///
    /// 参数:
    /// - `provider`: 判分模型 provider；
    /// - `model`: 判分模型标识（参与缓存键）；
    /// - `kind`: 模型类别（本地 / 云端）；
    /// - `source`: 模型来源（矩阵 / 回退答题模型）；
    /// - `cache_path`: 缓存文件路径；
    /// - `vote_delay`: 三票之间的等待时长（单票入口与末票之后不等待）。
    ///
    /// 返回:
    /// - 缓存载入失败返回对应错误。
    ///
    /// 说明: 并发度取默认值，由调用方按环境变量用 `with_concurrency` 覆盖。
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
        kind: AnswerModelKind,
        source: JudgeSource,
        cache_path: PathBuf,
        vote_delay: Duration,
    ) -> RamariaResult<Self> {
        tracing::debug!(vote_delay_ms = vote_delay.as_millis(), "判分投票间隔");
        Ok(Self {
            client: JudgeClient::new(provider),
            cache: Mutex::new(JudgeCache::load(cache_path)?),
            model,
            kind,
            source,
            vote_delay,
            concurrency: DEFAULT_JUDGE_CONCURRENCY,
        })
    }

    /// 覆盖同一趟内部的并发度（超出 `1..=MAX_JUDGE_CONCURRENCY` 时收敛到边界）。
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.clamp(1, MAX_JUDGE_CONCURRENCY);
        self
    }

    /// 同一趟内部同时判分的题目数。
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// 本次运行至今累计的判分用量。
    ///
    /// 返回:
    /// - 逐次调用累加的 token 用量；没有可用量时为全 0。
    ///
    /// 说明: 条件级用量由调用方按"判分前后差值"取增量，避免跨条件重复计入。
    pub fn usage(&self) -> LlmUsage {
        self.client.usage()
    }

    /// 本次运行至今累计的判分调用次数（每次非流式调用 +1，命中缓存不计）。
    pub fn usage_calls(&self) -> usize {
        self.client.usage_calls()
    }

    /// 写进 `score.json` 的判分状态。
    pub fn status(&self) -> JudgeStatus {
        JudgeStatus::Configured {
            model: self.model.clone(),
            kind: self.kind,
            source: self.source,
            votes: JUDGE_VOTES,
            temperature: JUDGE_TEMPERATURE,
            templates: prompt::templates(),
        }
    }

    /// 对一道题执行一个任务的固定三次投票（命中缓存时零模型调用）。
    ///
    /// 参数:
    /// - `task`: 任务类型；
    /// - `question_id` / `condition_id`: 复合键；
    /// - `reply`: 待评回复（只取其摘要进缓存键）；
    /// - `user`: 用户消息；
    /// - `points`: rubric 任务的要点数（其他任务传 `None`）。
    ///
    /// 返回:
    /// - 投票结果；模型调用失败与解析失败只作废该票，不返回错误；
    /// - 缓存读写失败返回对应错误。
    ///
    /// 说明: 票与票之间按构造时的间隔等待，首票立即发出、末票之后不再等待。
    pub async fn ask(
        &self,
        task: JudgeTask,
        question_id: &str,
        condition_id: &str,
        reply: &str,
        user: &str,
        points: Option<usize>,
    ) -> RamariaResult<Verdict> {
        if task == JudgeTask::Tone {
            tracing::debug!("语气维只走单票入口，三次投票入口返回空判定");
            return Ok(Verdict::empty());
        }
        let template_sha256 = prompt::template_sha256(task);
        let key = cache::key(
            &template_sha256,
            &self.model,
            task.as_str(),
            question_id,
            condition_id,
            reply,
        );
        let cached = {
            let cache = lock_recover(&self.cache, "eval_judge.cache");
            cache.get(&key).cloned()
        };
        if let Some(entry) = cached {
            return Ok(Verdict::from_entry(&entry));
        }

        let unit_hint = points.unwrap_or(1);
        let mut votes = Vec::with_capacity(JUDGE_VOTES);
        for index in 0..JUDGE_VOTES {
            // 间隔只压在票与票之间：首票立刻发出，末票之后不留等待。
            if index > 0 && !self.vote_delay.is_zero() {
                tokio::time::sleep(self.vote_delay).await;
            }
            let value = self.vote(task, user, points).await;
            votes.push(value);
        }
        let verdict = Verdict::tally(task, votes, unit_hint);
        if verdict.valid_votes() == JUDGE_VOTES {
            let entry = JudgeCacheEntry {
                key,
                model: self.model.clone(),
                task: task.as_str().to_string(),
                question_id: question_id.to_string(),
                condition_id: condition_id.to_string(),
                template_sha256,
                votes: verdict.votes.clone(),
                value: verdict.value.clone(),
                disagreements: verdict.disagreements,
                units: verdict.units,
            };
            lock_recover(&self.cache, "eval_judge.cache").insert(entry)?;
        }
        Ok(verdict)
    }

    /// 对一道题执行单票判分（命中缓存时零模型调用）。
    ///
    /// 参数:
    /// - `task`: 任务类型（语气维）；
    /// - `question_id` / `condition_id`: 复合键；
    /// - `reply`: 待评回复（只取其摘要进缓存键）；
    /// - `user`: 用户消息。
    ///
    /// 返回:
    /// - 单票判定；解析失败按一次重试处理，重试后仍失败记无效票（`value` 为 `None`）；
    /// - 缓存读写失败返回对应错误。
    ///
    /// 说明: 缓存键与 `ask` 同一套（模板摘要 + 模型 + 任务 + 题目 + 条件 + 回复摘要），
    /// 单票与多票写出的条目只差票面长度；无效票不写缓存，下次重跑整题。
    pub async fn ask_once(
        &self,
        task: JudgeTask,
        question_id: &str,
        condition_id: &str,
        reply: &str,
        user: &str,
    ) -> RamariaResult<Verdict> {
        let template_sha256 = prompt::template_sha256(task);
        let key = cache::key(
            &template_sha256,
            &self.model,
            task.as_str(),
            question_id,
            condition_id,
            reply,
        );
        let cached = {
            let cache = lock_recover(&self.cache, "eval_judge.cache");
            cache.get(&key).cloned()
        };
        if let Some(entry) = cached {
            return Ok(Verdict::from_entry(&entry));
        }

        let vote = self.vote(task, user, None).await;
        let verdict = Verdict::single(vote);
        if verdict.value.is_some() {
            let entry = JudgeCacheEntry {
                key,
                model: self.model.clone(),
                task: task.as_str().to_string(),
                question_id: question_id.to_string(),
                condition_id: condition_id.to_string(),
                template_sha256,
                votes: verdict.votes.clone(),
                value: verdict.value.clone(),
                disagreements: verdict.disagreements,
                units: verdict.units,
            };
            lock_recover(&self.cache, "eval_judge.cache").insert(entry)?;
        }
        Ok(verdict)
    }

    /// 调用一次模型并解析；解析失败按一次重试处理。
    async fn vote(&self, task: JudgeTask, user: &str, points: Option<usize>) -> Option<JudgeValue> {
        let system = task.system();
        if let Ok(body) = self.client.ask(system, user).await {
            if let Some(value) = prompt::parse(task, &body, points) {
                return Some(value);
            }
        }
        let retry = prompt::retry_user(user);
        let body = self.client.ask(system, &retry).await.ok()?;
        prompt::parse(task, &body, points)
    }
}
