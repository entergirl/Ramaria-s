//! crates/ramaria-service/tests/suites/dedup_knowledge.rs - 记忆、知识与表达的跨层去重用例
//!
//! 设计特点:
//! - 覆盖断言层关闭时事实由检索摘要覆盖的兜底关系
//! - 覆盖检索命中时知识卡不重复注入、检索未命中时知识卡兜底注入
//! - 覆盖去重链后说话风格不重复注入
//! - 断言经 mock LLM 记录的生成请求段落，只使用确定性 fixture 文本

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{LlmProvider, StoreCrud};
use ramaria_core::types::{
    FactSource, FactStatus, FactTier, MemoryL1, Persona, PersonaFact, PersonaKind, ProfileField,
};
use ramaria_service::Engine;
use uuid::Uuid;

use crate::support::engine_env::{build_engine, mark_ready, send_stream};
use crate::support::mock_backend::{MockLlm, MockStorage};

// =========================================================
// 测试辅助
// =========================================================

/// 构造并推进到"对话可用"状态的引擎（MockStorage + MockLlm，无嵌入）。
async fn build_ready_engine(
    storage: &Arc<MockStorage>,
    llm: &Arc<MockLlm>,
    config: RamariaConfig,
) -> Arc<Engine> {
    let engine = build_engine(
        Arc::clone(storage),
        Arc::clone(llm) as Arc<dyn LlmProvider>,
        config,
    );
    mark_ready(&engine).await.expect("就绪推进应成功");
    engine
}

/// 发送消息并消费完整个事件流（无 session_id → 创建新会话）。
async fn send_and_drain(engine: &Arc<Engine>, text: &str, persona: Option<&str>) {
    send_stream(engine, text, persona, None).await;
}

/// 注册角色类 persona（char-0001，带表达层 speaking_style）。
fn add_char_persona(storage: &MockStorage) {
    let mut p = Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    p.config =
        Some(r#"{"description":"测试角色","speaking_style":"热情活泼，喜欢用emoji"}"#.into());
    storage.add_persona(p);
}

/// 保存一条无主 L1 摘要（persona_uid=None），供索引重建经 unbound 通道加载。
///
/// 返回该 L1 的 id（PersonaFact.ref_l1_id 指向它，模拟事件抽取后的事实来源）。
async fn save_unbound_l1(storage: &MockStorage, summary: &str, keywords: &str) -> Uuid {
    let mut l1 = MemoryL1::new(Uuid::new_v4(), summary.to_string(), None);
    l1.keywords = Some(keywords.to_string());
    l1.salience = 1.0;
    let id = l1.id;
    storage.save_memory_l1(&l1).await.unwrap();
    id
}

/// 构造一条 active 事实（ref_l1_id 可选）。
fn active_fact(
    persona_uid: &str,
    field: ProfileField,
    content: &str,
    keyword_hint: &str,
    ref_l1: Option<Uuid>,
) -> PersonaFact {
    let mut f = PersonaFact::new(
        persona_uid.to_string(),
        field,
        content.to_string(),
        FactSource::Event,
    );
    f.status = FactStatus::Active;
    f.tier = FactTier::Stable;
    f.keyword_hint = Some(keyword_hint.to_string());
    f.confidence = 0.9;
    f.ref_l1_id = ref_l1;
    f
}

// =========================================================
// 场景 A: 关闭断言层（auto_fact_detect=false）→ RAG 仍独立覆盖事实
// =========================================================

/// RAG（memory_rag 开启）存在含事实的 L1 摘要时，即使知识层关闭，
/// memory_context 文本仍包含该事实摘要（RAG 摘要为主召回路径不依赖断言层）。
#[tokio::test]
async fn rag_covers_fact_when_assertion_layer_off() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，明白了。"));
    let config = RamariaConfig::default(); // auto_fact_detect=false（默认关闭断言层）
    let engine = build_ready_engine(&storage, &llm, config).await;

    add_char_persona(&storage);
    // 角色层 BasicInfo（避免 facts 为空触发 persona.toml 冷启动分支）
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::BasicInfo,
        "出生于上海",
        "上海",
        None,
    ));
    // L1 摘要内含"喜欢科幻电影"这一事实（RAG 覆盖源）
    let _l1_id = save_unbound_l1(
        &storage,
        "用户最近迷上了科幻电影，几乎每周都会看一两次",
        "科幻,电影",
    )
    .await;
    engine.rebuild_index().await.unwrap();

    send_and_drain(&engine, "你喜欢科幻电影吗？", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    // RAG 覆盖事实独立成立
    let rag = request
        .memory_context
        .as_deref()
        .expect("RAG 开启且有命中 → memory_context 应有内容");
    assert!(
        rag.contains("科幻电影"),
        "RAG 摘要应覆盖事实（即使断言层关闭）: {rag}"
    );
    // 断言层关闭 → system_prompt 不含知识卡片文本
    let prompt = &request.system_prompt;
    assert!(
        !prompt.contains("关于兴趣爱好"),
        "auto_fact_detect=false 不应产生知识卡片: {prompt}"
    );
}

