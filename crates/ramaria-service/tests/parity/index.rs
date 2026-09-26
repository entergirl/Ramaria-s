//! crates/ramaria-service/tests/parity/index.rs - 对照路径：检索索引（懒加载与刷新）
//!
//! 设计特点:
//! - 覆盖合并后的索引语义要点：懒加载（只构建一次）/ 加载后增量可检索 /
//!   跨进程写入按代次刷新 / 未加载窗口的脏标记重建 / 显式重建（写回索引版本）
//! - 跨进程等价物：在同一库文件上开第二个连接池写入 L1，模拟"桌面写、MCP 读"场景，
//!   不引入真实多进程
//! - 快照只含布尔与计数：各场景的结论指标；索引内部结构与向量不落盘
//! - fixture 时间固定偏移：保证跨进程写入的 L1 与本地语料戳判定稳定
//! - 输出入口：`snapshot_of` 是"某一实现在该 fixture 上的规范化输出"的唯一入口，
//!   同形状快照可直接送入 `assert_parity` 比对

use std::sync::Arc;

use ramaria_core::traits::{LlmProvider, StoreInfrastructure};
use ramaria_service::types::{RecallLayer, RecallRequest};
use ramaria_service::{DEFAULT_PERSONA_UID, Engine};
use ramaria_storage::SqliteStorage;
use serde_json::json;

use crate::support::{
    AppEnv, GoldenStore, ParityEnv, ParityError, ParityResult, ScriptedLlm, Snapshot,
    assert_parity, assert_stable, fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "index_lazy_load_and_refresh";

/// 逐字对照场景名（快照标签，不写基线）。
const CROSS_SCENARIO: &str = "index/app-vs-service";

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

/// 场景 C：显式全量重建（`rebuild_index`）→ 文档数 / 告警位 / 检索命中 / 索引版本。
///
/// 返回:
/// - `(文档总数, 告警位, 检索命中, 索引版本)`。
async fn scenario_explicit_rebuild(tag: &str) -> ParityResult<(usize, bool, bool, i32)> {
    let env = ParityEnv::new(tag).await?;
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
    fixtures::seed_l1(
        env.storage(),
        DEFAULT_PERSONA_UID,
        "用户最近开始夜跑，每周三次",
        Some("夜跑"),
        fixtures::fixture_ts(1_000),
    )
    .await?;

    // 显式置 0（"尚未构建"）→ 重建完成后应写回 1
    env.storage()
        .set_index_version(0)
        .await
        .map_err(|e| ParityError::env("写入索引版本", e))?;

    let total = engine
        .rebuild_index()
        .await
        .map_err(|e| ParityError::env("显式重建索引", e))?;
    let failed = engine.is_index_rebuild_failed();
    let hits = recall_l1_texts(engine, "夜跑").await?;
    let hit = hits.iter().any(|text| text.contains("夜跑"));
    let index_version = env
        .storage()
        .get_index_version()
        .await
        .map_err(|e| ParityError::env("读取索引版本", e))?;

    env.cleanup().await;
    Ok((total, failed, hit, index_version))
}

/// 在给定库文件与标签上执行索引场景，产出规范化快照（内部使用三个隔离环境）。
async fn snapshot_of(tag: &str) -> ParityResult<Snapshot> {
    let (first_built, second_built, loaded, incremental_hit, cross_process_hit) =
        scenario_lazy_and_refresh(&format!("{tag}-lazy")).await?;
    let (dirty_rebuilt, dirty_hit) = scenario_dirty_rebuild(&format!("{tag}-dirty")).await?;
    let (rebuild_total, rebuild_failed, rebuild_hit, rebuild_index_version) =
        scenario_explicit_rebuild(&format!("{tag}-rebuild")).await?;

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
            "rebuild": {
                "total": rebuild_total,
                "failed": rebuild_failed,
                "hit": rebuild_hit,
                "index_version": rebuild_index_version,
            },
        }),
    ))
}

// =========================================================
// 测试
// =========================================================

