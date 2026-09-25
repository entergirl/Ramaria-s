//! crates/ramaria-service/tests/parity/index.rs - 对照路径：检索索引（懒加载与刷新）
//!
//! 设计特点:
//! - 覆盖合并后的索引语义要点：懒加载（只构建一次）/ 加载后增量可检索 /
//!   跨进程写入按代次刷新 / 未加载窗口的脏标记重建
//! - 跨进程等价物：在同一库文件上开第二个连接池写入 L1，模拟"桌面写、MCP 读"场景，
//!   不引入真实多进程
//! - 快照只含布尔与计数：四个场景的结论指标；索引内部结构与向量不落盘
//! - fixture 时间固定偏移：保证跨进程写入的 L1 与本地语料戳判定稳定
//! - 输出入口：`snapshot_of` 是"某一实现在该 fixture 上的规范化输出"的唯一入口，
//!   同形状快照可直接送入 `assert_parity` 比对

use std::sync::Arc;

use ramaria_core::traits::LlmProvider;
use ramaria_service::types::{RecallLayer, RecallRequest};
use ramaria_service::{DEFAULT_PERSONA_UID, Engine};
use ramaria_storage::SqliteStorage;
use serde_json::json;

use crate::support::{
    GoldenStore, ParityEnv, ParityError, ParityResult, ScriptedLlm, Snapshot, assert_stable,
    fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "index_lazy_load_and_refresh";

/// 封存脚本回复：摘要含"攀岩"（供增量可检索断言）。
const CLIMBING_L1_REPLY: &str = r#"{
  "summary": "用户这周开始学习攀岩，周末去了岩馆。",
  "keywords": "攀岩,周末",
  "time_period": "周末",
  "atmosphere": "新鲜",
  "valence": 0.6,
  "salience": 0.7,
  "situation_strength": 3
}"#;

/// 封存脚本回复：摘要含"加班"（供脏标记重建断言）。
const OVERTIME_L1_REPLY: &str = r#"{
  "summary": "用户最近连续加班，晚上常到十一点。",
  "keywords": "加班,工作",
  "time_period": "夜间",
  "atmosphere": "疲惫",
  "valence": -0.5,
  "salience": 0.8,
  "situation_strength": 4
}"#;

// =========================================================
// 场景执行
// =========================================================

/// 以 L1 层检索模式召回，返回命中的文本集合。
async fn recall_l1_texts(engine: &Engine, query: &str) -> ParityResult<Vec<String>> {
    let result = engine
        .recall(RecallRequest {
            query: Some(query.to_string()),
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .map_err(|e| ParityError::env(format!("召回 {query}"), e))?;
    Ok(result.items.into_iter().map(|item| item.text).collect())
}

/// 场景 A：懒加载 → 加载后增量 → 跨进程写入刷新。
///
/// 返回:
/// - `(首次构建, 二次构建, 加载状态, 增量命中, 跨进程命中)`。
async fn scenario_lazy_and_refresh(tag: &str) -> ParityResult<(bool, bool, bool, bool, bool)> {
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::replies(&[CLIMBING_L1_REPLY]));
    let env = ParityEnv::with_llm(tag, llm).await?;
    let engine = env.engine();

    fixtures::seed_persona(env.storage(), DEFAULT_PERSONA_UID).await?;
    fixtures::seed_l1(
        env.storage(),
        DEFAULT_PERSONA_UID,
        "用户最近在学游泳，每周去两次",
        Some("游泳"),
        fixtures::fixture_ts(0),
    )
    .await?;

    // ---- 懒加载：首次构建、二次免构建 ----
    let first_built = engine
        .ensure_index_loaded()
        .await
        .map_err(|e| ParityError::env("首次加载索引", e))?;
    let second_built = engine
        .ensure_index_loaded()
        .await
        .map_err(|e| ParityError::env("二次加载索引", e))?;
    let loaded = engine.is_retriever_loaded();

    // ---- 加载后增量：封存生成的新 L1 立即可检索（增量镜像，不重建整库） ----
    let session_id = fixtures::seed_active_session(
        env.storage(),
        DEFAULT_PERSONA_UID,
        4,
        fixtures::fixture_ts(1_000),
    )
    .await?;
    engine
        .seal(session_id)
        .await
        .map_err(|e| ParityError::env("加载后封存（增量镜像）", e))?;
    let incremental_hits = recall_l1_texts(engine, "攀岩").await?;
    let incremental_hit = incremental_hits.iter().any(|text| text.contains("攀岩"));

    // ---- 跨进程写入：第二个连接池写入 L1 → 代次刷新后可见 ----
    let other_storage = SqliteStorage::new(
        ramaria_storage::database::init_pool(Some(env.db_path().to_path_buf()))
            .await
            .map_err(|e| ParityError::env("创建第二个连接池", e))?,
    );
    fixtures::seed_l1(
        &other_storage,
        DEFAULT_PERSONA_UID,
        "用户最近开始夜跑，每周三次",
        Some("夜跑"),
        fixtures::fixture_ts(2_000),
    )
    .await?;
    let cross_process_hits = recall_l1_texts(engine, "夜跑").await?;
    let cross_process_hit = cross_process_hits.iter().any(|text| text.contains("夜跑"));

    env.cleanup().await;
    Ok((
        first_built,
        second_built,
        loaded,
        incremental_hit,
        cross_process_hit,
    ))
}

/// 场景 B：未加载窗口封存（脏标记）→ 加载时重建并找回该 L1。
///
/// 返回:
/// - `(加载是否触发构建, 该 L1 是否可检索)`。
async fn scenario_dirty_rebuild(tag: &str) -> ParityResult<(bool, bool)> {
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::replies(&[OVERTIME_L1_REPLY]));
    let env = ParityEnv::with_llm(tag, llm).await?;
    let engine = env.engine();

    fixtures::seed_persona(env.storage(), DEFAULT_PERSONA_UID).await?;
    let session_id = fixtures::seed_active_session(
        env.storage(),
        DEFAULT_PERSONA_UID,
        4,
        fixtures::fixture_ts(0),
    )
    .await?;

    // 索引尚未加载时封存：L1 写入存储并尝试增量镜像，此时内存索引缺席 → 置脏
    engine
        .seal(session_id)
        .await
        .map_err(|e| ParityError::env("未加载窗口封存", e))?;
    assert!(
        !engine.is_retriever_loaded(),
        "索引不应在封存过程中被意外加载"
    );

    // 脏标记应强制重建：加载返回 true，且封存期间产生的 L1 必须可检索（不漏）
    let rebuilt = engine
        .ensure_index_loaded()
        .await
        .map_err(|e| ParityError::env("脏标记后加载索引", e))?;
    let hits = recall_l1_texts(engine, "加班").await?;
    let hit = hits.iter().any(|text| text.contains("加班"));

    env.cleanup().await;
    Ok((rebuilt, hit))
}

