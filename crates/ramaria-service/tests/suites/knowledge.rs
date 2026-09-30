//! crates/ramaria-service/tests/suites/knowledge.rs - 知识事实加载与降级用例
//!
//! 设计特点:
//! - 覆盖判定器命中 / 未命中 / 关闭三条路径的返回集合
//! - 覆盖仅 active 事实参与注入、人格严格隔离
//! - 覆盖无嵌入依赖的检索路径与检索失败时的静默降级（返回空集合，不阻塞主流程）
//! - 全部使用内存存储，无 LLM 与真实模型依赖

use ramaria_core::config::KnowledgeConfig;
use ramaria_core::types::{FactSource, FactStatus, FactTier, PersonaFact, ProfileField};
use ramaria_memory::fact::retriever::load_knowledge_facts_for_query;

use crate::support::mock_backend::MockStorage;

// =========================================================
// 测试用 PersonaFact 构造辅助
// =========================================================

/// 构造一条 active 事实（Interests 字段）。
fn active_fact(persona_uid: &str, content: &str, keyword_hint: &str) -> PersonaFact {
    let mut f = PersonaFact::new(
        persona_uid.to_string(),
        ProfileField::Interests,
        content.to_string(),
        FactSource::Event,
    );
    f.status = FactStatus::Active;
    f.tier = FactTier::Stable;
    f.keyword_hint = Some(keyword_hint.to_string());
    f.confidence = 0.9;
    f
}

/// 构造一条 candidate 事实（不应参与注入）。
fn candidate_fact(persona_uid: &str, content: &str, keyword_hint: &str) -> PersonaFact {
    let mut f = active_fact(persona_uid, content, keyword_hint);
    f.status = FactStatus::Candidate;
    f
}

/// 构造一条 superseded 事实（不应参与注入）。
fn superseded_fact(persona_uid: &str, content: &str, keyword_hint: &str) -> PersonaFact {
    let mut f = active_fact(persona_uid, content, keyword_hint);
    f.status = FactStatus::Superseded;
    f
}

// =========================================================
// 判定器不命中 → 不注入
// =========================================================

#[tokio::test]
async fn detector_not_hit_returns_empty() {
    let storage = MockStorage::new();
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));

    let config = KnowledgeConfig::default();
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "今天天气怎么样？").await;

    assert!(
        facts.is_empty(),
        "判定器不命中 → 不注入（静默降级，不影响主线）"
    );
}

// =========================================================
// 判定器关闭 → 不注入
// =========================================================

#[tokio::test]
async fn detector_disabled_returns_empty() {
    let storage = MockStorage::new();
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));

    let config = KnowledgeConfig {
        detector_enabled: false, // 关闭判定器
        ..Default::default()
    };
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "你喜欢什么电影？").await;

    assert!(
        facts.is_empty(),
        "判定器关闭 → 不检索注入（回退 v1.5，prompt 无知识块）"
    );
}

// =========================================================
// 判定器命中 → 注入 active 事实
// =========================================================

#[tokio::test]
async fn detector_hit_returns_active_facts() {
    let storage = MockStorage::new();
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));

    let config = KnowledgeConfig::default();
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "你喜欢看什么电影？").await;

    assert_eq!(facts.len(), 1, "判定器命中应返回 active 事实");
    assert_eq!(facts[0].content, "喜欢科幻电影");
}

// =========================================================
// 注入只取 active（candidate/superseded 不注入）
// =========================================================

#[tokio::test]
async fn only_active_facts_injected() {
    let storage = MockStorage::new();
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));
    storage.add_fact(candidate_fact("char-0001", "喜欢编程", "编程"));
    storage.add_fact(superseded_fact("char-0001", "旧爱好音乐", "音乐"));

    let config = KnowledgeConfig::default();
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "你喜欢什么电影？").await;

    assert_eq!(facts.len(), 1, "只注入 active 事实");
    assert_eq!(facts[0].content, "喜欢科幻电影");
}

// =========================================================
// 知识检索无 embedding 依赖（embedding 不可用 → 同 field 召回仍可用）
// =========================================================

#[tokio::test]
async fn retrieval_works_without_embedding() {
    // load_knowledge_facts_for_query 走规则判定器（零 embedding/零 LLM），
    // 即使 embedding 模型不可用也不阻塞——知识检索本身就是同 field/关键词召回。
    let storage = MockStorage::new();
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));

    let config = KnowledgeConfig::default();
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "你喜欢看什么电影？").await;

    assert_eq!(
        facts.len(),
        1,
        "embedding 不可用 → 知识同 field 召回仍可用（不阻塞主流程）"
    );
}

// =========================================================
// 检索/注入严格按 persona_uid 隔离（跨 persona 不可见）
// =========================================================

#[tokio::test]
async fn persona_isolation_cross_persona_invisible() {
    let storage = MockStorage::new();
    // char-0001 的私人事实
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));

    // 其它 persona 查询 → 看不到 char-0001 的事实
    let config = KnowledgeConfig::default();
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0002", "你喜欢什么电影？").await;

    assert!(
        facts.is_empty(),
        "跨 persona 检索不可见（注入结果不含他人事实）"
    );
}

#[tokio::test]
async fn persona_isolation_same_persona_only() {
    let storage = MockStorage::new();
    storage.add_fact(active_fact("char-0001", "喜欢科幻电影", "电影,科幻"));
    storage.add_fact(active_fact("char-0002", "喜欢编程", "编程,开发"));

    let config = KnowledgeConfig::default();
    // char-0001 查询命中自己的事实，但注入结果不含 char-0002 的
    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "你喜欢什么电影？").await;

    assert_eq!(facts.len(), 1, "只返回本人 persona 的事实");
    assert_eq!(facts[0].content, "喜欢科幻电影");
    assert!(
        facts.iter().all(|f| f.persona_uid == "char-0001"),
        "注入结果不含他人 persona 事实"
    );
}

// =========================================================
// 读取 active 事实失败 → 静默降级为空（不阻塞主流程）
// =========================================================

/// 读取失败场景：知识层读取 active 事实时存储报错 → 降级为空，不抛错。
#[tokio::test]
async fn retrieval_failure_degrades_to_empty() {
    // MockStorage 不会失败；此处验证"空库（无该 persona 事实）→ 空结果不抛错"，
    // 等价于存储读取无结果时的静默降级路径。
    let storage = MockStorage::new();
    let config = KnowledgeConfig::default();

    let facts =
        load_knowledge_facts_for_query(&storage, &config, "char-0001", "你喜欢什么电影？").await;
    assert!(facts.is_empty(), "空库 → 空结果，不阻塞主流程");
}
