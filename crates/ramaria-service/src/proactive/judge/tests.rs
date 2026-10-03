//! crates/ramaria-service/src/proactive/judge/tests.rs - Ramaria 主动对话 AI 判据测试
//!
//! 设计特点:
//! - 脚本化 LLM 按调用序返回预设响应，覆盖解析递进与裁决分支
//! - 空候选路径断言 LLM 零调用（不发起无意义请求）
//! - 提示词组装直接断言（来源 / 效价标注、无锚点占位与无历史回退）
//! - 使用真实 SQLite 临时库与共享 LLM 句柄，不依赖网络与真实时钟

use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::engine::Engine;
use crate::test_support::{ScriptedLlm, engine_with_failing_llm, engine_with_shared_scripted_llm};
use ramaria_core::config::RamariaConfig;

// =========================================================
// 夹具
// =========================================================

/// 正常开口的裁决输出（合法 JSON）。
const SPEAK_JSON: &str = r#"{"speak": true, "candidate_id": "c0", "angle": "关心近况", "tone": "温和", "reason_bucket": "speak"}"#;

/// 信号简报夹具。
fn signals() -> JudgeSignals {
    JudgeSignals {
        hour: 14,
        activity_weight: 0.72,
        hours_since_last_chat: Some(6.5),
        silence_streak: 0,
        daily_count: 1,
    }
}

/// 候选夹具：事件正向 / 未了结负向 / 轻触达无锚点。
fn candidates() -> Vec<JudgeCandidate> {
    vec![
        JudgeCandidate {
            id: "c0".to_string(),
            source: "event".to_string(),
            anchor: Some("陶艺展：周末逛了陶艺展，用户很喜欢".to_string()),
            valence: 0.6,
        },
        JudgeCandidate {
            id: "c1".to_string(),
            source: "unresolved".to_string(),
            anchor: Some("与同事的误会还没说开".to_string()),
            valence: -0.5,
        },
        JudgeCandidate {
            id: "c2".to_string(),
            source: "light_touch".to_string(),
            anchor: None,
            valence: 0.0,
        },
    ]
}

/// 以脚本化响应装配引擎（返回引擎 / 共享 LLM 句柄 / 临时目录）。
async fn engine_with_replies(tag: &str, replies: &[&str]) -> (Engine, Arc<ScriptedLlm>, PathBuf) {
    let llm = Arc::new(ScriptedLlm::replies(replies));
    let (engine, _storage, dir) =
        engine_with_shared_scripted_llm(tag, Arc::clone(&llm), RamariaConfig::default(), None)
            .await;
    (engine, llm, dir)
}

// =========================================================
// 裁决用例
// =========================================================

/// 正常 JSON：speak=true 且候选有效 → 产出裁决，字段与输入回引一致。
#[tokio::test]
async fn speak_true_valid_candidate_returns_decision() {
    let (engine, llm, dir) = engine_with_replies("judge-speak-true", &[SPEAK_JSON]).await;

    let JudgeOutcome::Speak(decision) = decide(&engine, &candidates(), &signals()).await else {
        panic!("有效裁决应产出");
    };
    assert_eq!(decision.candidate_id, "c0");
    assert_eq!(decision.angle.as_deref(), Some("关心近况"));
    assert_eq!(decision.tone.as_deref(), Some("温和"));
    assert_eq!(decision.reason_bucket, "speak");
    assert_eq!(llm.call_count(), 1, "判据应恰好调用一次 LLM");

    let _ = std::fs::remove_dir_all(dir);
}

/// speak=false（沉默裁决）→ Silent，仍发起了一次调用。
#[tokio::test]
async fn speak_false_returns_none() {
    let reply = r#"{"speak": false, "candidate_id": "", "angle": "", "tone": "", "reason_bucket": "too_recent"}"#;
    let (engine, llm, dir) = engine_with_replies("judge-speak-false", &[reply]).await;

    assert!(matches!(
        decide(&engine, &candidates(), &signals()).await,
        JudgeOutcome::Silent
    ));
    assert_eq!(llm.call_count(), 1, "沉默裁决也消耗一次调用");

    let _ = std::fs::remove_dir_all(dir);
}

/// 候选编号不在候选中（幻觉编号）→ Failed。
#[tokio::test]
async fn invalid_candidate_id_returns_none() {
    let reply = r#"{"speak": true, "candidate_id": "c9", "angle": "关心", "tone": "温和", "reason_bucket": "speak"}"#;
    let (engine, _llm, dir) = engine_with_replies("judge-invalid-id", &[reply]).await;

    assert!(matches!(
        decide(&engine, &candidates(), &signals()).await,
        JudgeOutcome::Failed
    ));

    let _ = std::fs::remove_dir_all(dir);
}

