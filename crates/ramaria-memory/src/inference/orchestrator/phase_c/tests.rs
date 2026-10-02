//! crates/ramaria-memory/src/inference/orchestrator/phase_c/tests.rs - //! crates/ramaria-memory/src/inference/orchestrator/phase_c.rs - Phase C 置信度更新 + 漂移检测编排单元测试
//!
//! 设计特点:
//! - 位于 inference::orchestrator::phase_c 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 phase_c.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{ClusterSnapshot, MemoryEvent, Persona, PersonaKind, Presentation};
use ramaria_storage::SqliteStorage;

/// 固定测试基准时间（Unix 毫秒），保证用例不依赖真实时钟、连续运行结果一致。
const TEST_NOW_MS: i64 = 1_760_000_000_000;

/// 创建内存 SQLite 存储（跑 2.0 基线 migration，外键约束开启）。
async fn mem_storage() -> SqliteStorage {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("内存测试数据库创建失败");
    sqlx::migrate!("../ramaria-storage/migrations")
        .run(&pool)
        .await
        .expect("测试 migration 失败");
    SqliteStorage::new(pool)
}

/// 插入 persona 行，满足 persona_cluster_snapshots 的外键约束。
async fn insert_persona(storage: &SqliteStorage, uid: &str) {
    let persona = Persona::new(
        uid.into(),
        "测试人格".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("插入 persona fixture 应成功");
}

/// 构造分类级快照聚合 samples JSON（与 L3 Phase A 持久化格式一致）。
fn snapshot_samples(category: &str, n_eff: f64, valence_mean: f64, share_mean: f64) -> String {
    serde_json::json!({
        "category": category,
        "event_count": n_eff as u64,
        "n_effective": n_eff,
        "valence_mean": valence_mean,
        "valence_std": 0.1,
        "share_mean": share_mean,
    })
    .to_string()
}

/// 构造测试事件：keywords 首标签即分类，valence/share/confidence/salience 由参数给定。
fn make_event(id: i64, keywords: &str, valence: f64, share: f64) -> MemoryEvent {
    let now = TEST_NOW_MS;
    let mut ev = MemoryEvent::new(
        "persona-drift".into(),
        format!("事件 {id}"),
        format!("摘要 {id}"),
        now - 1000,
        now,
    );
    ev.id = id;
    ev.keywords = Some(keywords.into());
    ev.valence = valence;
    ev.share = share;
    ev.salience = 0.5;
    ev.confidence = 0.9;
    ev.presentation = Presentation::Mixed;
    ev
}

/// 写入一期旧快照（模拟上一轮已吸收画像分布）。
async fn seed_snapshot(storage: &SqliteStorage, uid: &str, category: &str, samples: String) {
    let snap = ClusterSnapshot {
        id: 0,
        persona_uid: uid.to_string(),
        category: category.to_string(),
        cluster_label: format!("cluster_{category}"),
        samples: Some(samples),
        count: 10,
        is_current: true,
        created_at: TEST_NOW_MS,
        semantic_label: None,
        semantic_label_embedding: None,
    };
    storage
        .save_cluster_snapshot(&snap)
        .await
        .expect("写入快照 fixture 应成功");
}

/// 两期分布差异显著（上一轮 valence≈0.8、本轮≈0.1）→ 该分类触发重审。
#[tokio::test]
async fn detect_drift_triggers_on_two_round_snapshot_shift() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift").await;
    seed_snapshot(
        &storage,
        "persona-drift",
        "工作",
        snapshot_samples("工作", 10.0, 0.8, 0.6),
    )
    .await;

    let events: Vec<MemoryEvent> = (0..10)
        .map(|i| make_event(i, "工作,会议", 0.1, 0.6))
        .collect();
    let summary =
        detect_and_summarize_drift(&storage, "persona-drift", &events, &DriftConfig::default())
            .await
            .expect("漂移检测应成功");

    let work = summary
        .categories
        .iter()
        .find(|c| c.category == "工作")
        .expect("工作分类应进入检测");
    assert!(work.needs_review, "valence 从 0.8 大幅降至 0.1 应触发漂移");
    assert!(summary.any_drift);
    assert_eq!(summary.skipped_count, 0, "有效对比不应产生跳过");
}

/// 两期分布基本一致 → 不触发重审。
#[tokio::test]
async fn detect_drift_no_trigger_when_distributions_similar() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift").await;
    seed_snapshot(
        &storage,
        "persona-drift",
        "家庭",
        snapshot_samples("家庭", 10.0, 0.4, 0.7),
    )
    .await;

    let events: Vec<MemoryEvent> = (0..10)
        .map(|i| make_event(i, "家庭,陪伴", 0.4, 0.7))
        .collect();
    let summary =
        detect_and_summarize_drift(&storage, "persona-drift", &events, &DriftConfig::default())
            .await
            .expect("漂移检测应成功");

    assert!(!summary.any_drift, "相同分布不应触发漂移");
    assert_eq!(summary.categories[0].category, "家庭");
    assert!(!summary.categories[0].needs_review);
}