// =========================================================
// 场景 B: auto_fact_detect=true + 事实来源已进 RAG 覆盖 → 知识块不去重误伤
// =========================================================

/// 同一条事实（ref_l1_id 指向已覆盖 L1）不应同时在 RAG 摘要与知识卡片出现：
/// 知识块为空，角色层已知事实区仍展示 BasicInfo（角色区保留）。
#[tokio::test]
async fn knowledge_not_duplicated_when_rag_covers_fact() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，明白了。"));
    let mut config = RamariaConfig::default();
    config.knowledge.auto_fact_detect = true; // 打开断言知识层
    let engine = build_ready_engine(&storage, &llm, config).await;

    add_char_persona(&storage);
    // 角色层已知事实（BasicInfo）：会进角色描述区
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::BasicInfo,
        "出生于上海",
        "上海",
        None,
    ));
    // 知识层事实（Interests）：来源 L1 与 RAG 覆盖的摘要同一文档
    let l1_id = save_unbound_l1(
        &storage,
        "用户最近迷上了科幻电影，几乎每周都会看一两次",
        "科幻,电影",
    )
    .await;
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        Some(l1_id),
    ));
    engine.rebuild_index().await.unwrap();

    send_and_drain(&engine, "你喜欢科幻电影吗？", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    // RAG 摘要覆盖事实
    let rag = request
        .memory_context
        .as_deref()
        .expect("RAG 应命中覆盖 L1");
    assert!(rag.contains("科幻电影"), "RAG 摘要覆盖: {rag}");
    // 知识卡片不重复注入同一事实（已被 RAG 覆盖去重）
    let prompt = &request.system_prompt;
    assert!(
        !prompt.contains("关于兴趣爱好：喜欢科幻电影"),
        "事实已由 RAG 覆盖 → 知识卡片不应重复注入: {prompt}"
    );
    // 角色层已知事实区保留 BasicInfo（取角色区、去知识区）
    assert!(
        prompt.contains("出生于上海"),
        "角色层已知事实区应保留 BasicInfo: {prompt}"
    );
}

// =========================================================
// 场景 C: auto_fact_detect=true + RAG 未覆盖 → 知识卡片兜底注入
// =========================================================

/// RAG 未命中（无相关 L1 文档）时，知识判定器命中仍按兜底注入知识卡片，
/// 断言知识作为兜底不失效（RAG 覆盖集合为空 → 不去重）。
#[tokio::test]
async fn knowledge_fallback_injected_when_rag_misses() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，明白了。"));
    let mut config = RamariaConfig::default();
    config.knowledge.auto_fact_detect = true;
    let engine = build_ready_engine(&storage, &llm, config).await;

    add_char_persona(&storage);
    // 角色层 BasicInfo（避免 facts 为空触发 persona.toml 冷启动分支）
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::BasicInfo,
        "出生于上海",
        "上海",
        None,
    ));
    // 手工/事件类事实（无 ref_l1_id，无法被 RAG 映射）→ 兜底保留
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        None,
    ));
    // 空检索器（RAG 未覆盖）
    engine.rebuild_index().await.unwrap();

    send_and_drain(&engine, "你喜欢科幻电影吗？", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    // RAG 无命中 → memory_context 为空
    assert!(
        request.memory_context.is_none()
            || !request
                .memory_context
                .as_deref()
                .unwrap()
                .contains("科幻电影"),
        "RAG 未覆盖时该事实不应来自摘要"
    );
    // 知识判定器兜底注入仍生效（RAG 未覆盖不误伤）
    let prompt = &request.system_prompt;
    assert!(
        prompt.contains("关于兴趣爱好：喜欢科幻电影"),
        "RAG 未覆盖 → 知识卡片应兜底注入: {prompt}"
    );
}

// =========================================================
// 回归红线: 全部红线在去重链路下保持
// =========================================================

/// SpeakingStyle 不参与知识层检索注入（既有红线在去重后仍不回归）。
#[tokio::test]
async fn speaking_style_not_injected_after_dedup_chain() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let mut config = RamariaConfig::default();
    config.knowledge.auto_fact_detect = true;
    let engine = build_ready_engine(&storage, &llm, config).await;

    add_char_persona(&storage);
    // 角色层 BasicInfo（避免 facts 为空触发 persona.toml 冷启动分支）
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::BasicInfo,
        "出生于上海",
        "上海",
        None,
    ));
    // SpeakingStyle active 事实（表达层注入内容，知识层只读引用）
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::SpeakingStyle,
        "习惯使用口癖词「哇塞」",
        "口癖,节奏",
        None,
    ));
    // 一个 Interests fact 触发判定
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        None,
    ));
    engine.rebuild_index().await.unwrap();

    send_and_drain(&engine, "你喜欢科幻电影吗？", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    // Interests 事实触发知识卡片兜底注入（SpeakingStyle 被排除后仅 Interests 进入）
    assert!(
        prompt.contains("关于兴趣爱好：喜欢科幻电影"),
        "Interests 事实应触发知识卡片: {prompt}"
    );
    assert!(
        !prompt.contains("哇塞"),
        "SpeakingStyle 事实内容不应进入任何注入区（表达层只读引用无副作用）: {prompt}"
    );
}
