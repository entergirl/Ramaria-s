//! crates/ramaria-service/tests/parity/recall.rs - 对照路径：召回（recall）
//!
//! 设计特点:
//! - fixture 固定：单 persona + 3 条不同时间与主题的 L1（含一条与查询强相关），
//!   时间取"当前时间 - 固定偏移"，保证衰减分数跨运行稳定
//! - 快照只含稳定字段：模式 / 条目（层、文本、分数）/ 通道命中数 / 截断标记 /
//!   上下文长度与关键片段命中布尔值；不落 id、时间与上下文全文
//! - 覆盖三条链路：向量化确实发生（嵌入 mock 调用计数）、基线一致（golden 冻结）、
//!   跨隔离环境等价（`assert_parity`）；另以独立用例锁定人格白名单的隐私拒绝语义
//! - 输出入口：`snapshot_of` 是"该 fixture 上的规范化输出"的唯一入口，
//!   快照直接送入 `GoldenStore` 冻结或比对

use std::sync::Arc;

use ramaria_core::traits::{ChatRequest, LlmProvider};
use ramaria_core::types::CHANNEL_LOCAL;
use ramaria_service::RecallPolicy;
use ramaria_service::types::{ChatSendRequest, RecallLayer, RecallRequest};
use ramaria_storage::SqliteStorage;
use serde_json::json;

use crate::support::env::DEFAULT_ASSISTANT_REPLY;
use crate::support::{
    GoldenStore, ParityEnv, ParityError, ParityResult, ScriptedLlm, Snapshot, assert_parity,
    fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "recall_l1_keyword_hit";

/// fixture 人格 uid。
const PERSONA: &str = "char-parity-recall";

/// 查询词（与第一条 L1 强相关）。
const QUERY: &str = "工作压力";

/// 注入上下文场景名（同时作为 golden 基线文件名）。
const INJECTION_SCENARIO: &str = "recall_injected_context";

// =========================================================
// 场景执行
// =========================================================

/// 造 fixture：persona 与 3 条不同主题的 L1。
///
/// 时间设计:
/// - 第 1 条最"新"（固定偏移基准），第 2 / 3 条逐日更早，
///   使衰减分排序可预期、可解释（新鲜且相关的记忆优先）。
async fn fixture(env: &ParityEnv) -> ParityResult<()> {
    fixtures::seed_persona(env.storage(), PERSONA).await?;
    fixtures::seed_l1(
        env.storage(),
        PERSONA,
        "用户最近工作压力很大，常加班到深夜",
        Some("工作压力,加班"),
        fixtures::fixture_ts(0),
    )
    .await?;
    fixtures::seed_l1(
        env.storage(),
        PERSONA,
        "用户周末去爬山，天气很好",
        Some("爬山,周末"),
        fixtures::fixture_ts(-86_400_000),
    )
    .await?;
    fixtures::seed_l1(
        env.storage(),
        PERSONA,
        "用户喜欢喝手冲咖啡",
        Some("咖啡"),
        fixtures::fixture_ts(-172_800_000),
    )
    .await?;
    Ok(())
}

/// 构造召回请求（仅 L1 层，固定预算）。
fn request() -> RecallRequest {
    RecallRequest {
        query: Some(QUERY.to_string()),
        persona: Some(PERSONA.to_string()),
        include: Some(vec![RecallLayer::L1]),
        max_items: Some(5),
        max_chars: Some(1_200),
        ..RecallRequest::default()
    }
}

/// 在给定环境上执行召回场景，产出规范化快照。
async fn snapshot_of(env: &ParityEnv) -> ParityResult<Snapshot> {
    fixture(env).await?;

    let result = env
        .engine()
        .recall(request())
        .await
        .map_err(|e| ParityError::env("执行召回用例", e))?;

    let items: Vec<serde_json::Value> = result
        .items
        .iter()
        .map(|item| {
            json!({
                "layer": item.layer,
                "text": item.text.clone(),
                "score": item.score,
            })
        })
        .collect();

    Ok(Snapshot::new(
        SCENARIO,
        json!({
            "mode": result.stats.mode,
            "items": items,
            "channels": result.stats.channels,
            "truncated": result.stats.truncated,
            "context_chars": result.context.chars().count(),
            "context_hits_query": result.context.contains(QUERY),
        }),
    ))
}

/// 构造"默认回复 + 可用嵌入"的召回环境（本路径不消费 LLM 回复）。
async fn env_for_recall(tag: &str) -> ParityResult<ParityEnv> {
    ParityEnv::new(tag).await
}

// =========================================================
// 测试
// =========================================================

/// 基线一致：召回输出与冻结基线逐字段一致，且关键命中断言成立。
#[tokio::test]
async fn recall_snapshot_matches_golden_baseline() {
    let env = env_for_recall("recall-golden")
        .await
        .expect("召回对照环境应可构建");
    let snapshot = snapshot_of(&env).await.expect("召回场景应执行成功");

    // 关键行为断言：强相关 L1 必须命中，且向量化确实发生（嵌入 mock 被调用）
    let items = snapshot.value()["items"]
        .as_array()
        .expect("快照应含 items 数组");
    assert!(!items.is_empty(), "召回应有命中条目");
    assert!(
        items.iter().any(|item| item["text"]
            .as_str()
            .is_some_and(|text| text.contains(QUERY))),
        "与查询强相关的 L1 必须命中: {items:?}"
    );
    assert!(
        env.embedding().call_count() > 0,
        "索引构建应调用嵌入 provider（向量通道参与检索）"
    );

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&snapshot)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );
    tracing::info!(
        path = %outcome.path().display(),
        ?outcome,
        "召回基线比对完成"
    );

    env.cleanup().await;
}