/// 非 JSON 文本（无法解析）→ Failed。
#[tokio::test]
async fn garbage_response_returns_none() {
    let (engine, _llm, dir) = engine_with_replies(
        "judge-garbage",
        &["我觉得现在不适合说话，最近的话题都聊过了。"],
    )
    .await;

    assert!(matches!(
        decide(&engine, &candidates(), &signals()).await,
        JudgeOutcome::Failed
    ));

    let _ = std::fs::remove_dir_all(dir);
}

/// `<think>` 包裹的 JSON：剥离思考内容后解析成功。
#[tokio::test]
async fn think_wrapped_json_parses() {
    let reply = format!(
        "<think>先看看有没有值得提起的话题</think>\n{}",
        r#"{"speak": true, "candidate_id": "c1", "angle": "关心", "tone": "温和", "reason_bucket": "speak"}"#
    );
    let (engine, _llm, dir) = engine_with_replies("judge-think", &[reply.as_str()]).await;

    let JudgeOutcome::Speak(decision) = decide(&engine, &candidates(), &signals()).await else {
        panic!("剥离 think 后应解析成功");
    };
    assert_eq!(decision.candidate_id, "c1");

    let _ = std::fs::remove_dir_all(dir);
}

/// think 包裹 + 前后说明文字：剥离后仍带说明，最终提取 JSON 对象解析成功。
#[tokio::test]
async fn think_and_prose_falls_back_to_extraction() {
    let reply = format!(
        "<think>先判断</think>\n判断如下：{}，以上。",
        r#"{"speak": true, "candidate_id": "c0", "angle": "关心", "tone": "温和", "reason_bucket": "speak"}"#
    );
    let (engine, _llm, dir) = engine_with_replies("judge-think-prose", &[reply.as_str()]).await;

    let JudgeOutcome::Speak(decision) = decide(&engine, &candidates(), &signals()).await else {
        panic!("组合场景应回退到对象提取");
    };
    assert_eq!(decision.candidate_id, "c0");

    let _ = std::fs::remove_dir_all(dir);
}

/// LLM 调用失败（系统级不可用）→ Failed。
#[tokio::test]
async fn llm_failure_returns_none() {
    let (engine, _storage, dir) = engine_with_failing_llm("judge-llm-fail").await;

    assert!(matches!(
        decide(&engine, &candidates(), &signals()).await,
        JudgeOutcome::Failed
    ));

    let _ = std::fs::remove_dir_all(dir);
}

/// JSON 前后夹带说明文字：提取首个 JSON 对象后解析成功。
#[tokio::test]
async fn json_with_surrounding_prose_parses() {
    let reply = r#"判断如下：{"speak": true, "candidate_id": "c2", "angle": "问候", "tone": "轻松", "reason_bucket": "speak"}，以上。"#;
    let (engine, _llm, dir) = engine_with_replies("judge-prose", &[reply]).await;

    let JudgeOutcome::Speak(decision) = decide(&engine, &candidates(), &signals()).await else {
        panic!("提取 JSON 对象后应解析成功");
    };
    assert_eq!(decision.candidate_id, "c2");
    assert_eq!(decision.angle.as_deref(), Some("问候"));

    let _ = std::fs::remove_dir_all(dir);
}

/// 原因类别归一：非白名单值落 `unknown`，合法值原样保留，缺字段回退。
#[tokio::test]
async fn reason_bucket_unknown_falls_back() {
    let replies = [
        r#"{"speak": true, "candidate_id": "c0", "angle": "关心", "tone": "温和", "reason_bucket": "随便写的"}"#,
        r#"{"speak": true, "candidate_id": "c1", "angle": "关心", "tone": "温和", "reason_bucket": "too_recent"}"#,
        r#"{"speak": true, "candidate_id": "c2"}"#,
    ];
    let (engine, _llm, dir) = engine_with_replies("judge-bucket", &replies).await;

    let JudgeOutcome::Speak(first) = decide(&engine, &candidates(), &signals()).await else {
        panic!("第一次裁决应产出");
    };
    assert_eq!(first.reason_bucket, "unknown", "非白名单值应归一为 unknown");

    let JudgeOutcome::Speak(second) = decide(&engine, &candidates(), &signals()).await else {
        panic!("第二次裁决应产出");
    };
    assert_eq!(second.reason_bucket, "too_recent", "白名单值应原样保留");

    let JudgeOutcome::Speak(third) = decide(&engine, &candidates(), &signals()).await else {
        panic!("缺字段裁决应产出");
    };
    assert_eq!(third.reason_bucket, "unknown", "缺字段应归一为 unknown");
    assert!(third.angle.is_none(), "缺字段角度应为 None");
    assert!(third.tone.is_none(), "缺字段语气应为 None");

    let _ = std::fs::remove_dir_all(dir);
}

