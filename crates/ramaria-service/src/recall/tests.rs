//! crates/ramaria-service/src/recall/tests.rs - 召回用例单元测试
//!
//! 设计特点:
//! - 由 recall 模块以 `#[cfg(test)] mod tests;` 收纳：覆盖策略口径 / 检索输入解析 /
//!   检索模式 / 概览模式 / 知识层五组路径
//! - 使用 mock LLM 与真实 SQLite 临时库，断言以召回结果结构与落库状态为准
//! - 未配置嵌入（确定性 BM25 路径），不依赖网络与模型文件
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::entry::resolve_query;
use super::*;
use crate::test_support::{
    MockLlm, engine_with_db, engine_with_llm_and_config, seed_l1 as seed_l1_raw, seed_persona,
};
use crate::types::{ChatRole, ChatTurn, RecallLayer, RecallMode, RecallRequest};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{FactSource, FactTier, ProfileField};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

/// 造一条 L1（默认不带关键词；BM25 直接命中摘要文本）。
async fn seed_l1(storage: &SqliteStorage, persona: &str, summary: &str, created_at: i64) -> Uuid {
    seed_l1_raw(storage, persona, summary, None, created_at).await
}

// ---- 策略 ----

#[test]
fn policy_default_is_conservative() {
    let policy = RecallPolicy::default();
    assert!(!policy.allow_raw_text, "默认不返回原文块");
    assert!(policy.persona_allowed("char-0001"), "默认全部人格可见");
}

/// 配置映射：原文开关 = `[injection].utt` × `[utt].enabled`；白名单缺省不收紧。
#[test]
fn policy_from_config_maps_utt_gates() {
    let config = RamariaConfig::default();
    let policy = RecallPolicy::from_config(&config);
    assert!(policy.allow_raw_text, "默认配置：两闸门开启 → 原文层开放");
    assert_eq!(
        policy.allowed_personas,
        vec!["*".to_string()],
        "缺省不收紧人格白名单"
    );

    let mut injection_off = RamariaConfig::default();
    injection_off.injection.utt = false;
    assert!(
        !RecallPolicy::from_config(&injection_off).allow_raw_text,
        "注入闸门关闭 → 原文层关闭"
    );

    let mut utt_off = RamariaConfig::default();
    utt_off.utt.enabled = false;
    assert!(
        !RecallPolicy::from_config(&utt_off).allow_raw_text,
        "utt 链路关闭 → 原文层关闭"
    );
}