/// 在给定库文件与标签上执行索引场景，产出规范化快照（内部使用两个隔离环境）。
async fn snapshot_of(tag: &str) -> ParityResult<Snapshot> {
    let (first_built, second_built, loaded, incremental_hit, cross_process_hit) =
        scenario_lazy_and_refresh(&format!("{tag}-lazy")).await?;
    let (dirty_rebuilt, dirty_hit) = scenario_dirty_rebuild(&format!("{tag}-dirty")).await?;

    Ok(Snapshot::new(
        SCENARIO,
        json!({
            "lazy": {
                "first_built": first_built,
                "second_built": second_built,
                "loaded": loaded,
            },
            "incremental_hit": incremental_hit,
            "cross_process_hit": cross_process_hit,
            "dirty": {
                "rebuilt": dirty_rebuilt,
                "hit": dirty_hit,
            },
        }),
    ))
}

// =========================================================
// 测试
// =========================================================

/// 基线一致：索引场景四个结论指标与冻结基线一致。
#[tokio::test]
async fn index_snapshot_matches_golden_baseline() {
    let snapshot = snapshot_of("index-golden")
        .await
        .expect("索引场景应执行成功");

    // 关键行为断言：懒加载只构建一次；增量 / 跨进程 / 脏标记三条路径均命中
    assert_eq!(
        snapshot.value()["lazy"]["first_built"].as_bool(),
        Some(true),
        "首次调用应完成懒加载构建"
    );
    assert_eq!(
        snapshot.value()["lazy"]["second_built"].as_bool(),
        Some(false),
        "二次调用不应重复构建"
    );
    assert_eq!(
        snapshot.value()["incremental_hit"].as_bool(),
        Some(true),
        "加载后封存的新 L1 应立即可检索"
    );
    assert_eq!(
        snapshot.value()["cross_process_hit"].as_bool(),
        Some(true),
        "跨进程写入的 L1 应经代次刷新后可检索"
    );
    assert_eq!(
        snapshot.value()["dirty"]["hit"].as_bool(),
        Some(true),
        "加载窗口内的 L1 不应漏检索（脏标记重建）"
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
        "索引基线比对完成"
    );
}

/// 确定性：两组隔离环境各自执行索引场景，输出应完全一致。
#[tokio::test]
async fn index_snapshot_is_stable_across_env_groups() {
    let first = snapshot_of("index-stable-a")
        .await
        .expect("首轮索引场景应执行成功");
    let replay = snapshot_of("index-stable-b")
        .await
        .expect("重复索引场景应执行成功");
    assert_stable("index/lazy-load-and-refresh", &first, &replay);
}
