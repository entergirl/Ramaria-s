//! crates/ramaria-eval/src/score/judge/tests.rs - Ramaria 评测判分单元测试
//!
//! 设计特点:
//! - 用 mock provider 锁定投票、重试与缓存语义，不发起任何网络请求
//! - 响应队列逐次消费，票面顺序与调用次数可直接断言
//! - 缓存文件写到临时目录，不读写真实评测产物
//! - 只断言可复现的计数与票面，不断言提示词全文

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ramaria_core::traits::{LlmProvider, LlmUsage};
use ramaria_testkit::{MockLlm, MockUsageLlm};

use super::cache::CACHE_FILE;
use super::prompt;
use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("ramaria-eval-judge-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    dir
}

fn judge_with(mock: &Arc<MockLlm>, dir: &Path) -> Judge {
    let provider: Arc<dyn LlmProvider> = mock.clone();
    Judge::new(
        provider,
        "judge-model".to_string(),
        AnswerModelKind::Local,
        JudgeSource::Matrix,
        dir.join(CACHE_FILE),
        Duration::ZERO,
    )
    .expect("判分句柄应可构造")
}

fn points_body(hits: &[bool]) -> String {
    let points: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(index, hit)| format!("{{\"index\":{index},\"hit\":{hit},\"reason\":\"r{index}\"}}"))
        .collect();
    format!("{{\"points\":[{}]}}", points.join(","))
}

fn refusal_body(value: bool) -> String {
    format!("{{\"refused\":{value},\"reason\":\"r\"}}")
}

// =========================================================
// 用量
// =========================================================

/// 三次投票各自上报一次用量：调用计数与 token 合计按调用数累加，命中率按输入占比。
#[tokio::test]
async fn usage_accumulates_over_votes() {
    let dir = temp_dir("usage");
    let usage = LlmUsage {
        prompt_tokens: 500,
        completion_tokens: 20,
        cache_hit_tokens: 400,
        cache_miss_tokens: 100,
    };
    let mock = Arc::new(MockUsageLlm::new(&refusal_body(true), usage));
    let provider: Arc<dyn LlmProvider> = mock.clone();
    let judge = Judge::new(
        provider,
        "judge-model".to_string(),
        AnswerModelKind::Local,
        JudgeSource::Matrix,
        dir.join(CACHE_FILE),
        Duration::ZERO,
    )
    .expect("判分句柄应可构造");

    judge
        .ask(
            JudgeTask::Unanswerable,
            "q1",
            "c1",
            "回复",
            "用户消息",
            None,
        )
        .await
        .expect("判分应成功");

    assert_eq!(judge.usage_calls(), JUDGE_VOTES, "三票即三次调用");
    assert_eq!(mock.calls(), JUDGE_VOTES);
    let total = judge.usage();
    assert_eq!(total.prompt_tokens, 500 * JUDGE_VOTES as u32);
    assert_eq!(total.completion_tokens, 20 * JUDGE_VOTES as u32);
    assert_eq!(total.cache_hit_tokens, 400 * JUDGE_VOTES as u32);
    assert_eq!(total.cache_miss_tokens, 100 * JUDGE_VOTES as u32);
    let rate = total.cache_hit_rate().expect("输入非 0 应有命中率");
    assert!((rate - 0.8).abs() < 1e-9, "命中率={rate}");
}

/// 命中缓存不产生用量：同一题第二次判分零调用、用量不变。
#[tokio::test]
async fn cached_verdict_adds_no_usage() {
    let dir = temp_dir("usage-cache");
    let usage = LlmUsage {
        prompt_tokens: 300,
        completion_tokens: 10,
        cache_hit_tokens: 0,
        cache_miss_tokens: 300,
    };
    let mock = Arc::new(MockUsageLlm::new(&refusal_body(false), usage));
    let provider: Arc<dyn LlmProvider> = mock.clone();
    let judge = Judge::new(
        provider,
        "judge-model".to_string(),
        AnswerModelKind::Local,
        JudgeSource::Matrix,
        dir.join(CACHE_FILE),
        Duration::ZERO,
    )
    .expect("判分句柄应可构造");

    for _ in 0..2 {
        judge
            .ask(
                JudgeTask::Unanswerable,
                "q1",
                "c1",
                "回复",
                "用户消息",
                None,
            )
            .await
            .expect("判分应成功");
    }

    assert_eq!(mock.calls(), JUDGE_VOTES, "第二次走缓存不再调模型");
    assert_eq!(judge.usage_calls(), JUDGE_VOTES, "缓存命中不计调用");
    assert_eq!(judge.usage().prompt_tokens, 300 * JUDGE_VOTES as u32);
}