/// Engine 装配缺省：策略随配置映射（默认配置开放原文；配置关闭 utt 则关闭）。
#[tokio::test]
async fn engine_default_policy_follows_config() {
    let (engine, _storage, dir) =
        engine_with_llm_and_config("policy-default", MockLlm::local(), RamariaConfig::default())
            .await;
    assert!(
        engine.recall_policy().allow_raw_text,
        "默认配置装配 → 原文层开放"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let mut utt_off = RamariaConfig::default();
    utt_off.utt.enabled = false;
    let (engine, _storage, dir) =
        engine_with_llm_and_config("policy-utt-off", MockLlm::local(), utt_off).await;
    assert!(
        !engine.recall_policy().allow_raw_text,
        "配置关闭 utt → 原文层关闭"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 宿主覆盖优先：`set_recall_policy` 注入值整体替换装配缺省。
#[tokio::test]
async fn engine_policy_override_beats_default() {
    let (engine, _storage, dir) = engine_with_db("policy-override").await;
    assert!(
        engine.recall_policy().allow_raw_text,
        "装配缺省为配置映射（默认配置开放原文）"
    );

    engine.set_recall_policy(RecallPolicy::default());
    assert!(
        !engine.recall_policy().allow_raw_text,
        "注入的保守策略覆盖装配缺省"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn policy_persona_whitelist() {
    let policy = RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]);
    assert!(policy.persona_allowed("char-0001"));
    assert!(!policy.persona_allowed("char-0002"));
    // 空列表兜底为全可见（避免"空即全禁"的误配）
    let all = RecallPolicy::default().with_allowed_personas(Vec::new());
    assert!(all.persona_allowed("char-0002"));
}

// ---- 检索输入解析 ----

#[test]
fn query_resolution_prefers_explicit_then_last_user_turn() {
    let req = RecallRequest {
        query: Some(" 项目进度 ".to_string()),
        messages: vec![ChatTurn {
            role: ChatRole::User,
            content: "另一句".to_string(),
        }],
        ..RecallRequest::default()
    };
    assert_eq!(resolve_query(&req), "项目进度");

    let req = RecallRequest {
        query: None,
        messages: vec![
            ChatTurn {
                role: ChatRole::User,
                content: "第一句".to_string(),
            },
            ChatTurn {
                role: ChatRole::Assistant,
                content: "回复".to_string(),
            },
            ChatTurn {
                role: ChatRole::User,
                content: "最后一句".to_string(),
            },
        ],
        ..RecallRequest::default()
    };
    assert_eq!(resolve_query(&req), "最后一句", "取最后一条用户消息");

    let empty = RecallRequest {
        query: Some("   ".to_string()),
        ..RecallRequest::default()
    };
    assert!(resolve_query(&empty).is_empty(), "空白 query 视为无输入");
}

// ---- 检索模式 ----

/// 检索模式：命中 L1 → context 含摘要、items 结构完整、stats.mode = search。
#[tokio::test]
async fn search_mode_returns_context_and_items() {
    let (engine, storage, dir) = engine_with_db("search").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班",
        1_000,
    )
    .await;
    engine.ensure_index_loaded().await.expect("索引加载");

    let result = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert_eq!(result.stats.mode, RecallMode::Search);
    assert!(
        result.context.contains("工作压力"),
        "上下文应含命中摘要: {}",
        result.context
    );
    assert!(!result.items.is_empty(), "应返回结构化条目");
    let item = &result.items[0];
    assert_eq!(item.layer, RecallLayer::L1);
    assert!(item.score.is_some(), "检索条目应带融合分");
    assert!(item.time.is_some(), "检索条目应带时间");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 分层开关：include 只含 knowledge 时不返回记忆层条目（也不触发检索）。
#[tokio::test]
async fn include_filters_layers() {
    let (engine, storage, dir) = engine_with_db("layers").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "用户最近工作压力很大", 1_000).await;

    let result = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::Knowledge]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert!(
        result
            .items
            .iter()
            .all(|i| i.layer == RecallLayer::Knowledge),
        "仅请求知识层时不应返回记忆层条目: {:?}",
        result.items
    );
    assert!(!result.context.contains("[相关记忆]"), "记忆段落未请求");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 分层开关（摘要路子层）：只请求 L2 时不返回 L1 条目与 L1 文本（反向亦然）。
///
/// 回归背景: 早期实现把 L1/L2 当"整层"处理，include=[L2] 仍会带出 L1 条目与正文，
/// 违反「分层开关生效」的验收口径。
#[tokio::test]
async fn memory_sublayer_switch_is_honoured() {
    let (engine, storage, dir) = engine_with_db("sublayers").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1_raw(
        &storage,
        "char-0001",
        "用户提到工作压力（摘要侧）",
        Some("工作压力"),
        1_000,
    )
    .await;
    // 同关键词的 L2 事件（确保两层都能被同一查询命中）
    let mut event = ramaria_core::types::MemoryEvent::new(
        "char-0001".to_string(),
        "工作压力事件（事件侧）".to_string(),
        "群聊里被点名批评".to_string(),
        1_000,
        2_000,
    );
    event.keywords = Some("工作压力".to_string());
    event.share = 1.0;
    event.confidence = 0.9;
    storage.save_event(&event).await.expect("写入事件");
    engine.ensure_index_loaded().await.expect("索引加载");

    // 只请求 L2：无 L1 条目、段落不含摘要侧文本
    let l2_only = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L2]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(
        l2_only
            .items
            .iter()
            .all(|item| item.layer == RecallLayer::L2),
        "仅请求 L2 时不应出现其它分层: {:?}",
        l2_only.items
    );
    assert!(!l2_only.items.is_empty(), "L2 应命中");
    assert!(
        !l2_only.context.contains("摘要侧"),
        "段落不应含 L1 文本: {}",
        l2_only.context
    );

    // 只请求 L1：无 L2 条目、段落不含事件侧文本
    let l1_only = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(
        l1_only
            .items
            .iter()
            .all(|item| item.layer == RecallLayer::L1),
        "仅请求 L1 时不应出现其它分层: {:?}",
        l1_only.items
    );
    assert!(!l1_only.items.is_empty(), "L1 应命中");
    assert!(
        !l1_only.context.contains("事件侧"),
        "段落不应含 L2 文本: {}",
        l1_only.context
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预算裁剪：max_chars 极小时 context 被截断且 stats.truncated = true。
#[tokio::test]
async fn budget_truncates_context() {
    let (engine, storage, dir) = engine_with_db("budget").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班到深夜，反复提到职场焦虑",
        1_000,
    )
    .await;
    engine.ensure_index_loaded().await.expect("索引加载");

    let result = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            max_chars: Some(4),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert!(result.stats.truncated, "超预算应标记截断");
    assert!(
        result.context.chars().count() <= 4,
        "context 应在字符预算内: {}",
        result.context
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// max_items 上限：请求超过 20 时按硬边界截断（不 panic、不越界）。
#[tokio::test]
async fn max_items_clamped_to_hard_limit() {
    let (engine, storage, dir) = engine_with_db("items").await;
    seed_persona(&storage, "char-0001").await;
    for i in 0..3 {
        seed_l1(
            &storage,
            "char-0001",
            &format!("用户第{i}次提到工作压力"),
            1_000 + i,
        )
        .await;
    }
    engine.ensure_index_loaded().await.expect("索引加载");

    let result = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            max_items: Some(999),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert!(result.items.len() <= 20, "items 不得超过硬上限 20");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 隐私策略：人格不在白名单 → Privacy 错误（越权拒绝）。
#[tokio::test]
async fn persona_whitelist_rejects_others() {
    let (engine, storage, dir) = engine_with_db("policy").await;
    seed_persona(&storage, "char-0001").await;
    engine.set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
    );

    let err = engine
        .recall(RecallRequest {
            query: Some("任意".to_string()),
            persona: Some("char-0002".to_string()),
            ..RecallRequest::default()
        })
        .await
        .expect_err("白名单外人格应被拒绝");
    assert_eq!(err.category(), "privacy");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 原文层：策略关闭时即使请求 raw 也不返回原文块。
#[tokio::test]
async fn raw_layer_requires_policy_allow() {
    let (engine, storage, dir) = engine_with_db("raw").await;
    seed_persona(&storage, "char-0001").await;
    // 本用例验证"策略关闭"分支：显式注入保守策略（装配缺省由配置映射决定）
    engine.set_recall_policy(RecallPolicy::default());

    let result = engine
        .recall(RecallRequest {
            query: Some("任意".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::Raw]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert!(result.context.is_empty(), "策略关闭时原文层不产出段落");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 概览模式 ----

/// 概览模式：无 query / messages → 时间倒序返回最近记忆。
#[tokio::test]
async fn overview_mode_returns_timeline() {
    let (engine, storage, dir) = engine_with_db("overview").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "较早的摘要内容", 1_000).await;
    seed_l1(&storage, "char-0001", "较新的摘要内容", 2_000).await;

    let result = engine
        .recall(RecallRequest {
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("概览成功");

    assert_eq!(result.stats.mode, RecallMode::Overview);
    assert!(result.context.contains("[记忆概览]"));
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.items[0].text, "较新的摘要内容", "应时间倒序");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 概览模式：空库 → 空结构（不报错）。
#[tokio::test]
async fn overview_mode_empty_db() {
    let (engine, _storage, dir) = engine_with_db("overview-empty").await;

    let result = engine
        .recall(RecallRequest::default())
        .await
        .expect("概览成功");

    assert_eq!(result.stats.mode, RecallMode::Overview);
    assert!(result.items.is_empty());
    assert!(result.context.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 概览条数上限：max_items 生效且指标标记截断。
#[tokio::test]
async fn overview_respects_max_items() {
    let (engine, storage, dir) = engine_with_db("overview-limit").await;
    seed_persona(&storage, "char-0001").await;
    for i in 0..4 {
        seed_l1(&storage, "char-0001", &format!("第{i}条摘要"), 1_000 + i).await;
    }

    let result = engine
        .recall(RecallRequest {
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            max_items: Some(2),
            ..RecallRequest::default()
        })
        .await
        .expect("概览成功");

    assert_eq!(result.items.len(), 2);
    assert!(result.stats.truncated);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 知识层与辅助条目 ----

/// 知识层：命中判定 + 卡片渲染 + 条目结构（含事实 id 与置信度）。
#[tokio::test]
async fn knowledge_layer_renders_hit_facts() {
    let (engine, storage, dir) = engine_with_db("knowledge").await;
    seed_persona(&storage, "char-0001").await;
    let mut fact = ramaria_core::types::PersonaFact::new(
        "char-0001".to_string(),
        ProfileField::Interests,
        "喜欢露营和徒步".to_string(),
        FactSource::Manual,
    );
    fact.tier = FactTier::Stable;
    fact.keyword_hint = Some("露营,徒步".to_string());
    storage.save_fact(&fact).await.expect("写入事实");

    let result = engine
        .recall(RecallRequest {
            query: Some("你记得我喜欢露营吗".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::Knowledge]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert!(
        result.context.contains("露营"),
        "知识卡片应命中并渲染: {}",
        result.context
    );
    assert!(!result.items.is_empty());
    assert_eq!(result.items[0].layer, RecallLayer::Knowledge);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空白请求的归一化：query 与 messages 皆空 → 概览；messages 单条 → 检索。
#[tokio::test]
async fn messages_only_enters_search_mode() {
    let (engine, storage, dir) = engine_with_db("messages").await;
    seed_persona(&storage, "char-0001").await;

    let result = engine
        .recall(RecallRequest {
            persona: Some("char-0001".to_string()),
            messages: vec![ChatTurn {
                role: ChatRole::User,
                content: "今天想聊聊工作".to_string(),
            }],
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");

    assert_eq!(result.stats.mode, RecallMode::Search, "有对话片段应走检索");

    let _ = std::fs::remove_dir_all(&dir);
}