/// angle / tone 归一：空白串 → None；超长按字符截断到上限。
#[tokio::test]
async fn angle_tone_trimmed_and_truncated() {
    let long_tone = "这是一个特别特别特别特别特别特别长的语气描述";
    assert_eq!(long_tone.chars().count(), 22, "夹具语气长度应超过截断上限");
    let reply = format!(
        r#"{{"speak": true, "candidate_id": "c0", "angle": "   ", "tone": "{long_tone}", "reason_bucket": "speak"}}"#
    );
    let (engine, _llm, dir) = engine_with_replies("judge-field", &[reply.as_str()]).await;

    let JudgeOutcome::Speak(decision) = decide(&engine, &candidates(), &signals()).await else {
        panic!("裁决应产出");
    };
    assert!(decision.angle.is_none(), "空白角度应归一为 None");
    let tone = decision.tone.expect("语气应保留");
    assert_eq!(tone.chars().count(), JUDGE_MAX_FIELD_CHARS);
    assert_eq!(tone, "这是一个特别特别特别特别特别特别长的语气");

    let _ = std::fs::remove_dir_all(dir);
}

/// 空候选：直接返回 Failed 且不发起 LLM 调用。
#[tokio::test]
async fn empty_candidates_skips_decision() {
    let (engine, llm, dir) = engine_with_replies("judge-empty", &[SPEAK_JSON]).await;

    assert!(matches!(
        decide(&engine, &[], &signals()).await,
        JudgeOutcome::Failed
    ));
    assert_eq!(llm.call_count(), 0, "空候选不应发起 LLM 调用");

    let _ = std::fs::remove_dir_all(dir);
}

/// 超出候选上限的编号（截断后不可见）→ Failed。
#[tokio::test]
async fn candidate_beyond_limit_is_rejected() {
    let mut many = candidates();
    for index in 3..7 {
        many.push(JudgeCandidate {
            id: format!("c{index}"),
            source: "event".to_string(),
            anchor: Some(format!("候选 {index}")),
            valence: 0.5,
        });
    }
    let reply = r#"{"speak": true, "candidate_id": "c6", "angle": "关心", "tone": "温和", "reason_bucket": "speak"}"#;
    let (engine, _llm, dir) = engine_with_replies("judge-limit", &[reply]).await;

    assert!(
        matches!(
            decide(&engine, &many, &signals()).await,
            JudgeOutcome::Failed
        ),
        "超出候选上限的编号不应被接受"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 提示词组装：情境行 / 来源与效价标注 / 无锚点占位 / 无历史回退。
#[test]
fn user_message_format_contract() {
    let mut all_sources = candidates();
    all_sources.push(JudgeCandidate {
        id: "c3".to_string(),
        source: "time_node".to_string(),
        anchor: Some("认识一周年".to_string()),
        valence: 0.2,
    });
    all_sources.push(JudgeCandidate {
        id: "c4".to_string(),
        source: "rule".to_string(),
        anchor: Some("用户提到想早睡".to_string()),
        valence: 0.0,
    });
    all_sources.push(JudgeCandidate {
        id: "c5".to_string(),
        source: "自定义来源".to_string(),
        anchor: Some("未知来源摘要".to_string()),
        valence: -0.3,
    });

    let message = build_user_message(&all_sources, &signals());
    assert!(message.contains("## 当前情境"));
    assert!(message.contains("- 本地时间：14 点"));
    assert!(message.contains("- 用户活跃权重：0.72（越高表示用户通常在这个时段活跃）"));
    assert!(message.contains("- 距上次对话：6.5 小时"));
    assert!(message.contains("- 连续未回应主动消息：0 次"));
    assert!(message.contains("- 今日已主动投递：1 次"));
    assert!(message.contains("## 候选话题"));
    assert!(message.contains("c0 [高显著事件][正向] 陶艺展：周末逛了陶艺展，用户很喜欢"));
    assert!(message.contains("c1 [未了结事件][负向] 与同事的误会还没说开"));
    assert!(message.contains("c2 [轻触达][中性] （无具体话题，可作轻量问候）"));
    assert!(message.contains("c3 [时间节点][正向] 认识一周年"));
    assert!(message.contains("c4 [行为规则][中性] 用户提到想早睡"));
    assert!(message.contains("c5 [自定义来源][负向] 未知来源摘要"));

    // 无历史：距上次对话回退为「无历史」
    let mut cold = signals();
    cold.hours_since_last_chat = None;
    let message = build_user_message(&all_sources, &cold);
    assert!(message.contains("- 距上次对话：无历史"));
}