/// 旧分布缺失（新分类、无历史快照）→ 该分类显式跳过并计数。
#[tokio::test]
async fn detect_drift_skips_category_missing_old_snapshot() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift").await;
    // 未写入任何快照
    let events: Vec<MemoryEvent> = (0..5)
        .map(|i| make_event(i, "社交,聚会", -0.2, 0.5))
        .collect();
    let summary =
        detect_and_summarize_drift(&storage, "persona-drift", &events, &DriftConfig::default())
            .await
            .expect("漂移检测应成功");

    assert!(summary.categories.is_empty(), "缺失旧分布不应产出检测结果");
    assert!(!summary.any_drift);
    assert_eq!(summary.skipped_count, 1, "应显式跳过并标注 1 个分类");
}

/// restore_real_distribution=false → 漂移检测整体显式跳过，不生成占位假数据。
#[tokio::test]
async fn detect_drift_disabled_skips_explicitly() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift").await;
    seed_snapshot(
        &storage,
        "persona-drift",
        "工作",
        snapshot_samples("工作", 10.0, 0.8, 0.6),
    )
    .await;

    let events: Vec<MemoryEvent> = (0..10)
        .map(|i| make_event(i, "工作,会议", 0.1, 0.6))
        .collect();
    let cfg = DriftConfig {
        restore_real_distribution: false,
        ..DriftConfig::default()
    };
    let summary = detect_and_summarize_drift(&storage, "persona-drift", &events, &cfg)
        .await
        .expect("漂移检测应成功");

    assert!(
        summary.categories.is_empty(),
        "关闭真实恢复后不应产出检测结果"
    );
    assert!(!summary.any_drift);
    assert_eq!(
        summary.skipped_count, 1,
        "关闭开关应显式跳过全部候选分类（此例 1 个）"
    );
}

/// 旧分布无判别信息（valence/share 解析全零）→ 该分类显式跳过并计数，
/// 不产出假性漂移（空数据守卫，不 panic、不阻塞主流程）。
#[tokio::test]
async fn detect_drift_skips_category_when_old_distribution_all_zero() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift-zero").await;
    seed_snapshot(
        &storage,
        "persona-drift-zero",
        "工作",
        snapshot_samples("工作", 10.0, 0.0, 0.0), // valence/share 全零
    )
    .await;

    let events: Vec<MemoryEvent> = (0..5)
        .map(|i| make_event(i, "工作,会议", 0.3, 0.5))
        .collect();
    let summary = detect_and_summarize_drift(
        &storage,
        "persona-drift-zero",
        &events,
        &DriftConfig::default(),
    )
    .await
    .expect("漂移检测应成功");

    assert!(summary.categories.is_empty(), "全零旧分布不应产出检测结果");
    assert!(!summary.any_drift);
    assert_eq!(summary.skipped_count, 1, "应显式跳过并标注 1 个分类");
}

/// 空 new_traits（如 LLM 空数组响应下游）→ Phase C 早退：不 panic、零更新、
/// 不执行漂移检测（结构化返回全空，调用方据此不阻塞事件吸收）。
#[tokio::test]
async fn phase_c_empty_new_traits_returns_empty_no_drift() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-empty-pc").await;

    let result = run_phase_c_update(
        &ConfidenceConfig::default(),
        &DriftConfig::default(),
        &storage,
        "persona-empty-pc",
        &[],
        &[],
        false,
    )
    .await
    .expect("空 new_traits 不应报错");

    assert_eq!(result.traits_updated, 0);
    assert_eq!(result.evidence_saved, 0);
    assert!(!result.has_significant_drift);
    assert!(result.drift_categories.is_empty());
    assert!(result.confidence_summary.is_none());
    assert!(result.drift_summary.is_none());
}

