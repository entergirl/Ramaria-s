//! crates/ramaria-service/src/chat/tests_eval.rs - Ramaria 评测生成入口单元测试
//!
//! 设计特点:
//! - 由 chat 模块以 `#[cfg(test)] mod tests_eval;` 收纳：覆盖层掩码 / 查询时钟 / 访问时间开关
//! - 使用真实 SQLite 临时库 + mock LLM：断言以召回观测、提示词与落库状态为准
//! - 产品等价：`chat_send` 与全开掩码的评测入口在同一夹具下回复与落库条数一致
//! - 只断言行为，不复制底层召回规则（分层过滤 / 衰减细节在 memory 层测试覆盖）
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::tests::REPLY;
use crate::test_support::{
    CountingEmbedding, MockLlm, engine_with_shared_llm, engine_with_shared_provider, seed_l1,
    seed_persona,
};
use crate::types::{
    CHANNEL_MCP, ChatSendRequest, DEFAULT_PERSONA_UID, EvalChatOptions, EvalInjectionMask,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmUsage, StoreCrud};
use ramaria_core::types::{MemoryEvent, PersonalityTrait, TraitLayer, TraitSource, now_ms};
use ramaria_storage::SqliteStorage;
use ramaria_testkit::MockUsageLlm;
use std::sync::Arc;

/// 一天毫秒数（夹具时间刻度）。
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// 构造生成请求（默认人格、通道 mcp）。
fn request(message: &str, conversation_id: &str) -> ChatSendRequest {
    ChatSendRequest {
        message: message.to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        channel: CHANNEL_MCP.to_string(),
        conversation_id: Some(conversation_id.to_string()),
        session_id: None,
    }
}

/// 造一条 L2 事件（标题 / 摘要 / 关键词都命中查询「工作压力」，share 对角色类型可见）。
async fn seed_event(storage: &SqliteStorage, persona: &str, title: &str) {
    let at = now_ms() - DAY_MS;
    let mut event = MemoryEvent::new(
        persona.to_string(),
        title.to_string(),
        format!("{title} 的详细经过"),
        at,
        at,
    );
    event.keywords = Some("工作压力".to_string());
    event.share = 1.0;
    event.salience = 0.8;
    event.created_at = at;
    storage.save_event(&event).await.expect("写入事件应成功");
}

/// 造一条命中同一查询的 L1 摘要，返回其 id。
async fn seed_hit_l1(storage: &SqliteStorage, persona: &str) -> uuid::Uuid {
    seed_l1(
        storage,
        persona,
        "用户最近工作压力很大，常常加班到深夜",
        Some("工作压力,加班"),
        now_ms() - DAY_MS,
    )
    .await
}

/// 读取 L1 行的访问时间（None = 从未被访问）。
async fn l1_access_time(storage: &SqliteStorage, id: uuid::Uuid) -> Option<i64> {
    storage
        .get_memory_l1(id)
        .await
        .expect("读取 L1 应成功")
        .expect("L1 行应存在")
        .last_accessed_at
}

// =========================================================
// 层掩码
// =========================================================

