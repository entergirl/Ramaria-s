//! crates/ramaria-eval/src/tui/console/review/detail/tests.rs - Ramaria 审查详情测试
//!
//! 设计特点:
//! - 合成逐题记录与判分结果，只断言分节里的关键文案
//! - 一个用例覆盖全部分节，未判分另起一条
//! - 不做终端渲染，纯函数断言便于定位

use super::*;

use ramaria_core::traits::LlmUsage;
use serde_json::json;

use crate::run::ConditionLine;
use crate::score::ScoredItem;

// =========================================================
// 夹具
// =========================================================

fn condition_line() -> ConditionLine {
    ConditionLine {
        corpus_name: "lifebench".to_string(),
        corpus_version: "2.0".to_string(),
        language: "zh".to_string(),
        corpus_sha256: String::new(),
        memory_model: "local".to_string(),
        memory_model_kind: "local".to_string(),
        memory_temperature: 0.0,
        memory_max_tokens: 0,
        embedding_fingerprint: String::new(),
        embedding_dimension: None,
        prompt_template_version: String::new(),
        schema_version: 1,
        index_status: "ready".to_string(),
        device_os: "windows".to_string(),
        device_arch: "x86_64".to_string(),
        device_cores: 1,
    }
}

fn item() -> RunItemRecord {
    RunItemRecord {
        question_id: "q0".to_string(),
        condition_id: "c1".to_string(),
        tier: "u1".to_string(),
        pipeline: "l1".to_string(),
        budget_chars: 800,
        answer_model: "local".to_string(),
        answer_model_effective: "local".to_string(),
        sample_key: "alice".to_string(),
        persona_uid: "lifebench-alice".to_string(),
        ask_time: "2025-01-02".to_string(),
        as_of_ms: 0,
        question: "她上次说了什么？".to_string(),
        gold_answer: "她说了旅行计划。".to_string(),
        gold_message_ids: vec!["m1".to_string(), "m2".to_string()],
        evidence: vec!["chat1".to_string(), "chat2".to_string()],
        question_type: vec!["Single_hop".to_string()],
        score_points: Vec::new(),
        category: vec![0],
        reply: Some("她说要去旅行。".to_string()),
        reply_chars: 8,
        recall: None,
        preset_chars: Some(120),
        usage: Some(LlmUsage {
            prompt_tokens: 10,
            completion_tokens: 20,
            cache_hit_tokens: 4,
            cache_miss_tokens: 6,
        }),
        elapsed_ms: 1500,
        error: None,
        attempts: 2,
        conditions: condition_line(),
    }
}

fn scored() -> ScoredItem {
    serde_json::from_value(json!({
        "question_id": "q0",
        "condition_id": "c1",
        "tier": "u1",
        "pipeline": "l1",
        "budget_chars": 800,
        "answer_model": "local",
        "sample_key": "alice",
        "category": [0],
        "gold_count": 2,
        "retrieval": {
            "coverage_pre": 0.5,
            "coverage_post": 1.0,
            "precision_pre": 0.2,
            "precision_post": 0.5,
            "rank_pre": 2,
            "rank_post": 1,
            "units_pre": 10,
            "units_post": 2,
            "unmappable_pre": 0,
            "unmappable_post": 0,
            "artifact": null
        },
        "credibility": {
            "ua_refusal_hint": null,
            "cd_score_points": 0,
            "ua_judge": null,
            "cd_judge": null,
            "judge_failed": false
        },
        "rubric": {
            "points": [
                {
                    "index": 0,
                    "description": "要点一",
                    "weight": 6.0,
                    "hit": true,
                    "votes": [true, true, true],
                    "reason": "答到了"
                },
                {
                    "index": 1,
                    "description": "要点二",
                    "weight": 4.0,
                    "hit": false,
                    "votes": [false, false, false],
                    "reason": "没答到"
                }
            ],
            "score": 6.0,
            "max": 10.0,
            "ratio": 0.6,
            "full_hit": false,
            "judge_failed": false
        },
        "persona": null,
        "notes": []
    }))
    .expect("判分夹具应可反序列化")
}

/// 拼接全部详情文本。
fn joined(view: &DetailView) -> String {
    view.lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// =========================================================
// 用例
// =========================================================

/// 有判分时各分节都有内容。
#[test]
fn detail_renders_all_sections() {
    let view = build_detail(item(), Some(scored()));
    let text = joined(&view);
    for needle in [
        "输入",
        "题目: 她上次说了什么？",
        "参考答案",
        "她说了旅行计划。",
        "gold 消息: 2 条",
        "证据: chat1, chat2",
        "回复",
        "她说要去旅行。",
        "尝试 2 次",
        "预置上下文 120 字符",
        "用量: 输入 10 / 输出 20",
        "无召回（预置上下文或失败）",
        "检索指标",
        "覆盖 0.50→1.00",
        "分项得分",
        "得分 6.00 / 10.00（60.0%）",
        "[命中] 6.0 — 答到了（票 3/3）",
        "[未命中] 4.0 — 没答到（票 3/3）",
        "可信度",
    ] {
        assert!(text.contains(needle), "详情应包含 {needle}，实际:\n{text}");
    }
}

/// 未判分时给出提示，判分相关分节不出现。
#[test]
fn detail_without_score_prompts_scoring() {
    let view = build_detail(item(), None);
    let text = joined(&view);
    assert!(text.contains("未判分"), "应提示未判分:\n{text}");
    assert!(!text.contains("分项得分"), "未判分不应有分项分节:\n{text}");
}

/// 失败题显示错误原因。
#[test]
fn detail_failure_shows_error() {
    let mut record = item();
    record.reply = None;
    record.error = Some("模型调用超时".to_string());
    let view = build_detail(record, None);
    assert!(
        joined(&view).contains("失败: 模型调用超时"),
        "应显示失败原因:\n{}",
        joined(&view)
    );
}