/// 无活跃 trait（storage 无该 persona 的任何 trait）且 new_traits 非空 →
/// 活性过滤后为空 → 早退（不 panic、零更新）。
#[tokio::test]
async fn phase_c_no_active_traits_returns_empty() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-noactive").await;
    // 不写入任何 trait：活性过滤结果为空

    let trait_fixture = ramaria_core::types::PersonalityTrait {
        id: 0,
        persona_uid: "persona-noactive".into(),
        layer: ramaria_core::types::TraitLayer::Base,
        trait_label: "尽责".into(),
        meaning: "测试".into(),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 0,
        source: ramaria_core::types::TraitSource::Inferred,
        ref_event_id: None,
        ref_l1_id: None,
        confidence: 0.5,
        evidence: 1.0,
        consistency: 0.5,
        status: ramaria_core::types::TraitStatus::Active,
        created_at: TEST_NOW_MS,
        updated_at: TEST_NOW_MS,
    };

    let result = run_phase_c_update(
        &ConfidenceConfig::default(),
        &DriftConfig::default(),
        &storage,
        "persona-noactive",
        &[trait_fixture],
        &[],
        false,
    )
    .await
    .expect("无活跃 trait 不应报错");

    assert_eq!(result.traits_updated, 0);
    assert_eq!(result.evidence_saved, 0);
    assert!(!result.has_significant_drift);
    assert!(result.drift_summary.is_none());
}

// =========================================================
// Phase C 编排：首轮判定与漂移检测衔接
// =========================================================

/// 构造并落库一个活跃 Base trait fixture，返回存储层回填 id 后的 trait。
///
/// 说明:
/// - 对应 Phase B 产出的 trait 列表元素；`run_phase_c_update` 的实际数据源
///   仍是 storage 中已落库的活跃 trait，本 fixture 保证非空且可被加载。
async fn seed_active_trait_fixture(storage: &SqliteStorage, uid: &str) -> PersonalityTrait {
    let trait_fixture = PersonalityTrait {
        id: 0,
        persona_uid: uid.into(),
        layer: ramaria_core::types::TraitLayer::Base,
        trait_label: "尽责".into(),
        meaning: "对任务有强烈的完成意愿".into(),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 0,
        source: ramaria_core::types::TraitSource::Inferred,
        ref_event_id: None,
        ref_l1_id: None,
        confidence: 0.5,
        evidence: 1.0,
        consistency: 0.5,
        status: TraitStatus::Active,
        created_at: TEST_NOW_MS,
        updated_at: TEST_NOW_MS,
    };
    let id = storage
        .save_trait(&trait_fixture)
        .await
        .expect("落库 trait fixture 应成功");
    PersonalityTrait {
        id,
        ..trait_fixture
    }
}

/// Keep-only 轮不豁免漂移检测：Phase B 产出 trait_ids 非空即非首轮，
/// 即使标签/含义未变（traits_updated 为 0），is_first_round=false 时仍必须
/// 继续执行漂移检测，锁定"分布已漂移但标签未变"的轮次不被跳过。
#[tokio::test]
async fn phase_c_non_first_round_runs_drift_detection() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift").await;
    let trait_fixture = seed_active_trait_fixture(&storage, "persona-drift").await;
    seed_snapshot(
        &storage,
        "persona-drift",
        "工作",
        snapshot_samples("工作", 10.0, 0.8, 0.6),
    )
    .await;

    let events: Vec<MemoryEvent> = (0..10)
        .map(|i| make_event(i, "工作,会议", 0.1, 0.6))
        .collect();

    let result = run_phase_c_update(
        &ConfidenceConfig::default(),
        &DriftConfig::default(),
        &storage,
        "persona-drift",
        std::slice::from_ref(&trait_fixture),
        &events,
        false,
    )
    .await
    .expect("非首轮 Phase C 更新应成功");

    assert!(result.drift_summary.is_some(), "非首轮必须执行漂移检测");
    assert!(
        result.has_significant_drift,
        "旧分布 valence≈0.8、本轮≈0.1，应判定显著漂移"
    );
    assert!(
        result.drift_categories.contains(&"工作".to_string()),
        "工作分类应进入重审列表"
    );
}

/// 首轮（Phase B 未产出任何 trait）无旧画像分布可对比：跳过漂移检测，
/// 结构化返回空漂移结果，不阻塞主流程；即便库中已有快照也不参与检测。
#[tokio::test]
async fn phase_c_first_round_skips_drift_detection() {
    let storage = mem_storage().await;
    insert_persona(&storage, "persona-drift").await;
    let trait_fixture = seed_active_trait_fixture(&storage, "persona-drift").await;
    seed_snapshot(
        &storage,
        "persona-drift",
        "工作",
        snapshot_samples("工作", 10.0, 0.8, 0.6),
    )
    .await;

    let events: Vec<MemoryEvent> = (0..10)
        .map(|i| make_event(i, "工作,会议", 0.1, 0.6))
        .collect();

    let result = run_phase_c_update(
        &ConfidenceConfig::default(),
        &DriftConfig::default(),
        &storage,
        "persona-drift",
        std::slice::from_ref(&trait_fixture),
        &events,
        true,
    )
    .await
    .expect("首轮 Phase C 更新应成功");

    assert!(
        result.drift_summary.is_none(),
        "首轮无旧分布，不应执行漂移检测"
    );
    assert!(!result.has_significant_drift, "首轮不应产生漂移结论");
}
