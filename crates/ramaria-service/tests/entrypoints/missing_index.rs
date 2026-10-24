//! crates/ramaria-service/tests/entrypoints/missing_index.rs - 首启状态与索引构建解耦用例
//!
//! 设计特点:
//! - 新库首启：设置完成判定只看后端配置与模型选择，与索引是否构建无关
//! - 索引构建可独立完成（加载、重建），不改变应用状态
//! - 无嵌入环境验证降级：状态为 `Degraded`，重建不阻塞且状态不变

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig};

use crate::support::{DeterministicEmbedding, ParityResult, ScriptedLlm, TestDb};

/// 脚本回复：封存 / 重建涉及会话的摘要（本用例只关心状态与索引构建）。
const L1_JSON: &str = r#"{
  "summary": "用户最近在学游泳，每周去两次。",
  "keywords": "游泳",
  "time_period": "近期",
  "atmosphere": "放松",
  "valence": 0.3,
  "salience": 0.5,
  "situation_strength": 2
}"#;

/// 新库首启：设置完成判定只看后端配置与模型选择；索引构建不改变状态。
#[tokio::test]
async fn fresh_db_state_depends_only_on_backend_and_model() -> ParityResult<()> {
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

    // 后端未配置：未完成，状态 NeedsSetup
    let status = engine.check_setup_status().await.expect("诊断应成功");
    assert!(!status.is_complete(), "后端未配置时应判未完成");

    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("后端配置应写入成功");
    engine
        .setting_set(ramaria_service::SETUP_COMPLETED_KEY, "1")
        .await
        .expect("完成标记应写入成功");

    // 完成标记 + 后端 + 模型就绪即判完成（与索引是否构建无关）
    let status = engine.check_setup_status().await.expect("诊断应成功");
    assert!(status.backend_configured, "后端配置应已就绪");
    assert!(status.model_selected, "本地 provider 模型选择应视为完成");
    assert!(status.embedding_available, "确定性嵌入应判定可用");
    assert!(status.is_complete(), "后端与模型就绪即应判完成");
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新状态应成功"),
        AppState::Ready,
        "核心配置与嵌入均就绪时状态应为 Ready"
    );

    // 索引构建独立完成，不改变应用状态
    let built = engine
        .ensure_index_loaded()
        .await
        .expect("新库首启应完成一次构建");
    assert!(built, "索引此前未加载，首次调用应完成构建");
    assert_eq!(
        engine.current_state(),
        AppState::Ready,
        "索引构建不应改变应用状态"
    );

    db.cleanup().await;
    Ok(())
}

/// 无嵌入环境：状态为 Degraded；索引重建可完成且不改变状态。
#[tokio::test]
async fn rebuild_without_embedding_keeps_degraded_state() -> ParityResult<()> {
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
    engine
        .setting_set(ramaria_service::SETUP_COMPLETED_KEY, "1")
        .await
        .expect("完成标记应写入成功");
    assert!(
        !engine.is_embedding_available(),
        "本用例应运行在嵌入缺失的降级环境"
    );
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新状态应成功"),
        AppState::Degraded,
        "嵌入缺失时应降级（对话可用，向量通道缺席）"
    );

    let total = engine
        .rebuild_index()
        .await
        .expect("嵌入缺失不应阻塞索引重建");
    assert_eq!(total, 1, "重建应加载 1 条未吸收 L1（实际 {total}）");
    assert_eq!(
        engine.current_state(),
        AppState::Degraded,
        "索引重建不应改变应用状态"
    );

    db.cleanup().await;
    Ok(())
}