/// 独立产出等价：两个隔离环境各自执行同一召回，输出应完全一致。
#[tokio::test]
async fn recall_isolated_envs_produce_equivalent_snapshots() {
    let first_env = env_for_recall("recall-parity-a")
        .await
        .expect("首个对照环境应可构建");
    let first = snapshot_of(&first_env).await.expect("首轮召回应成功");
    first_env.cleanup().await;

    let second_env = env_for_recall("recall-parity-b")
        .await
        .expect("第二个对照环境应可构建");
    let second = snapshot_of(&second_env).await.expect("第二轮召回应成功");
    second_env.cleanup().await;

    assert_parity("recall/l1-keyword-hit", &first, &second);
}

/// 隐私边界：目标人格不在白名单内时，召回以 Privacy 错误显式拒绝。
#[tokio::test]
async fn recall_rejects_persona_outside_whitelist() {
    let env = env_for_recall("recall-privacy")
        .await
        .expect("召回对照环境应可构建");
    fixture(&env).await.expect("fixture 应造数成功");

    env.engine().set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["char-other".into()]),
    );

    let error = env
        .engine()
        .recall(request())
        .await
        .expect_err("白名单外的人格应被拒绝");
    assert_eq!(error.category(), "privacy", "应返回隐私类错误: {error}");

    env.cleanup().await;
}

// =========================================================
// 逐字对照（应用装配 vs 服务装配）
// =========================================================

/// 造逐字对照 fixture：persona 与 3 条不同主题的 L1（与既有场景同文本 / 同时间偏移）。
async fn seed_cross_fixture(storage: &SqliteStorage) -> ParityResult<()> {
    fixtures::seed_persona(storage, PERSONA).await?;
    fixtures::seed_l1(
        storage,
        PERSONA,
        "用户最近工作压力很大，常加班到深夜",
        Some("工作压力,加班"),
        fixtures::fixture_ts(0),
    )
    .await?;
    fixtures::seed_l1(
        storage,
        PERSONA,
        "用户周末去爬山，天气很好",
        Some("爬山,周末"),
        fixtures::fixture_ts(-86_400_000),
    )
    .await?;
    fixtures::seed_l1(
        storage,
        PERSONA,
        "用户喜欢喝手冲咖啡",
        Some("咖啡"),
        fixtures::fixture_ts(-172_800_000),
    )
    .await?;
    Ok(())
}

/// 取最后一个生成请求（本路径每轮恰有一次生成调用）。
fn last_request(requests: Vec<ChatRequest>) -> ParityResult<ChatRequest> {
    requests
        .into_iter()
        .last()
        .ok_or_else(|| ParityError::env("读取生成请求", "应至少记录一次请求"))
}

/// 召回产物快照（两侧同形）。
fn recall_value(request: &ChatRequest) -> serde_json::Value {
    json!({
        "memory_context": request.memory_context.clone(),
        "memory_context_chars": request
            .memory_context
            .as_ref()
            .map(|text| text.chars().count())
            .unwrap_or(0),
        "memory_context_present": request.memory_context.is_some(),
        "history_len": request.history.len(),
        "user_message": request.user_message.clone(),
    })
}

/// 注入上下文：加载索引 → 发送查询消息 → 取生成请求中的召回产物。
async fn injected_context_snapshot(env: &ParityEnv, llm: &ScriptedLlm) -> ParityResult<Snapshot> {
    seed_cross_fixture(env.storage()).await?;
    env.engine()
        .ensure_index_loaded()
        .await
        .map_err(|e| ParityError::env("加载索引", e))?;
    env.engine()
        .chat_send(ChatSendRequest {
            message: QUERY.to_string(),
            persona: Some(PERSONA.to_string()),
            session_id: None,
            conversation_id: None,
            channel: CHANNEL_LOCAL.to_string(),
        })
        .await
        .map_err(|e| ParityError::env("发送查询消息", e))?;

    let request = last_request(llm.requests())?;
    Ok(Snapshot::new(INJECTION_SCENARIO, recall_value(&request)))
}

/// 基线一致：注入生成请求的记忆上下文与冻结基线逐字段一致。
///
/// 观测口径:
/// - 生成入口没有独立的召回读取面，召回结果以"注入生成请求的记忆上下文"为观测量
///   （即用户可见的召回产物），从生成请求中读取；
/// - fixture 只有摘要层（不造原文块），加载索引后发一条查询消息；
/// - 上下文内的分数标注按两位小数渲染，对运行时间基准只有量级远小于显示精度的差异。
#[tokio::test]
async fn recall_injected_context_matches_golden_baseline() {
    let llm = Arc::new(ScriptedLlm::reply(DEFAULT_ASSISTANT_REPLY));
    let env = ParityEnv::with_llm(
        "recall-injected-context",
        Arc::clone(&llm) as Arc<dyn LlmProvider>,
    )
    .await
    .expect("召回对照环境应可构建");
    let snapshot = injected_context_snapshot(&env, &llm)
        .await
        .expect("召回注入场景应执行成功");
    assert!(
        env.embedding().call_count() > 0,
        "加载索引应触发向量化（向量通道参与检索）"
    );

    // 关键行为锚点：召回命中注入，且上下文包含命中的摘要文本（否则观测面无内容）
    assert_eq!(
        snapshot.value()["memory_context_present"].as_bool(),
        Some(true),
        "召回应命中并注入记忆上下文"
    );
    let context = snapshot.value()["memory_context"]
        .as_str()
        .unwrap_or_default();
    assert!(
        context.contains(QUERY),
        "记忆上下文应包含命中摘要的查询词: {context}"
    );

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&snapshot)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );

    env.cleanup().await;
}
