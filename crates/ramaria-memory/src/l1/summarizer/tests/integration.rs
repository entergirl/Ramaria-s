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
        fanout_others: false,
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
        fanout_others: false,
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
        fanout_others: false,
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

/// 主动消息口径：L1 摘要的 LLM 输入包含主动消息正文（零来源过滤）。
#[tokio::test]
async fn summarize_session_includes_proactive_message_in_llm_input() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();

    // 主动生成的助手消息（persona_uid=目标、is_proactive=true）
    let mut proactive = make_msg(
        session_id,
        MessageRole::Assistant,
        "主动问候：最近工作还顺利吗",
    );
    proactive.persona_uid = Some("test-persona".into());
    proactive.is_proactive = true;

    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![
            make_msg(session_id, MessageRole::User, "在的，最近还行"),
            proactive,
        ],
    );

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("含主动消息的摘要", None));

    let config = L1SummarizerConfig {
        persona_uid: Some("test-persona".into()),
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        fanout_others: false,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer
        .summarize_session(session_id)
        .await
        .expect("含主动消息的会话摘要应成功");

    // 锁定：主动消息正文进入 LLM 输入（L1 路径不按来源过滤）
    let request = llm.last_request().expect("应记录 LLM 请求");
    assert!(
        request.user_message.contains("主动问候：最近工作还顺利吗"),
        "L1 输入应包含主动消息正文，实际: {}",
        request.user_message
    );
    assert!(request.user_message.contains("在的，最近还行"));
}

// =========================================================
// 关键词写回（词池快照三分支）
// =========================================================

/// 构造词池行（供写入侧 pending 判定用例）。
fn pool_row(
    rowid: i64,
    keyword: &str,
    use_count: i64,
    alias_status: Option<&str>,
    canonical_id: Option<i64>,
) -> ramaria_core::keyword::KeywordPoolRow {
    ramaria_core::keyword::KeywordPoolRow {
        rowid,
        keyword: keyword.to_string(),
        use_count,
        created_at: 1_700_000_000_000,
        alias_status: alias_status.map(str::to_string),
        canonical_id,
        canonical_keyword: None,
    }
}

/// 构造 keywords 字段可控的 L1 摘要回复 JSON。
fn l1_keywords_reply(keywords: &str) -> String {
    serde_json::json!({
        "summary": "关键词写回测试摘要",
        "keywords": keywords,
        "time_period": "上午",
        "atmosphere": "平静",
        "valence": 0.0,
        "salience": 0.5,
        "evidence_notes": []
    })
    .to_string()
}

/// 三分支：命中词池 → 递增；未命中但相似 → pending 指向规范词；未命中不相似 → 规范词。
#[tokio::test]
async fn write_back_keywords_branches_by_pool_hit_and_similarity() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![
            make_msg(session_id, MessageRole::User, "最近在准备考研数学"),
            make_msg(session_id, MessageRole::Assistant, "数学要多做题"),
        ],
    );
    // 词池快照：规范词「数学」（rowid=7、use_count=10）
    storage.set_pool_rows(vec![pool_row(7, "数学", 10, None, None)]);

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(l1_keywords_reply("数学,考研数学,健身计划"));

    let config = L1SummarizerConfig {
        persona_uid: Some("test-persona".into()),
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        fanout_others: false,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer
        .summarize_session(session_id)
        .await
        .expect("摘要应成功");

    // 相似未命中词以 pending 登记，携带规范词 rowid
    assert_eq!(
        storage.pending_writes(),
        vec![("考研数学".to_string(), 7)],
        "相似未命中词应登记 pending 并指向规范词"
    );

    // 命中词与不相似词走 upsert_keyword；pending 词不作为规范词写入
    let keywords = storage.list_keywords().await.expect("读取关键词应成功");
    assert!(
        keywords.contains(&"数学".to_string()),
        "命中词池的词应走递增路径"
    );
    assert!(
        keywords.contains(&"健身计划".to_string()),
        "不相似词应写为规范词"
    );
    assert!(
        !keywords.contains(&"考研数学".to_string()),
        "pending 词不得同时作为规范词写入"
    );
}

/// pending 登记幂等：二次写回同一相似词（快照仍未命中）再次登记返回已存在，不重复记录。
#[tokio::test]
async fn write_back_keywords_repeated_pending_write_is_idempotent() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![make_msg(session_id, MessageRole::User, "继续准备考研数学")],
    );
    storage.set_pool_rows(vec![pool_row(7, "数学", 10, None, None)]);

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        l1_keywords_reply("考研数学"),
        l1_keywords_reply("考研数学"),
    ]);

    let config = L1SummarizerConfig {
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        fanout_others: false,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer
        .summarize_session(session_id)
        .await
        .expect("首次摘要应成功");
    summarizer
        .summarize_session(session_id)
        .await
        .expect("二次摘要应成功");

    // 两次登记同一别名：第二次返回已存在（幂等），记录不重复
    assert_eq!(
        storage.pending_writes(),
        vec![("考研数学".to_string(), 7)],
        "重复登记不得产生第二条记录"
    );
}

/// 空词池快照（读取失败降级口径）：全部按规范词写回，无 pending 登记。
#[tokio::test]
async fn write_back_keywords_empty_pool_snapshot_writes_canonical() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![make_msg(
            session_id,
            MessageRole::User,
            "最近在准备考研数学",
        )],
    );
    storage.set_pool_rows(Vec::new());

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(l1_keywords_reply("考研数学"));

    let config = L1SummarizerConfig {
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        fanout_others: false,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer
        .summarize_session(session_id)
        .await
        .expect("摘要应成功");

    assert!(
        storage.pending_writes().is_empty(),
        "空词池快照下不应产生 pending 登记"
    );
    let keywords = storage.list_keywords().await.expect("读取关键词应成功");
    assert!(
        keywords.contains(&"考研数学".to_string()),
        "空词池快照下降级为直接写规范词"
    );
}

/// 开销口径：词池快照每会话读取一次——与关键词数量无关（不逐词查询）。
#[tokio::test]
async fn pool_snapshot_read_once_per_session() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![
            make_msg(session_id, MessageRole::User, "最近在准备考研"),
            make_msg(session_id, MessageRole::Assistant, "复习要循序渐进"),
        ],
    );
    storage.set_pool_rows(vec![pool_row(1, "考研", 3, None, None)]);

    let llm = MockLlmProvider::new("test-model");
    // 5 个关键词：若退化为逐词查询则读取次数会远超一次
    llm.set_response(l1_keywords_reply("考研,复习,数学,英语,政治"));

    let config = L1SummarizerConfig {
        persona_uid: Some("test-persona".into()),
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        fanout_others: false,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer
        .summarize_session(session_id)
        .await
        .expect("摘要应成功");

    assert_eq!(
        storage.pool_read_count(),
        1,
        "词池快照应恰好读取一次（与关键词数量无关）"
    );
}
