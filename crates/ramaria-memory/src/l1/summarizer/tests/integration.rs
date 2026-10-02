//! crates/ramaria-memory/src/l1/summarizer/tests/integration.rs - summarize_session 集成
//!
//! 设计特点:
//! - 由 父测试模块 以 mod integration; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// summarize_session 集成测试
// =========================================================

/// 测试 summarize_session 完整流程：消息→格式化→mock LLM→解析→校验→存储。
#[tokio::test]
async fn summarize_session_integration_basic() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();

    // 准备 mock 存储：3 条对话消息
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![
            make_msg(session_id, MessageRole::User, "今天天气真不错"),
            make_msg(session_id, MessageRole::Assistant, "是啊，适合出去走走"),
            make_msg(session_id, MessageRole::User, "不过最近工作有点累"),
        ],
    );
    storage.set_keywords(vec!["天气".into(), "工作".into(), "疲惫".into()]);

    // 准备 mock LLM：返回有效 JSON（使用 serde_json 构造确保格式正确）
    let llm = MockLlmProvider::new("test-model");
    let response_json = serde_json::json!({
        "summary": "用户和助手聊了天气和最近的工作状态",
        "keywords": "天气,工作压力,日常闲聊",
        "time_period": "上午",
        "atmosphere": "轻松闲聊",
        "valence": 0.3,
        "salience": 0.5,
        "evidence_notes": ["用户说天气不错", "用户提到最近工作有点累"]
    });
    llm.set_response(response_json.to_string());

    let config = L1SummarizerConfig {
        persona_uid: Some("test-persona".into()),
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);

    let result = summarizer.summarize_session(session_id).await;
    assert!(
        result.is_ok(),
        "summarize_session 应成功: {:?}",
        result.err()
    );

    let l1 = result.unwrap();
    assert_eq!(l1.persona_uid, Some("test-persona".into()));
    assert!(l1.summary.contains("天气"), "摘要应包含天气相关内容");
    assert!(
        !l1.evidence_notes.as_ref().unwrap().is_empty(),
        "evidence_notes 不应为空"
    );

    // 验证存储写入
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "应保存 1 条 L1 记录");
    assert!(storage.keyword_count() >= 1, "应写入至少 1 个关键词");
}

/// 测试空消息 session 返回错误。
#[tokio::test]
async fn summarize_session_empty_messages_errors() {
    use crate::l1::mock::{MockLlmProvider, MockStorage};
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    let llm = MockLlmProvider::new("test-model");

    let config = L1SummarizerConfig {
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(session_id).await;
    assert!(result.is_err(), "空消息 session 应返回错误");
}

/// 测试 LLM 返回 JSON 中 evidence_notes 缺失时降级。
#[tokio::test]
async fn summarize_session_missing_evidence_notes_degrades() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![make_msg(session_id, MessageRole::User, "测试消息")],
    );

    let llm = MockLlmProvider::new("test-model");
    // 不包含 evidence_notes 字段
    let response_json = serde_json::json!({
        "summary": "一条测试消息",
        "keywords": "测试",
        "time_period": "未知",
        "atmosphere": "中性",
        "valence": 0.0,
        "salience": 0.3
    });
    llm.set_response(response_json.to_string());

    let config = L1SummarizerConfig {
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(session_id).await;
    assert!(
        result.is_ok(),
        "缺少 evidence_notes 不应阻塞流程: {:?}",
        result.err()
    );

    let l1 = result.unwrap();
    // evidence_notes 缺失时降级为空数组
    let notes = l1.evidence_notes.expect("evidence_notes 应为 Some");
    assert!(notes.is_empty(), "缺失 evidence_notes 时应降级为空数组");
}
