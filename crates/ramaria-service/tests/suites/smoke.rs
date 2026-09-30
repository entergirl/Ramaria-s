//! crates/ramaria-service/tests/suites/smoke.rs - 装配、生成与封存链路的冒烟用例
//!
//! 设计特点:
//! - 最小闭环：内存存储 + 固定回复 mock LLM → 生成 → 落库 → 封存 → L1
//! - 同时覆盖事件流形状（Delta… → Done）与错误路径（LLM 失败不留半条）
//! - 作为集成测试基建的自检用例，基建变更时最先失败

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{MessageRole, Persona, PersonaKind};

use crate::support::engine_env::{
    build_engine, kinds, mark_ready, seal_full, send_stream, stream_request, try_send_stream,
};
use crate::support::mock_backend::{MockLlm, MockStorage};

/// 固定助手回复（断言直接用）。
const REPLY: &str = "好的，我记住了。";

/// 符合 L1 摘要 JSON 契约的固定回复（封存链路解析该结构落库）。
const L1_JSON_REPLY: &str = r#"{
  "summary": "用户最近工作压力很大，常加班到深夜。",
  "keywords": "工作压力,加班",
  "time_period": "夜间",
  "atmosphere": "疲惫",
  "valence": -0.4,
  "salience": 0.8,
  "situation_strength": 4
}"#;

/// 构造带一个角色人格的空存储。
fn storage_with_persona() -> Arc<MockStorage> {
    let storage = Arc::new(MockStorage::new());
    storage.add_persona(Persona::new(
        "char-0001".to_string(),
        "小菌".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    ));
    storage
}

/// 生成路径：一轮消息落库两条（用户 + 助手），事件流以 Done 收尾。
#[tokio::test]
async fn generate_persists_both_messages_and_emits_done() {
    let storage = storage_with_persona();
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::new(MockLlm::new(REPLY)),
        RamariaConfig::default(),
    );
    mark_ready(&engine).await.expect("就绪推进应成功");

    let outcome = send_stream(&engine, "你好", Some("char-0001"), None).await;
    assert!(outcome.error.is_none(), "生成路径不应有错误事件");
    assert!(outcome.done, "事件流应以 Done 收尾");
    assert_eq!(outcome.text, REPLY, "增量拼接应等于 mock 回复全文");
    assert_eq!(
        kinds(&outcome),
        vec!["delta"; REPLY.chars().count()]
            .into_iter()
            .chain(std::iter::once("done"))
            .collect::<Vec<_>>(),
        "事件序列应为 逐字 Delta → Done"
    );

    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("读取会话消息应成功");
    assert_eq!(messages.len(), 2, "一轮对话应落库用户消息与助手回复");
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].content, "你好");
    assert_eq!(messages[1].content, REPLY);
}

/// 状态门禁：未就绪的交互式生成被拒绝，且不产生会话。
#[tokio::test]
async fn interactive_generate_rejected_before_ready() {
    let storage = storage_with_persona();
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::new(MockLlm::new(REPLY)),
        RamariaConfig::default(),
    );

    let err = try_send_stream(&engine, stream_request("你好", Some("char-0001"), None))
        .await
        .expect_err("未就绪状态应拒绝交互式生成");
    assert_eq!(err.category(), "validation", "应为业务校验类错误");

    let sessions = storage.list_sessions().await.expect("读取会话列表应成功");
    assert!(sessions.is_empty(), "被拒的生成不应创建会话");
}

/// LLM 失败：流内以 Error 收尾，不产生任何落库痕迹（失败不留半条）。
#[tokio::test]
async fn llm_failure_leaves_no_partial_write() {
    let storage = storage_with_persona();
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::new(MockLlm::failing("mock 网络错误")),
        RamariaConfig::default(),
    );
    mark_ready(&engine).await.expect("就绪推进应成功");

    let outcome = send_stream(&engine, "你好", Some("char-0001"), None).await;
    assert!(outcome.error.is_some(), "LLM 失败应经事件流传出错误");
    assert!(!outcome.done, "错误路径不应发 Done");

    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("读取会话消息应成功");
    assert!(messages.is_empty(), "失败路径不应留下用户消息或助手回复");
}

/// 封存路径：会话关闭并生成 L1 摘要。
///
/// 说明:
/// - mock LLM 为固定回复，故本轮助手回复与 L1 摘要共用同一段 JSON 文本；
///   断言只取"会话关闭 + L1 落库"两个可观测结果。
#[tokio::test]
async fn seal_closes_session_and_generates_l1() {
    let storage = storage_with_persona();
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::new(MockLlm::new(L1_JSON_REPLY)),
        RamariaConfig::default(),
    );
    mark_ready(&engine).await.expect("就绪推进应成功");

    let outcome = send_stream(&engine, "你好", Some("char-0001"), None).await;
    let seal = seal_full(&engine, outcome.session_id)
        .await
        .expect("封存应成功");
    assert!(seal.sealed, "本次调用应抢占并完成封存");
    assert!(seal.l1_count >= 1, "短会话应生成至少一条 L1");

    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("读取会话应成功")
        .expect("会话应存在");
    assert!(session.ended_at.is_some(), "封存后会话应已关闭");
}
