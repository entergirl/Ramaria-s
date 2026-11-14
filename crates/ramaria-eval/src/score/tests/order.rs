//! crates/ramaria-eval/src/score/tests/order.rs - Ramaria 评测判分分趟顺序测试
//!
//! 设计特点:
//! - 用 mock 的请求记录锁定四个任务的调用顺序，不发起任何网络请求
//! - 夹具按题型交错排列：逐题判分会自然交错，分趟判分必须成块
//! - 分趟不改变装配口径：逐题字段仍按下标落位，调用总数与逐题判分一致
//! - 只读写临时目录，不触真实评测产物与用户库

use std::sync::Arc;

use ramaria_testkit::MockLlm;

use super::{
    CONDITION, conflict_body, judge_for, observation, points_body, record, refusal_body, temp_dir,
};
use crate::score::condition;
use crate::score::judge::JudgeTask;

/// 系统提示 → 任务标识（用于断言调用顺序）。
fn task_of(system: &str) -> &'static str {
    if system == JudgeTask::RubricPoints.system() {
        "rubric"
    } else if system == JudgeTask::Unanswerable.system() {
        "unanswerable"
    } else if system == JudgeTask::Conflict.system() {
        "conflict"
    } else if system == JudgeTask::Tone.system() {
        "tone"
    } else {
        "unknown"
    }
}

/// 四个任务各自成块：分项 → 不可回答 → 冲突 → 语气；逐题结果仍按下标装配。
#[tokio::test]
async fn judge_runs_task_by_task_without_interleaving() {
    let dir = temp_dir("judge-order");
    let plain_a = record(
        "q0",
        &["m1"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    let mut unanswerable = record(
        "q4",
        &[],
        Some("我不知道"),
        Some(observation(&[], &[], &[])),
    );
    unanswerable.category = vec![4];
    unanswerable.score_points.clear();
    let mut conflict = record(
        "q7",
        &["m2"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    conflict.category = vec![7];
    conflict.score_points.clear();
    let plain_b = record(
        "q1",
        &["m1"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    let mut empty_gold = record(
        "q9",
        &["m3"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    empty_gold.gold_answer.clear();
    empty_gold.score_points.clear();

    // 队列覆盖前三个任务的全部票；语气维走 mock 的固定回复「4」。
    let mock = Arc::new(MockLlm::new("4"));
    let mut queue: Vec<String> = Vec::new();
    for _ in 0..2 {
        for _ in 0..3 {
            queue.push(points_body(&[true]));
        }
    }
    for _ in 0..3 {
        queue.push(refusal_body(true));
    }
    for _ in 0..3 {
        queue.push(conflict_body(false));
    }
    mock.set_responses(queue);

    let judge = judge_for(&mock, &dir);
    // 题型交错排列：逐题判分会在 q0 之后插入 UA / CD，分趟判分不会。
    let entries = vec![
        (0, plain_a),
        (1, unanswerable),
        (2, conflict),
        (3, plain_b),
        (4, empty_gold),
    ];
    let (score, items, usage) =
        condition::score_condition(CONDITION, entries, None, None, None, Some(&judge))
            .await
            .expect("条件判分应成功");

    let tasks: Vec<&str> = mock
        .requests()
        .iter()
        .map(|request| task_of(&request.system_prompt))
        .collect();
    let mut expected = vec!["rubric"; 6];
    expected.extend(vec!["unanswerable"; 3]);
    expected.extend(vec!["conflict"; 3]);
    expected.extend(vec!["tone"; 4]);
    assert_eq!(tasks, expected, "同一任务应连续推进，不出现交错");

    assert_eq!(
        mock.chat_calls(),
        16,
        "两题分项各三票 + UA 三票 + CD 三票 + 语气四票"
    );
    assert_eq!(usage.judge.calls, 16, "条件用量应等于实际调用次数");

    // ---- 分趟只改调用顺序，逐题结果照旧按下标落位 ----
    assert!(items[0].item.rubric.is_some(), "普通题应产出分项结果");
    assert!(items[1].item.rubric.is_none(), "无要点不参与分项判分");
    assert_eq!(items[1].item.credibility.ua_judge, Some(true));
    assert_eq!(items[2].item.credibility.cd_judge, Some(false));
    assert!(
        items[2].item.credibility.ua_judge.is_none(),
        "非 UA 题不写 UA 列"
    );
    assert!(
        items[1].item.credibility.cd_judge.is_none(),
        "非 CD 题不写 CD 列"
    );
    assert!(items[3].item.rubric.is_some(), "普通题应产出分项结果");
    assert_eq!(
        items[0]
            .item
            .persona
            .as_ref()
            .and_then(|persona| persona.tone),
        Some(4),
        "语气维在趟四填入"
    );
    assert!(
        items[4]
            .item
            .persona
            .as_ref()
            .is_some_and(|persona| persona.tone.is_none()),
        "参考为空的题不参与语气维"
    );

    assert_eq!(score.rubric.expect("应产出分项汇总").judged, 2);
    let persona = score.persona.expect("应产出三维汇总");
    assert_eq!(persona.questions, 5, "有回复的题都参与三维");
    assert_eq!(persona.tone, Some(4.0), "四题拿到分值");
    assert_eq!(persona.tone_failed, 1, "参考为空的题记语气维失败");
    assert_eq!(score.judge_failed, 0);
    assert_eq!(score.credibility.ua_judge.expect("应有 UA 判定").judged, 1);
    assert_eq!(score.credibility.cd_judge.expect("应有 CD 判定").judged, 1);

    let _ = std::fs::remove_dir_all(&dir);
}
