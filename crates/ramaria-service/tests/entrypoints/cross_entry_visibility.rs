//! crates/ramaria-service/tests/entrypoints/cross_entry_visibility.rs - 跨入口回流可见用例
//!
//! 设计特点:
//! - MCP 形态引擎执行外部对话回流写入（`channel` / 外部标识落库），桌面形态引擎
//!   在独立连接池上召回同一摘要：回流内容经另一入口可见（同库）
//! - `finalize = true` 触发封存与摘要生成；channel / external_ref 按落库值断言
//! - 召回断言精确到写入会话对应的 L1 标识，不只检查上下文文本

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StoreCrud, StoreInfrastructure};
use ramaria_core::types::BackendConfig;
use ramaria_service::types::{ChatRole, ChatTurn, IngestRequest, RecallLayer, RecallRequest};
use ramaria_service::{CHANNEL_MCP, RecallPolicy, default_seal_hooks, full_seal_hooks};

use crate::support::{DeterministicEmbedding, ScriptedLlm, TestDb};

/// 脚本回复：回流会话摘要（含检索特征词"杭州"）。
const INGEST_L1_JSON: &str = r#"{
  "summary": "用户下周三要去杭州出差，想顺便尝尝当地的小馆子。",
  "keywords": "杭州,出差",
  "time_period": "下周",
  "atmosphere": "期待",
  "valence": 0.4,
  "salience": 0.7,
  "situation_strength": 3
}"#;

/// MCP 侧回流写入 → 另一入口（桌面形态）召回可见，且通道 / 外部标识落库正确。
#[tokio::test]
async fn mcp_ingest_flow_is_recallable_from_another_entry() {
    const PERSONA: &str = "char-entry-visibility";
    const CONVERSATION_ID: &str = "cb-entry-1";

    let db = TestDb::new("entry-visibility");
    let config = RamariaConfig::default();
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::reply(INGEST_L1_JSON));

    // ---- MCP 形态：轻量封存钩子链 + 保守召回策略（与宿主装配口径一致） ----
    let (mcp, storage) = db
        .open_engine(
            Arc::clone(&llm),
            Some(Arc::clone(&embedding)),
            config.clone(),
        )
        .await
        .expect("MCP 形态引擎应可装配");
    mcp.set_seal_hooks(default_seal_hooks(mcp.as_ref()));
    mcp.set_recall_policy(RecallPolicy::default());

    // ---- 桌面形态：完整封存钩子链（独立连接池，同库） ----
    let (desktop, _) = db
        .open_engine(
            Arc::clone(&llm),
            Some(Arc::clone(&embedding)),
            config.clone(),
        )
        .await
        .expect("桌面形态引擎应可装配");
    desktop.set_seal_hooks(full_seal_hooks(desktop.as_ref()));

    // ---- 种子与就绪 ----
    crate::support::fixtures::seed_persona(storage.as_ref(), PERSONA)
        .await
        .expect("种子人格应写入成功");
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("后端配置应写入成功");
    mcp.refresh_setup_state()
        .await
        .expect("MCP 侧刷新状态应成功");
    desktop
        .refresh_setup_state()
        .await
        .expect("桌面侧刷新状态应成功");

    // ---- MCP 侧回流写入（finalize 触发封存与摘要生成） ----
    let outcome = mcp
        .ingest(IngestRequest {
            messages: vec![
                ChatTurn {
                    role: ChatRole::User,
                    content: "我下周三要去杭州出差".to_string(),
                },
                ChatTurn {
                    role: ChatRole::Assistant,
                    content: "好的，我记下你的行程安排".to_string(),
                },
                ChatTurn {
                    role: ChatRole::User,
                    content: "还想顺便尝尝当地的小馆子".to_string(),
                },
            ],
            persona: Some(PERSONA.to_string()),
            conversation_id: Some(CONVERSATION_ID.to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        })
        .await
        .expect("回流写入应成功");
    assert_eq!(
        outcome.written, 3,
        "三条消息应全部写入（session={}）",
        outcome.session_id
    );
    assert!(
        outcome.finalized,
        "finalize=true 应完成封存与摘要生成（session={}）",
        outcome.session_id
    );

    // ---- 通道与外部标识落库正确 ----
    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("读取回流会话应成功")
        .expect("回流会话应存在");
    assert_eq!(
        session.channel, CHANNEL_MCP,
        "回流会话通道应为 mcp（session={}）",
        outcome.session_id
    );
    assert_eq!(
        session.external_ref.as_deref(),
        Some(CONVERSATION_ID),
        "回流会话外部标识应为 {CONVERSATION_ID}（session={}）",
        outcome.session_id
    );

    let l1_list = storage
        .list_memory_l1(outcome.session_id)
        .await
        .expect("读取回流会话 L1 应成功");
    assert_eq!(
        l1_list.len(),
        1,
        "finalize 封存后应恰好一份 L1（session={}）",
        outcome.session_id
    );
    let expected_id = format!("L1:{}", l1_list[0].id);

    // ---- 另一入口（桌面形态，独立连接池）召回：同库可见 ----
    let result = desktop
        .recall(RecallRequest {
            query: Some("杭州 出差".to_string()),
            persona: Some(PERSONA.to_string()),
            include: Some(vec![RecallLayer::L1]),
            max_items: Some(10),
            ..RecallRequest::default()
        })
        .await
        .expect("桌面侧召回应成功");
    assert!(
        result
            .items
            .iter()
            .any(|item| item.layer == RecallLayer::L1 && item.id == expected_id),
        "回流内容应对另一入口可见（session={}，期望 id={expected_id}）: {:?}",
        outcome.session_id,
        result.items
    );

    db.cleanup().await;
}