// =========================================================
// 投票
// =========================================================

/// 三票两真一假：多数为命中，票面三项齐全，且记一条分歧。
#[tokio::test]
async fn points_majority_keeps_all_votes() {
    let dir = temp_dir("majority");
    let mock = Arc::new(MockLlm::new(""));
    mock.set_responses(vec![
        points_body(&[true]),
        points_body(&[false]),
        points_body(&[true]),
    ]);
    let judge = judge_with(&mock, &dir);

    let verdict = judge
        .ask(
            JudgeTask::RubricPoints,
            "q1",
            "c1",
            "回复",
            "用户消息",
            Some(1),
        )
        .await
        .expect("投票应成功");

    assert_eq!(verdict.valid_votes(), 3);
    assert_eq!(
        verdict.point_votes(0),
        vec![Some(true), Some(false), Some(true)],
        "三票票面应按调用顺序保留"
    );
    assert_eq!(verdict.point(0).map(|point| point.hit), Some(true));
    assert_eq!(verdict.disagreements, 1, "一真两假之外的一票即一条分歧");
    assert_eq!(verdict.units, 1);
    assert!(!verdict.failed());
    assert_eq!(mock.chat_calls(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 解析失败重试一次仍失败：该票作废；有效票不足两票即判分失败且不写缓存。
#[tokio::test]
async fn parse_failure_retries_once_then_fails_whole_question() {
    let dir = temp_dir("retry");
    let mock = Arc::new(MockLlm::new(""));
    mock.set_responses(vec![
        "不是 JSON".to_string(),
        "还不是 JSON".to_string(),
        "仍然不是".to_string(),
        "依旧不是".to_string(),
        points_body(&[true]),
    ]);
    let judge = judge_with(&mock, &dir);

    let verdict = judge
        .ask(
            JudgeTask::RubricPoints,
            "q1",
            "c1",
            "回复",
            "用户消息",
            Some(1),
        )
        .await
        .expect("投票流程不应返回错误");

    assert_eq!(verdict.valid_votes(), 1, "前两票各重试一次后作废");
    assert!(verdict.failed(), "有效票不足两票应判分失败");
    assert_eq!(mock.chat_calls(), 5, "两票各两次调用，第三票一次");
    assert!(!dir.join(CACHE_FILE).exists(), "任一票失败时整题不写缓存");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 布尔任务两真一假：多数为真，并记一条分歧。
#[tokio::test]
async fn flag_majority_and_disagreement() {
    let dir = temp_dir("flag");
    let mock = Arc::new(MockLlm::new(""));
    mock.set_responses(vec![
        refusal_body(true),
        refusal_body(false),
        refusal_body(true),
    ]);
    let judge = judge_with(&mock, &dir);

    let verdict = judge
        .ask(
            JudgeTask::Unanswerable,
            "q1",
            "c1",
            "回复",
            "用户消息",
            None,
        )
        .await
        .expect("投票应成功");

    assert_eq!(verdict.flag(), Some(true));
    assert_eq!(verdict.disagreements, 1);
    assert_eq!(verdict.units, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 缓存
// =========================================================

/// 同一输入第二次判分命中缓存：不再调用模型，票面完全一致。
#[tokio::test]
async fn cache_hit_skips_model_calls() {
    let dir = temp_dir("cache");
    let mock = Arc::new(MockLlm::with_reply(&points_body(&[true, false])));
    let judge = judge_with(&mock, &dir);
    let first = judge
        .ask(
            JudgeTask::RubricPoints,
            "q1",
            "c1",
            "回复",
            "用户消息",
            Some(2),
        )
        .await
        .expect("首次投票应成功");
    assert_eq!(mock.chat_calls(), 3);
    drop(judge);

    let judge = judge_with(&mock, &dir);
    let second = judge
        .ask(
            JudgeTask::RubricPoints,
            "q1",
            "c1",
            "回复",
            "用户消息",
            Some(2),
        )
        .await
        .expect("命中缓存应成功");
    assert_eq!(mock.chat_calls(), 3, "命中缓存不再调用模型");
    assert_eq!(first, second, "缓存还原的票面应与首次一致");

    let body = std::fs::read_to_string(dir.join(CACHE_FILE)).expect("缓存应落盘");
    assert_eq!(body.lines().count(), 1, "一题一任务只写一行");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 缓存坏行：载入时报错并带行号。
#[tokio::test]
async fn cache_corrupt_line_reports_line_number() {
    let dir = temp_dir("cache-broken");
    std::fs::write(dir.join(CACHE_FILE), "\n坏行\n").expect("缓存夹具应可写");
    let mock = Arc::new(MockLlm::new(""));
    let provider: Arc<dyn LlmProvider> = mock.clone();

    let error = Judge::new(
        provider,
        "judge-model".to_string(),
        AnswerModelKind::Local,
        JudgeSource::Matrix,
        dir.join(CACHE_FILE),
        Duration::ZERO,
    )
    .err()
    .expect("坏行应报错");

    let text = error.to_string();
    assert!(text.contains("第 2 行"), "错误应带行号，实际 {text}");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 投票间隔
// =========================================================

/// 间隔解析：未设置与非法取默认值，`0` 不等待，正整数按毫秒。
#[test]
fn vote_delay_parses_env_values() {
    let default = Duration::from_millis(DEFAULT_VOTE_DELAY_MS);
    assert_eq!(DEFAULT_VOTE_DELAY_MS, 0, "默认不留间隔");
    assert_eq!(parse_vote_delay(None), default, "未设置取默认");
    assert_eq!(parse_vote_delay(Some("")), default, "空白取默认");
    assert_eq!(parse_vote_delay(Some("0")), Duration::ZERO, "0 不等待");
    assert_eq!(parse_vote_delay(Some("1500")), Duration::from_millis(1500));
    assert_eq!(parse_vote_delay(Some("abc")), default, "非数字取默认");
    assert_eq!(parse_vote_delay(Some("-5")), default, "负数取默认");
}

/// 并发度解析：默认四路，非法与越界回退默认，边界值按原值。
#[test]
fn concurrency_parses_env_values() {
    assert_eq!(DEFAULT_JUDGE_CONCURRENCY, 4, "默认四路");
    assert_eq!(parse_concurrency(None), 4, "未设置取默认");
    assert_eq!(parse_concurrency(Some("")), 4, "空白取默认");
    assert_eq!(parse_concurrency(Some("abc")), 4, "非数字取默认");
    assert_eq!(parse_concurrency(Some("0")), 4, "0 越界回退默认");
    assert_eq!(parse_concurrency(Some("17")), 4, "超过上限回退默认");
    assert_eq!(parse_concurrency(Some("1")), 1);
    assert_eq!(parse_concurrency(Some("16")), MAX_JUDGE_CONCURRENCY);
    assert_eq!(parse_concurrency(Some(" 8 ")), 8, "两侧空白应忽略");
}

/// 句柄并发度默认四路；显式设置越界时收敛到 1..=16。
#[test]
fn judge_concurrency_clamps_to_bounds() {
    let dir = temp_dir("concurrency-clamp");
    let mock = Arc::new(MockLlm::local());
    let judge = judge_with(&mock, &dir);
    assert_eq!(judge.concurrency(), DEFAULT_JUDGE_CONCURRENCY);
    let judge = judge_with(&mock, &dir).with_concurrency(0);
    assert_eq!(judge.concurrency(), 1);
    let judge = judge.with_concurrency(99);
    assert_eq!(judge.concurrency(), MAX_JUDGE_CONCURRENCY);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 模板
// =========================================================

/// 四个在用模板的摘要齐全、互不相同，且系统提示与任务口径一致。
#[test]
fn templates_cover_four_tasks() {
    let templates = prompt::templates();
    assert_eq!(templates.len(), 4);
    let mut hashes: Vec<&str> = templates.iter().map(|hash| hash.sha256.as_str()).collect();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), 4, "四个任务的模板摘要应互不相同");
    for template in &templates {
        assert_eq!(template.sha256.len(), 64, "摘要应是 sha256 十六进制");
    }
    for task in [
        JudgeTask::RubricPoints,
        JudgeTask::Unanswerable,
        JudgeTask::Conflict,
    ] {
        assert!(
            task.system()
                .starts_with(&format!("任务: {}", task.as_str())),
            "系统提示首行应带任务名"
        );
    }
    let tone = JudgeTask::Tone.system();
    assert!(
        tone.starts_with("请按 1~5 分评估"),
        "语气维提示应是 rubric 正文"
    );
    assert!(tone.contains("【示例 1】"), "语气维提示应带 few-shot 锚定");
    assert!(
        tone.contains("重要：不要按回复长短判分"),
        "语气维提示应保留长度中性约束"
    );
}

// =========================================================
// 单票（语气维）
// =========================================================

/// 单票判分：一次调用取分，第二次命中缓存零调用，票面长度 1。
#[tokio::test]
async fn ask_once_scores_and_caches_single_vote() {
    let dir = temp_dir("once");
    let mock = Arc::new(MockLlm::with_reply("{\"score\":5,\"reason\":\"风格一致\"}"));
    let judge = judge_with(&mock, &dir);

    let verdict = judge
        .ask_once(JudgeTask::Tone, "q1", "c1", "回复", "用户消息")
        .await
        .expect("单票判分应成功");
    assert_eq!(verdict.score(), Some(5));
    assert_eq!(verdict.votes.len(), 1, "单票入口只记一票");
    assert_eq!(verdict.units, 1);
    assert_eq!(verdict.disagreements, 0, "单票不产分歧");
    assert_eq!(mock.chat_calls(), 1);

    let again = judge
        .ask_once(JudgeTask::Tone, "q1", "c1", "回复", "用户消息")
        .await
        .expect("命中缓存应成功");
    assert_eq!(mock.chat_calls(), 1, "命中缓存不再调用模型");
    assert_eq!(verdict, again, "缓存还原的单票应与首次一致");

    let body = std::fs::read_to_string(dir.join(CACHE_FILE)).expect("缓存应落盘");
    assert_eq!(body.lines().count(), 1, "一题一任务只写一行");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 单票解析失败重试一次仍失败：记为无效票且不写缓存。
#[tokio::test]
async fn ask_once_retries_once_then_invalid() {
    let dir = temp_dir("once-retry");
    let mock = Arc::new(MockLlm::with_reply("这条回复看不出语气"));
    let judge = judge_with(&mock, &dir);

    let verdict = judge
        .ask_once(JudgeTask::Tone, "q1", "c1", "回复", "用户消息")
        .await
        .expect("单票流程不应返回错误");
    assert_eq!(verdict.score(), None, "解析不出 1~5 应记为无效");
    assert_eq!(verdict.units, 0);
    assert_eq!(mock.chat_calls(), 2, "解析失败只重试一次");
    assert!(!dir.join(CACHE_FILE).exists(), "无效票不写缓存");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 冲突判定要带参考答案：否则判分模型无从判断问题里哪个说法是错误前提。
#[test]
fn conflict_user_includes_reference() {
    let user = prompt::conflict_user("问题文本", "参考文本", "回复文本");
    assert!(user.contains("问题: 问题文本"));
    assert!(user.contains("参考答案: 参考文本"));
    assert!(user.contains("待评回复: 回复文本"));
}