/// 基线一致：索引场景各结论指标与冻结基线一致。
#[tokio::test]
async fn index_snapshot_matches_golden_baseline() {
    let snapshot = snapshot_of("index-golden")
        .await
        .expect("索引场景应执行成功");

    // 关键行为断言：懒加载只构建一次；增量 / 跨进程 / 脏标记 / 显式重建各路径结论正确
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
    assert_eq!(
        snapshot.value()["rebuild"]["total"].as_u64(),
        Some(2),
        "显式重建返回的文档数应为加载的 L1 总数"
    );
    assert_eq!(
        snapshot.value()["rebuild"]["failed"].as_bool(),
        Some(false),
        "显式重建成功后告警位应为 false"
    );
    assert_eq!(
        snapshot.value()["rebuild"]["hit"].as_bool(),
        Some(true),
        "显式重建后写入的 L1 应可检索"
    );
    assert_eq!(
        snapshot.value()["rebuild"]["index_version"].as_i64(),
        Some(1),
        "显式重建应写回索引版本 1"
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

// =========================================================
// 逐字对照（应用装配 vs 服务装配）
// =========================================================

/// 造逐字对照 fixture：persona 与 2 条关键词不同的 L1。
async fn seed_cross_fixture(storage: &SqliteStorage) -> ParityResult<()> {
    fixtures::seed_persona(storage, DEFAULT_PERSONA_UID).await?;
    fixtures::seed_l1(
        storage,
        DEFAULT_PERSONA_UID,
        "用户最近在学游泳，每周去两次",
        Some("游泳"),
        fixtures::fixture_ts(0),
    )
    .await?;
    fixtures::seed_l1(
        storage,
        DEFAULT_PERSONA_UID,
        "用户最近开始夜跑，每周三次",
        Some("夜跑"),
        fixtures::fixture_ts(1_000),
    )
    .await?;
    Ok(())
}

/// 读取重建结果快照（两侧同形）。
async fn rebuild_value(
    storage: &SqliteStorage,
    doc_total: usize,
    rebuild_failed: bool,
) -> ParityResult<serde_json::Value> {
    let index_version = storage
        .get_index_version()
        .await
        .map_err(|e| ParityError::env("读取索引版本", e))?;
    let bm25_index_version = storage
        .get_bm25_index_version()
        .await
        .map_err(|e| ParityError::env("读取 BM25 分词版本", e))?;
    Ok(json!({
        "doc_total": doc_total,
        "index_version": index_version,
        "bm25_index_version": bm25_index_version,
        "rebuild_failed": rebuild_failed,
    }))
}

/// 应用装配：全量重建内存检索索引。
async fn cross_snapshot_app(env: &AppEnv) -> ParityResult<Snapshot> {
    seed_cross_fixture(env.storage()).await?;
    // 两侧重建前显式对齐索引版本（同一"已构建"起始状态）
    env.storage()
        .set_index_version(1)
        .await
        .map_err(|e| ParityError::env("对齐索引版本", e))?;
    let total = env
        .app()
        .rebuild_retriever()
        .await
        .map_err(|e| ParityError::env("应用装配重建索引", e))?;
    let failed = env.app().is_retriever_rebuild_failed();
    let value = rebuild_value(env.storage(), total, failed).await?;
    Ok(Snapshot::new(CROSS_SCENARIO, value))
}

/// 服务装配：全量重建内存检索索引。
async fn cross_snapshot_service(env: &ParityEnv) -> ParityResult<Snapshot> {
    seed_cross_fixture(env.storage()).await?;
    // 两侧重建前显式对齐索引版本（同一"已构建"起始状态）
    env.storage()
        .set_index_version(1)
        .await
        .map_err(|e| ParityError::env("对齐索引版本", e))?;
    let total = env
        .engine()
        .rebuild_index()
        .await
        .map_err(|e| ParityError::env("服务装配重建索引", e))?;
    let failed = env.engine().is_index_rebuild_failed();
    let value = rebuild_value(env.storage(), total, failed).await?;
    Ok(Snapshot::new(CROSS_SCENARIO, value))
}

/// 逐字对照：索引重建的文档数与版本状态在两侧等价。
///
/// 口径说明:
/// - 两侧为同一 fixture（同一人格 + 2 条关键词不同的 L1），各以全量重建路径构建索引；
/// - 快照取重建返回值与存储可读的版本 / 告警位（不比较索引内部结构与向量）；
/// - 索引版本在两侧重建前显式对齐为同一已构建状态，对照只看重建后的读取值。
#[tokio::test]
async fn index_rebuild_outputs_are_equivalent_between_app_and_service() {
    let app_env = AppEnv::new("index-cross-app")
        .await
        .expect("应用装配对照环境应可构建");
    let left = cross_snapshot_app(&app_env)
        .await
        .expect("应用装配重建场景应执行成功");
    app_env.cleanup().await;

    let service_env = ParityEnv::new("index-cross-service")
        .await
        .expect("服务装配对照环境应可构建");
    let right = cross_snapshot_service(&service_env)
        .await
        .expect("服务装配重建场景应执行成功");
    service_env.cleanup().await;

    // 关键行为锚点：2 条 L1 全部进入索引、重建成功且版本处于已构建态（对照面成立）
    assert_eq!(
        left.value()["doc_total"].as_u64(),
        Some(2),
        "2 条 L1 应全部进入索引"
    );
    assert_eq!(
        left.value()["rebuild_failed"].as_bool(),
        Some(false),
        "重建应成功"
    );
    assert_eq!(
        left.value()["index_version"].as_i64(),
        Some(1),
        "重建后索引版本应处于已构建态"
    );

    assert_parity(CROSS_SCENARIO, &left, &right);
}
