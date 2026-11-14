//! crates/ramaria-eval/src/score/tests/concurrency.rs - Ramaria 评测判分并发测试
//!
//! 设计特点:
//! - 按提示内容路由的 mock 带固定延迟：响应与请求一一对应，不依赖调用次序
//! - 同一夹具分别用并发 1 与并发 4 判分：三份产物逐字节一致
//! - 峰值在飞数锁定并发上限：四路档出现并发且不超过 4，串行档恒为 1
//! - 缓存按键去重：两题并发判分后每題一行、键唯一
//! - 只读写临时目录，不触真实评测产物与用户库

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ramaria_core::traits::{LlmProvider, LlmUsage};
use ramaria_testkit::MockRoutedLlm;

use super::{
    CONDITION, conflict_body, observation, points_body, record, refusal_body, temp_dir, write_run,
};
use crate::run::{AnswerModelKind, RunItemRecord};
use crate::score::{
    JudgeSource, SCORE_FILE, SCORED_FILE, ScoreReport, USAGE_FILE, condition, finish, judge,
    judge::{Judge, JudgeCacheEntry},
    read_state,
};

/// 单次判分的模型延迟：让并发窗口真实存在。
const DELAY: Duration = Duration::from_millis(20);

/// 每次调用上报的固定用量。
const USAGE: LlmUsage = LlmUsage {
    prompt_tokens: 100,
    completion_tokens: 10,
    cache_hit_tokens: 40,
    cache_miss_tokens: 60,
};

/// 按调用序号轮回取用的用量序列：四档值互不相同，便于验证不丢不混。
const USAGES: [LlmUsage; 4] = [
    LlmUsage {
        prompt_tokens: 100,
        completion_tokens: 10,
        cache_hit_tokens: 40,
        cache_miss_tokens: 60,
    },
    LlmUsage {
        prompt_tokens: 200,
        completion_tokens: 20,
        cache_hit_tokens: 0,
        cache_miss_tokens: 200,
    },
    LlmUsage {
        prompt_tokens: 300,
        completion_tokens: 30,
        cache_hit_tokens: 150,
        cache_miss_tokens: 150,
    },
    LlmUsage {
        prompt_tokens: 400,
        completion_tokens: 40,
        cache_hit_tokens: 400,
        cache_miss_tokens: 0,
    },
];

// =========================================================
// 夹具
// =========================================================

/// 一道题：q0~q3 普通题，q4 不可回答题，q5 冲突题。
fn item(question_id: &str, category: u8) -> RunItemRecord {
    let reply = if category == 4 {
        "我不知道"
    } else {
        "回复"
    };
    let mut item = record(
        question_id,
        &["m1"],
        Some(reply),
        Some(observation(&[], &[], &[])),
    );
    item.question = format!("题目 {question_id}");
    item.category = vec![category];
    item
}

/// 六道题的夹具：四道普通题 + 一道不可回答题 + 一道冲突题。
fn records() -> Vec<RunItemRecord> {
    vec![
        item("q0", 0),
        item("q1", 0),
        item("q2", 0),
        item("q3", 0),
        item("q4", 4),
        item("q5", 7),
    ]
}

/// 按提示内容应答的 mock：分项命中交替，UA / CD 各一条，语气维走回退值。
fn routed_mock() -> MockRoutedLlm {
    routes(MockRoutedLlm::new("4").with_delay(DELAY).with_usage(USAGE))
}

/// 同上，但每次调用的用量按序号轮回取用（并发下区分每一次调用）。
fn routed_mock_with_usage_seq() -> MockRoutedLlm {
    routes(
        MockRoutedLlm::new("4")
            .with_delay(DELAY)
            .with_usage_seq(USAGES.to_vec()),
    )
}

