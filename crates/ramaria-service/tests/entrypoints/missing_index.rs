//! crates/ramaria-service/tests/entrypoints/missing_index.rs - 缺索引版本自愈用例
//!
//! 设计特点:
//! - 构造"缺键库"（`schema_meta` 中 `index_version` 键删除）：缺键按未构建（0）判定，
//!   `needs_indexing = true` → 状态为 `Indexing`
//! - 覆盖两条入口自愈路径：启动路径（`ensure_index_loaded` + 刷新状态）与
//!   CLI 重建路径（`rebuild_index` + 刷新状态）；两者均写回索引版本 1
//! - 无嵌入环境验证降级：重建不阻塞，状态推进到 `Degraded`

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig};

use crate::support::{DeterministicEmbedding, ParityError, ParityResult, ScriptedLlm, TestDb};

/// 脚本回复：封存 / 重建涉及会话的摘要（本用例只关心索引版本与状态推进）。
const L1_JSON: &str = r#"{
  "summary": "用户最近在学游泳，每周去两次。",
  "keywords": "游泳",
  "time_period": "近期",
  "atmosphere": "放松",
  "valence": 0.3,
  "salience": 0.5,
  "situation_strength": 2
}"#;

/// 删除 `schema_meta` 中的键（第二个连接池，模拟"缺键由库外因素产生"）。
async fn delete_schema_meta_key(db: &TestDb, key: &str) -> ParityResult<()> {
    let pool = db.open_pool().await?;
    sqlx::query("DELETE FROM schema_meta WHERE key = ?")
        .bind(key)
        .execute(&pool)
        .await
        .map_err(|e| ParityError::env(format!("删除 schema_meta 键 {key}"), e))?;
    Ok(())
}

/// 启动路径自愈：缺键库判定 `Indexing` → 一次构建写回版本 → 嵌入可用时推进 `Ready`。
#[tokio::test]
async fn missing_index_version_self_heals_on_startup_flow() {
    const PERSONA: &str = "char-entry-missing-index";

    let db = TestDb::new("entry-missing-index-startup");
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
    let (engine, storage) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(L1_JSON)),
            Some(embedding),
            RamariaConfig::default(),
        )
        .await
        .expect("引擎应可装配");

    crate::support::fixtures::seed_persona(storage.as_ref(), PERSONA)
        .await
        .expect("种子人格应写入成功");
    crate::support::fixtures::seed_l1(
        storage.as_ref(),
        PERSONA,
        "用户最近在学游泳，每周去两次",
        Some("游泳"),
        crate::support::fixtures::fixture_ts(0),
    )
    .await
    .expect("种子 L1 应写入成功");
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("后端配置应写入成功");

    // 正常库（migration 预置索引版本）：不判待构建
    let before = engine.check_setup_status().await.expect("诊断应成功");
    assert!(
        !before.needs_indexing,
        "migration 预置索引版本时不应判待构建（实际 {before:?}）"
    );

    // 构造缺键库
    delete_schema_meta_key(&db, "index_version")
        .await
        .expect("删除索引版本键应成功");

    let status = engine.check_setup_status().await.expect("诊断应成功");
    assert!(status.backend_configured, "后端配置应已就绪");
    assert!(status.model_selected, "本地 provider 模型选择应视为完成");
    assert!(
        status.needs_indexing,
        "缺键应按未构建判定（needs_indexing=true）"
    );
    assert!(status.embedding_available, "确定性嵌入应判定可用");
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新状态应成功"),
        AppState::Indexing,
        "缺键库刷新后状态应为 Indexing"
    );

    // ---- 启动自愈：Indexing → 一次构建 + 刷新状态 ----
    let built = engine
        .ensure_index_loaded()
        .await
        .expect("缺键库启动自愈应完成构建");
    assert!(built, "索引此前未加载，首次调用应完成构建");
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新状态应成功"),
        AppState::Ready,
        "嵌入可用时构建完成后应推进到 Ready"
    );
    assert_eq!(
        storage
            .get_index_version()
            .await
            .expect("读取索引版本应成功"),
        1,
        "构建完成后应写回索引版本 1"
    );
    assert!(
        !engine
            .check_setup_status()
            .await
            .expect("诊断应成功")
            .needs_indexing,
        "自愈完成后不应再判待构建"
    );

    db.cleanup().await;
}

/// CLI 重建路径自愈：缺键库 + 无嵌入 → 重建不阻塞，状态推进到 `Degraded`（对话可用）。
#[tokio::test]
async fn missing_index_version_self_heals_on_cli_rebuild_path() {
    const PERSONA: &str = "char-entry-missing-index-cli";

    let db = TestDb::new("entry-missing-index-cli");
    let (engine, storage) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(L1_JSON)),
            None,
            RamariaConfig::default(),
        )
        .await
        .expect("引擎应可装配（无嵌入降级）");

    crate::support::fixtures::seed_persona(storage.as_ref(), PERSONA)
        .await
        .expect("种子人格应写入成功");
    crate::support::fixtures::seed_l1(
        storage.as_ref(),
        PERSONA,
        "用户最近在学游泳，每周去两次",
        Some("游泳"),
        crate::support::fixtures::fixture_ts(0),
    )
    .await
    .expect("种子 L1 应写入成功");
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("后端配置应写入成功");
    assert!(
        !engine.is_embedding_available(),
        "本用例应运行在嵌入缺失的降级环境"
    );

    delete_schema_meta_key(&db, "index_version")
        .await
        .expect("删除索引版本键应成功");
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新状态应成功"),
        AppState::Indexing,
        "缺键库刷新后状态应为 Indexing"
    );

    // ---- CLI 重建自愈：rebuild + 刷新状态（对话前 / 索引重建命令共用口径） ----
    let total = engine
        .rebuild_index()
        .await
        .expect("缺键库重建应成功（嵌入缺失不阻塞构建）");
    assert_eq!(total, 1, "重建应加载 1 条未吸收 L1（实际 {total}）");
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新状态应成功"),
        AppState::Degraded,
        "嵌入缺失时构建完成后应降级（对话可用，向量通道缺席）"
    );
    assert_eq!(
        storage
            .get_index_version()
            .await
            .expect("读取索引版本应成功"),
        1,
        "重建完成后应写回索引版本 1"
    );

    db.cleanup().await;
}