/// memory_only：观测含记忆子层与轨迹，提示词不含知识块 / 行为块。
#[tokio::test]
async fn memory_only_mask_observes_memory_layers_only() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-memory-only",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;
    seed_event(&storage, DEFAULT_PERSONA_UID, "工作压力事件").await;

    let outcome = engine
        .chat_answer_eval(
            request("工作压力", "eval-memory-only"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");

    assert_eq!(outcome.reply, REPLY);
    assert!(outcome.usage.is_none(), "mock 未上报用量时结果应为空用量");
    let layers: Vec<&str> = outcome
        .recall
        .items
        .iter()
        .map(|item| item.layer.as_str())
        .collect();
    assert!(layers.contains(&"l1"), "应返回 L1 条目: {layers:?}");
    assert!(layers.contains(&"l2"), "应返回 L2 条目: {layers:?}");
    assert!(
        layers.iter().all(|layer| *layer == "l1" || *layer == "l2"),
        "只应含记忆子层: {layers:?}"
    );
    assert!(outcome.recall.context_chars > 0, "应产出记忆上下文");
    assert!(outcome.recall.injected_tokens > 0, "应记录注入 token 估算");
    assert!(
        outcome.recall.trace.budget.max_memories > 0,
        "轨迹应含条数预算"
    );
    assert!(
        outcome
            .recall
            .trace
            .retrieval
            .channels
            .iter()
            .any(|channel| channel.active),
        "轨迹应含通道候选"
    );

    let prompt = llm.last_request().expect("应记录 LLM 请求").system_prompt;
    assert!(!prompt.contains("# 知识"), "知识层关闭时不应出现知识块");
    assert!(!prompt.contains("# 行为"), "行为层关闭时不应出现行为块");

    let _ = std::fs::remove_dir_all(&dir);
}

/// L2 掩码：关闭后条目不含 l2，L1 仍命中；开启时同一夹具出现 l2。
#[tokio::test]
async fn l2_mask_controls_event_items() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-l2-mask",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;
    seed_event(&storage, DEFAULT_PERSONA_UID, "工作压力事件").await;

    let with_l2 = engine
        .chat_answer_eval(
            request("工作压力", "eval-l2-on"),
            EvalChatOptions {
                mask: EvalInjectionMask {
                    l2: true,
                    ..EvalInjectionMask::memory_only()
                },
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");

    let without_l2 = engine
        .chat_answer_eval(
            request("工作压力", "eval-l2-off"),
            EvalChatOptions {
                mask: EvalInjectionMask {
                    l2: false,
                    ..EvalInjectionMask::memory_only()
                },
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");

    assert!(
        with_l2.recall.items.iter().any(|item| item.layer == "l2"),
        "开启 L2 时应出现事件条目: {:?}",
        with_l2.recall.items
    );
    assert!(
        without_l2
            .recall
            .items
            .iter()
            .all(|item| item.layer != "l2"),
        "关闭 L2 后不应出现事件条目: {:?}",
        without_l2.recall.items
    );
    assert!(
        without_l2
            .recall
            .items
            .iter()
            .any(|item| item.layer == "l1"),
        "关闭 L2 不应影响 L1: {:?}",
        without_l2.recall.items
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// L3 掩码：关闭后提示词不含性格标签文本，开启时包含（同一夹具）。
#[tokio::test]
async fn l3_mask_controls_trait_block() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-l3-mask",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    storage
        .save_trait(&PersonalityTrait::new(
            DEFAULT_PERSONA_UID.to_string(),
            TraitLayer::Primary,
            "慢热".to_string(),
            "只在被问起时才说的心事".to_string(),
            TraitSource::Manual,
            1,
        ))
        .await
        .expect("写入性格标签应成功");

    engine
        .chat_answer_eval(
            request("你好", "eval-l3-on"),
            EvalChatOptions {
                mask: EvalInjectionMask {
                    l3: true,
                    ..EvalInjectionMask::memory_only()
                },
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    let with_l3 = llm.last_request().expect("应记录 LLM 请求").system_prompt;
    assert!(
        with_l3.contains("只在被问起时才说的心事"),
        "开启 L3 时应注入性格标签"
    );

    engine
        .chat_answer_eval(
            request("你好", "eval-l3-off"),
            EvalChatOptions {
                mask: EvalInjectionMask {
                    l3: false,
                    ..EvalInjectionMask::memory_only()
                },
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    let without_l3 = llm.last_request().expect("应记录 LLM 请求").system_prompt;
    assert!(
        !without_l3.contains("只在被问起时才说的心事"),
        "关闭 L3 后不应注入性格标签"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 访问时间开关与产品等价
// =========================================================

/// touch 开关：默认不写访问时间，显式开启后按查询时钟刷新。
#[tokio::test]
async fn eval_touch_switch_controls_l1_access_time() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-touch",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let l1_id = seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;
    assert_eq!(
        l1_access_time(&storage, l1_id).await,
        None,
        "新建 L1 不应有访问时间"
    );

    engine
        .chat_answer_eval(
            request("工作压力", "eval-touch-off"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    assert_eq!(
        l1_access_time(&storage, l1_id).await,
        None,
        "touch=false（默认）不应刷新访问时间"
    );

    engine
        .chat_answer_eval(
            request("工作压力", "eval-touch-on"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                touch: true,
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    assert!(
        l1_access_time(&storage, l1_id).await.is_some(),
        "touch=true 应刷新访问时间"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 产品等价：`chat_send` 与全开掩码 / 刷新访问时间的评测入口回复与落库条数一致。
#[tokio::test]
async fn chat_send_matches_eval_with_all_layers() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-parity",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;
    seed_event(&storage, DEFAULT_PERSONA_UID, "工作压力事件").await;

    let product = engine
        .chat_send(request("工作压力", "product"))
        .await
        .expect("产品生成应成功");
    let evaluated = engine
        .chat_answer_eval(
            request("工作压力", "eval"),
            EvalChatOptions {
                mask: EvalInjectionMask::all(),
                touch: true,
                as_of_ms: None,
                max_chars: None,
                preset_context: None,
            },
        )
        .await
        .expect("评测生成应成功");

    assert_eq!(product.reply, evaluated.reply, "回复应一致");
    assert_eq!(product.chars, evaluated.chars, "字符数应一致");
    let product_messages = storage
        .list_messages(product.session_id)
        .await
        .expect("读取产品会话消息成功");
    let eval_messages = storage
        .list_messages(evaluated.session_id)
        .await
        .expect("读取评测会话消息成功");
    assert_eq!(
        product_messages.len(),
        eval_messages.len(),
        "落库消息数应一致"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 生成用量
// =========================================================

/// 评测生成把本次调用的用量放进结果；用量随返回值流动，不读共享槽位。
#[tokio::test]
async fn eval_outcome_carries_per_call_usage() {
    let usage = LlmUsage {
        prompt_tokens: 500,
        completion_tokens: 20,
        cache_hit_tokens: 400,
        cache_miss_tokens: 100,
    };
    let mock = Arc::new(MockUsageLlm::new(REPLY, usage));
    let provider: Arc<dyn LlmProvider> = mock.clone();
    let (engine, storage, dir) =
        engine_with_shared_provider("eval-usage", provider, RamariaConfig::default()).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .chat_answer_eval(
            request("工作压力", "eval-usage"),
            EvalChatOptions::default(),
        )
        .await
        .expect("评测生成应成功");

    assert_eq!(outcome.reply, REPLY);
    assert_eq!(outcome.usage, Some(usage), "用量应随本次调用返回");
    assert_eq!(mock.calls(), 1, "评测生成只调用一次模型");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 预置上下文（full-context 对照）
// =========================================================

/// 预置上下文：跳过召回直接注入，观测体量按预置文本计算。
///
/// 说明:
/// - 库内预置可命中的 L1：短路生效时它不应进入上下文；
/// - 预置文本经 `ChatRequest.memory_context` 注入（provider 侧拼进 system 消息）。
#[tokio::test]
async fn preset_context_skips_recall_and_injects_text() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-preset-context",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;

    let preset = "用户此前在杭州工作，因加班与失眠常向朋友倾诉。";
    let outcome = engine
        .chat_answer_eval(
            request("工作压力", "eval-preset"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                preset_context: Some(preset.to_string()),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");

    assert_eq!(outcome.reply, REPLY);
    assert!(
        outcome.recall.items.is_empty(),
        "预置上下文不应产出召回条目: {:?}",
        outcome.recall.items
    );
    assert_eq!(
        outcome.recall.context_chars,
        preset.chars().count(),
        "上下文体量应按预置文本计算"
    );
    assert!(
        outcome.recall.injected_tokens > 0,
        "应按预置文本估算注入 token"
    );
    assert_eq!(outcome.recall.utt_chars, 0, "预置上下文不注入原文片段");

    let sent = llm.last_request().expect("应记录 LLM 请求");
    // provider 侧把 `memory_context` 以 `<memory_context>` 追加进 system 消息，
    // 故断言落在请求的记忆上下文字段上。
    assert_eq!(sent.memory_context.as_deref(), Some(preset));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预置上下文：不做查询嵌入（同夹具对照先证明常规召回会产生嵌入调用）。
#[tokio::test]
async fn preset_context_skips_embedding_calls() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let counter = Arc::new(CountingEmbedding::new());
    let embedding: Arc<dyn EmbeddingProvider> = Arc::clone(&counter) as Arc<dyn EmbeddingProvider>;
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-preset-embedding",
        Arc::clone(&llm),
        RamariaConfig::default(),
        Some(embedding),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;

    // 对照：同一夹具不传预置上下文，召回会调用查询嵌入
    engine
        .chat_answer_eval(
            request("工作压力", "eval-preset-control"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    let control_calls = counter.embed_calls() + counter.batch_calls();
    assert!(control_calls > 0, "对照条件应产生检索嵌入调用");

    let outcome = engine
        .chat_answer_eval(
            request("工作压力", "eval-preset-skip"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                preset_context: Some("预置的全量历史摘要。".to_string()),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");

    assert_eq!(
        counter.embed_calls() + counter.batch_calls(),
        control_calls,
        "预置上下文不应触发嵌入调用"
    );
    assert!(
        outcome.recall.trace.retrieval.channels.is_empty(),
        "预置上下文不应记录通道候选"
    );
    assert!(outcome.recall.items.is_empty(), "预置上下文不应产出条目");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 全空白预置上下文：按无上下文处理（不注入、不检索）。
#[tokio::test]
async fn preset_context_blank_is_no_context() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-preset-blank",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .chat_answer_eval(
            request("工作压力", "eval-preset-blank"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                preset_context: Some("  ".to_string()),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");

    assert_eq!(outcome.recall.context_chars, 0, "空白预置应视为无上下文");
    assert_eq!(outcome.recall.injected_tokens, 0);
    assert_eq!(outcome.recall.utt_chars, 0);
    assert!(outcome.recall.items.is_empty(), "空白预置不应回退到检索");

    let sent = llm.last_request().expect("应记录 LLM 请求");
    assert!(sent.memory_context.is_none(), "空白预置不应注入记忆上下文");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 字符预算：记忆上下文按预算截断，原文片段只用剩余额度。
#[tokio::test]
async fn max_chars_caps_memory_and_utt_injection() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "eval-budget",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_hit_l1(&storage, DEFAULT_PERSONA_UID).await;
    seed_event(&storage, DEFAULT_PERSONA_UID, "工作压力事件").await;

    let capped = engine
        .chat_answer_eval(
            request("工作压力", "eval-budget-capped"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                max_chars: Some(10),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    assert!(
        capped.recall.context_chars + capped.recall.utt_chars <= 10,
        "注入总量不应超过预算: {} + {}",
        capped.recall.context_chars,
        capped.recall.utt_chars
    );
    let sent = llm.last_request().expect("应记录 LLM 请求");
    let injected = sent.memory_context.as_deref().unwrap_or("");
    assert!(injected.chars().count() <= 10, "注入文本不应超过预算");

    let uncapped = engine
        .chat_answer_eval(
            request("工作压力", "eval-budget-uncapped"),
            EvalChatOptions {
                mask: EvalInjectionMask::memory_only(),
                ..EvalChatOptions::default()
            },
        )
        .await
        .expect("评测生成应成功");
    assert!(
        uncapped.recall.context_chars > 10,
        "不设预算时应保留完整上下文"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