/// 登记路由：分项命中交替，UA / CD 各一条，语气维走回退值。
fn routes(mut mock: MockRoutedLlm) -> MockRoutedLlm {
    for (index, hit) in [true, false, true, false, true, false]
        .into_iter()
        .enumerate()
    {
        mock = mock.route(
            "任务: rubric_points",
            &format!("题目 q{index}"),
            points_body(&[hit]),
        );
    }
    mock.route("任务: unanswerable", "题目 q4", refusal_body(true))
        .route("任务: conflict", "题目 q5", conflict_body(false))
}

/// 用路由 mock 构造判分句柄（缓存落在判分产物目录）。
fn judge_with(mock: &Arc<MockRoutedLlm>, dir: &Path, concurrency: usize) -> Judge {
    let provider: Arc<dyn LlmProvider> = mock.clone();
    Judge::new(
        provider,
        "judge-model".to_string(),
        AnswerModelKind::Local,
        JudgeSource::Matrix,
        ScoreReport::dir_of(dir).join(judge::CACHE_FILE),
        Duration::ZERO,
    )
    .expect("判分句柄应可构造")
    .with_concurrency(concurrency)
}

// =========================================================
// 并发等价
// =========================================================

/// 一次判分的三份产物与运行级用量。
struct Products {
    score: Vec<u8>,
    scored: Vec<u8>,
    usage: Vec<u8>,
    calls: usize,
    prompt_tokens: u32,
}

/// 清空判分产物与缓存后按指定并发度判分一次。
async fn score_once(dir: &Path, concurrency: usize) -> Products {
    let _ = std::fs::remove_dir_all(ScoreReport::dir_of(dir));
    let state = read_state(dir).expect("运行状态应可读");
    let mock = Arc::new(routed_mock());
    let judge = judge_with(&mock, dir, concurrency);
    let (_, usage) = finish(dir, state, Vec::new(), Some(judge))
        .await
        .expect("判分应成功");
    let score_dir = ScoreReport::dir_of(dir);
    Products {
        score: std::fs::read(score_dir.join(SCORE_FILE)).expect("应写出汇总"),
        scored: std::fs::read(score_dir.join(SCORED_FILE)).expect("应写出逐题"),
        usage: std::fs::read(score_dir.join(USAGE_FILE)).expect("应写出用量"),
        calls: usage.total.judge.calls,
        prompt_tokens: usage.total.judge.prompt_tokens,
    }
}

