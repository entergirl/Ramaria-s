//! crates/ramaria-service/tests/parity/recall.rs - 对照路径：召回（recall）
//!
//! 设计特点:
//! - fixture 固定：单 persona + 3 条不同时间与主题的 L1（含一条与查询强相关），
//!   时间取"当前时间 - 固定偏移"，保证衰减分数跨运行稳定
//! - 快照只含稳定字段：模式 / 条目（层、文本、分数）/ 通道命中数 / 截断标记 /
//!   上下文长度与关键片段命中布尔值；不落 id、时间与上下文全文
//! - 覆盖三条链路：向量化确实发生（嵌入 mock 调用计数）、基线一致（golden 冻结）、
//!   跨隔离环境等价（`assert_parity`）；另以独立用例锁定人格白名单的隐私拒绝语义
//! - 输出入口：`snapshot_of` 是"某一实现在该 fixture 上的规范化输出"的唯一入口，
//!   同形状快照可直接送入 `assert_parity` 比对

use ramaria_service::RecallPolicy;
use ramaria_service::types::{RecallLayer, RecallRequest};
use serde_json::json;

use crate::support::{
    GoldenStore, ParityEnv, ParityError, ParityResult, Snapshot, assert_parity, fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "recall_l1_keyword_hit";

/// fixture 人格 uid。
const PERSONA: &str = "char-parity-recall";

/// 查询词（与第一条 L1 强相关）。
const QUERY: &str = "工作压力";

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