/// 同一夹具并发 1 与并发 4：三份产物逐字节一致，调用数与 token 总和相同。
#[tokio::test]
async fn concurrent_scoring_matches_serial_byte_for_byte() {
    let dir = temp_dir("concurrency-equivalent");
    write_run(&dir, &records());

    let serial = score_once(&dir, 1).await;
    let parallel = score_once(&dir, 4).await;

    assert_eq!(serial.score, parallel.score, "score.json 应逐字节一致");
    assert_eq!(serial.scored, parallel.scored, "scored.jsonl 应逐字节一致");
    assert_eq!(serial.usage, parallel.usage, "usage.json 应逐字节一致");
    // 分项 6 题 × 3 票 + UA 3 票 + CD 3 票 + 语气 6 票 = 30 次调用。
    assert_eq!(serial.calls, 30, "每次判分都应真实调用模型");
    assert_eq!(parallel.calls, serial.calls, "并发不改变调用总数");
    assert_eq!(
        parallel.prompt_tokens, serial.prompt_tokens,
        "并发不改变 token 总和"
    );

    let report: super::ScoreReport =
        serde_json::from_slice(&parallel.score).expect("汇总应可反序列化");
    assert_eq!(report.conditions[0].judge_failed, 0, "所有题都应判出结果");
    let rubric = report.conditions[0]
        .rubric
        .as_ref()
        .expect("应产出分项汇总");
    assert_eq!(rubric.judged, 6);
    assert_eq!(rubric.micro_mean_ratio, Some(0.5), "三题命中三题未命中");
    let persona = report.conditions[0]
        .persona
        .as_ref()
        .expect("应产出三维汇总");
    assert_eq!(persona.tone, Some(4.0), "语气维取回退回复");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 并发用量
// =========================================================

/// 每次调用的用量随返回值累加：并发 4 与串行 1 的总和一致，且等于逐次之和。
#[tokio::test]
async fn concurrent_usage_sums_every_call_once() {
    async fn score(concurrency: usize) -> (u32, usize) {
        let dir = temp_dir(&format!("concurrency-usage-{concurrency}"));
        let mock = Arc::new(routed_mock_with_usage_seq());
        let judge = judge_with(&mock, &dir, concurrency);
        let entries: Vec<(usize, RunItemRecord)> = records().into_iter().enumerate().collect();
        condition::score_condition(CONDITION, entries, None, None, None, Some(&judge))
            .await
            .expect("条件判分应成功");
        let prompt_tokens = judge.usage().prompt_tokens;
        let calls = mock.calls();
        let _ = std::fs::remove_dir_all(&dir);
        (prompt_tokens, calls)
    }

    let (serial, serial_calls) = score(1).await;
    let (parallel, parallel_calls) = score(4).await;

    assert_eq!(serial_calls, 30, "串行档每次判分都应真实调用模型");
    assert_eq!(parallel_calls, 30, "并发不改变调用总数");
    // 30 次调用按序号轮回取四档用量：各档分别为 8 / 8 / 7 / 7 次。
    let expected = 8 * 100 + 8 * 200 + 7 * 300 + 7 * 400;
    assert_eq!(serial, expected, "串行总量应等于逐次之和");
    assert_eq!(parallel, expected, "并发总量应与串行一致（不丢不混）");
}

// =========================================================
// 并发上限
// =========================================================

/// 四路档峰值在飞数大于 1 且不超过 4；串行档恒为 1。
#[tokio::test]
async fn concurrency_limit_caps_in_flight_requests() {
    for (concurrency, min_peak, max_peak) in [(4usize, 2usize, 4usize), (1, 1, 1)] {
        let dir = temp_dir("concurrency-cap");
        let mock = Arc::new(routed_mock());
        let judge = judge_with(&mock, &dir, concurrency);
        let entries: Vec<(usize, RunItemRecord)> = records().into_iter().enumerate().collect();
        condition::score_condition(CONDITION, entries, None, None, None, Some(&judge))
            .await
            .expect("条件判分应成功");

        let peak = mock.peak_in_flight();
        assert!(
            (min_peak..=max_peak).contains(&peak),
            "并发 {concurrency} 的峰值在飞数不符合预期：{peak}"
        );
        assert_eq!(mock.calls(), 30, "两种并发度的调用总数应相同");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// =========================================================
// 缓存线程安全
// =========================================================

/// 两题同任务并发判分：缓存按题与任务各写一行、键唯一，调用数等于票数之和。
#[tokio::test]
async fn concurrent_cache_writes_one_line_per_question() {
    let dir = temp_dir("concurrency-cache");
    let mock = Arc::new(routed_mock());
    let judge = judge_with(&mock, &dir, 4);
    let entries: Vec<(usize, RunItemRecord)> = records().into_iter().take(2).enumerate().collect();

    condition::score_condition(CONDITION, entries, None, None, None, Some(&judge))
        .await
        .expect("条件判分应成功");

    assert_eq!(mock.calls(), 8, "两题分项各三票 + 语气各一票");
    let body = std::fs::read_to_string(ScoreReport::dir_of(&dir).join(judge::CACHE_FILE))
        .expect("缓存应落盘");
    let cached: Vec<JudgeCacheEntry> = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("缓存行应可解析"))
        .collect();
    assert_eq!(cached.len(), 4, "两题的分项与语气各写一行");
    let mut keys: Vec<&str> = cached.iter().map(|entry| entry.key.as_str()).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), 4, "并发插入不应产生重复键");

    let _ = std::fs::remove_dir_all(&dir);
}
