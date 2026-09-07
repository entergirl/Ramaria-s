//! crates/ramaria-app/tests/fact_extract_integration.rs - auto_fact_detect 事实抽取编排集成测试
//!
//! 设计特点:
//! - 直接驱动 `run_fact_extraction`（mock storage + 合成 MemoryL1/MemoryEvent，无真实 LLM）
//! - 覆盖验收：开关关闭回退 v1.7；策略①主观隐含；策略②L1 线索→断言；策略③互证 Promote；
//!   幂等重跑；仲裁红线（单事件 stable 不提升、极性冲突不提升、persona 隔离）。
//! - 所有断言只读 mock storage 状态，不触碰真实库。

mod mock_backend;

use mock_backend::MockStorage;
use ramaria_app::app_fact_extract::run_fact_extraction;
use ramaria_core::config::KnowledgeConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    EvidenceNote, FactStatus, FactTier, MemoryEvent, MemoryL1, PersonaFact, Presentation,
    ProfileField,
};
use uuid::Uuid;

// =========================================================
// 构造辅助
// =========================================================

/// 打开 auto_fact_detect 的知识层配置。
fn enabled_knowledge() -> KnowledgeConfig {
    KnowledgeConfig {
        auto_fact_detect: true,
        ..Default::default()
    }
}

/// 构造一条 MemoryEvent（id 由调用方指定；入库时 mock 会重新分配真实 id）。
#[allow(clippy::too_many_arguments)]
fn event(
    persona_uid: &str,
    _id: i64,
    title: &str,
    summary: &str,
    keywords: &str,
    confidence: f64,
    presentation: Presentation,
    valence: f64,
) -> MemoryEvent {
    let mut e = MemoryEvent::new(
        persona_uid.to_string(),
        title.to_string(),
        summary.to_string(),
        1_700_000_000_000,
        1_700_000_000_000,
    );
    e.id = _id;
    e.keywords = Some(keywords.to_string());
    e.confidence = confidence;
    e.salience = 0.8;
    e.valence = valence;
    e.share = 0.5;
    e.presentation = presentation;
    e
}

/// 构造一条带 evidence_notes 的 L1（persona 已归属）。
fn l1_with_evidence(persona_uid: &str, text: &str) -> MemoryL1 {
    let mut l1 = MemoryL1::new(Uuid::new_v4(), "会话摘要".to_string(), None);
    l1.persona_uid = Some(persona_uid.to_string());
    l1.evidence_notes = Some(vec![EvidenceNote::new(text)]);
    l1
}

/// 读取 persona 全部事实（含 candidate/superseded，mock 提供全状态查询）。
async fn all_facts(
    storage: &MockStorage,
    persona_uid: &str,
    field: ProfileField,
) -> Vec<PersonaFact> {
    storage
        .list_facts_by_persona(persona_uid, field)
        .await
        .unwrap()
}

/// 读取 persona active 事实。
async fn active_facts(storage: &MockStorage, persona_uid: &str) -> Vec<PersonaFact> {
    storage
        .list_active_facts_by_persona(persona_uid)
        .await
        .unwrap()
}

// =========================================================
// (a) 开关关闭 → 零抽取/零写入（回退 v1.7）
// =========================================================

#[tokio::test]
async fn auto_fact_detect_off_gates_all_extraction() {
    let storage = MockStorage::new();
    let cfg = KnowledgeConfig::default(); // auto_fact_detect=false

    let subjective = event(
        "char-0001",
        1,
        "坚持跑步好开心",
        "提到坚持跑步后心情很开心",
        "跑步,开心",
        0.7,
        Presentation::Subjective,
        0.6,
    );
    let l1 = l1_with_evidence("char-0001", "每周都要加班到很晚");

    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[l1], &[subjective], None).await;

    assert!(report.gated_off, "开关关闭应标记 gated_off");
    assert_eq!(report.regular_candidates, 0);
    assert_eq!(report.implied_candidates, 0);
    assert_eq!(report.l1_candidates, 0);
    assert_eq!(report.deduped, 0);
    assert_eq!(report.promoted_active, 0);
    assert_eq!(report.candidates_saved, 0);

    let active = active_facts(&storage, "char-0001").await;
    assert!(
        active.is_empty(),
        "开关关闭不应写入任何事实（v1.7 回退锁定）"
    );
}

// =========================================================
// (b) 策略① 主观/低置信事件 → 字段感知隐含候选（落 candidate）
// =========================================================

#[tokio::test]
async fn subjective_event_implied_candidate_field_aware() {
    let storage = MockStorage::new();
    let cfg = enabled_knowledge();

    let subjective = event(
        "char-0001",
        1,
        "好喜欢看科幻电影",
        "提到喜欢科幻和悬疑",
        "喜欢,科幻",
        0.7,
        Presentation::Subjective,
        0.6,
    );

    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[], &[subjective], None).await;

    assert_eq!(report.implied_candidates, 1, "主观事件应产出策略①候选");
    assert_eq!(
        report.candidates_saved, 1,
        "无互证的低置信候选应落 candidate"
    );

    let facts = all_facts(&storage, "char-0001", ProfileField::Interests).await;
    assert_eq!(facts.len(), 1, "应保存 1 条 candidate");
    let f = &facts[0];
    assert_eq!(
        f.status,
        FactStatus::Candidate,
        "隐含候选应保持 candidate 轨道"
    );
    assert_eq!(
        f.field,
        ProfileField::Interests,
        "策略①应字段感知（Interests）"
    );
    assert!(
        f.content.starts_with("偏好："),
        "策略①应带字段语义前缀，content={}",
        f.content
    );
    assert!(
        (f.confidence - 0.5).abs() < f64::EPSILON,
        "隐含候选置信度应为 0.5"
    );
    assert!(f.ref_event_id.is_some(), "隐含候选应溯源事件");
}

// =========================================================
// (b) 策略② L1 保真线索 → 断言候选（source=L1 / ref_l1_id）
// =========================================================

#[tokio::test]
async fn l1_evidence_candidate_source_l1() {
    let storage = MockStorage::new();
    let cfg = enabled_knowledge();

    let l1 = l1_with_evidence("char-0001", "用户每周都要加班到很晚");
    let report = run_fact_extraction(
        &storage,
        &cfg,
        "char-0001",
        std::slice::from_ref(&l1),
        &[],
        None,
    )
    .await;

    assert_eq!(report.l1_candidates, 1, "L1 evidence_notes 应产出策略②候选");
    assert_eq!(report.candidates_saved, 1);

    let facts = all_facts(&storage, "char-0001", ProfileField::PersonalStatus).await;
    assert_eq!(facts.len(), 1, "加班线索应归类到 PersonalStatus");
    let f = &facts[0];
    assert_eq!(
        f.status,
        FactStatus::Candidate,
        "线索候选应保持 candidate 轨道"
    );
    assert_eq!(f.source.as_str(), "l1", "来源应为 L1");
    assert_eq!(f.ref_l1_id, Some(l1.id), "应溯源到来源 L1 id");
    assert_eq!(f.ref_event_id, None, "线索候选无事件溯源");
}

/// persona 归属不一致的 L1 线索应被拒绝（隔离红线）。
#[tokio::test]
async fn l1_evidence_persona_isolation() {
    let storage = MockStorage::new();
    let cfg = enabled_knowledge();

    let l1 = l1_with_evidence("char-other", "用户每周都要加班到很晚");
    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[l1], &[], None).await;

    assert_eq!(report.l1_candidates, 0, "跨 persona 的 L1 线索不应产出候选");
    let active = active_facts(&storage, "char-0001").await;
    assert!(active.is_empty());
}

// =========================================================
// (b) 策略③ 跨独立事件互证候选 → Promote 并落 active
// =========================================================

#[tokio::test]
async fn corroborated_implied_candidate_promoted_active() {
    let storage = MockStorage::new();
    let cfg = enabled_knowledge();

    // 预置两条"既有独立事件"佐证（不同来源 L1，库内最近事件通道，same_batch=false）
    let l1_a = Uuid::new_v4();
    let l1_b = Uuid::new_v4();
    let ev_a = event(
        "char-0001",
        1,
        "下午坚持跑步很开心",
        "下午坚持跑步后心情很好",
        "跑步",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let ev_b = event(
        "char-0001",
        2,
        "第二天继续跑步心情不错",
        "第二天继续跑步心情很好",
        "跑步",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let id_a = storage.save_event(&ev_a).await.unwrap();
    let id_b = storage.save_event(&ev_b).await.unwrap();
    storage.save_event_source(id_a, l1_a, 1.0).await.unwrap();
    storage.save_event_source(id_b, l1_b, 1.0).await.unwrap();

    // 本批主观源事件：策略①候选「偏好：好喜欢坚持跑步每次都很开心」
    let source = event(
        "char-0001",
        9001,
        "好喜欢坚持跑步每次都很开心",
        "提到坚持跑步后心情很开心",
        "跑步,开心",
        0.7,
        Presentation::Subjective,
        0.6,
    );

    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[], &[source], None).await;

    assert_eq!(report.implied_candidates, 1, "主观事件产出策略①候选");
    assert_eq!(
        report.promoted_active, 1,
        "≥2 独立事件佐证的候选应提升为 active"
    );
    assert_eq!(report.candidates_saved, 0, "提升路径不应额外落 candidate");

    let active = active_facts(&storage, "char-0001").await;
    assert_eq!(active.len(), 1, "应恰好 1 条 active");
    let f = &active[0];
    assert_eq!(f.status, FactStatus::Active);
    assert_eq!(
        f.field,
        ProfileField::Interests,
        "隐含候选字段感知为 Interests"
    );
    assert!(f.content.starts_with("偏好："), "content={}", f.content);
}

/// Promote 落 active 为"新增"，不覆盖同 field 既有 active（版本链红线，覆盖仅常规仲裁授权）。
#[tokio::test]
async fn promote_does_not_overwrite_existing_active_same_field() {
    let storage = MockStorage::new();
    // 既有同 field stable active（不同事实，例如旧兴趣音乐）
    let mut old = PersonaFact::new(
        "char-0001".into(),
        ProfileField::Interests,
        "旧兴趣：音乐".into(),
        ramaria_core::types::FactSource::Manual,
    );
    old.tier = FactTier::Stable;
    old.confidence = 0.9;
    storage.add_fact(old);

    let cfg = enabled_knowledge();
    // 佐证事件（独立、正向，支持"偏好跑步"候选 Promote）
    let l1_a = Uuid::new_v4();
    let l1_b = Uuid::new_v4();
    let ev_a = event(
        "char-0001",
        1,
        "下午坚持跑步很开心",
        "下午坚持跑步后心情很好",
        "跑步",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let ev_b = event(
        "char-0001",
        2,
        "第二天继续跑步心情不错",
        "第二天继续跑步心情很好",
        "跑步",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let id_a = storage.save_event(&ev_a).await.unwrap();
    let id_b = storage.save_event(&ev_b).await.unwrap();
    storage.save_event_source(id_a, l1_a, 1.0).await.unwrap();
    storage.save_event_source(id_b, l1_b, 1.0).await.unwrap();

    let source = event(
        "char-0001",
        9001,
        "好喜欢坚持跑步每次都很开心",
        "提到坚持跑步后心情很开心",
        "跑步,开心",
        0.7,
        Presentation::Subjective,
        0.6,
    );
    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[], &[source], None).await;

    assert_eq!(report.promoted_active, 1, "互证候选应新增为 active");
    assert_eq!(
        report.overwritten, 0,
        "Promote 不应覆盖同 field 既有 active"
    );

    let active = active_facts(&storage, "char-0001").await;
    assert_eq!(active.len(), 2, "旧兴趣与新偏好应并存（各自独立 active）");
    assert!(active.iter().any(|f| f.content.contains("旧兴趣")));
    assert!(active.iter().any(|f| f.content.contains("偏好")));
}

// =========================================================
// (c) 幂等：重复执行同批输入 → 不重复写/不重复提升
// =========================================================

#[tokio::test]
async fn rerun_same_batch_is_idempotent() {
    let storage = MockStorage::new();
    let cfg = enabled_knowledge();

    let l1_a = Uuid::new_v4();
    let l1_b = Uuid::new_v4();
    let ev_a = event(
        "char-0001",
        1,
        "下午坚持跑步很开心",
        "下午坚持跑步后心情很好",
        "跑步",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let ev_b = event(
        "char-0001",
        2,
        "第二天继续跑步心情不错",
        "第二天继续跑步心情很好",
        "跑步",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let id_a = storage.save_event(&ev_a).await.unwrap();
    let id_b = storage.save_event(&ev_b).await.unwrap();
    storage.save_event_source(id_a, l1_a, 1.0).await.unwrap();
    storage.save_event_source(id_b, l1_b, 1.0).await.unwrap();

    let source = event(
        "char-0001",
        9001,
        "好喜欢坚持跑步每次都很开心",
        "提到坚持跑步后心情很开心",
        "跑步,开心",
        0.7,
        Presentation::Subjective,
        0.6,
    );
    let l1 = l1_with_evidence("char-0001", "用户每周都要加班到很晚");

    let first = run_fact_extraction(
        &storage,
        &cfg,
        "char-0001",
        std::slice::from_ref(&l1),
        std::slice::from_ref(&source),
        None,
    )
    .await;
    assert_eq!(first.promoted_active, 1, "首次应产生 1 条提升 active");
    assert_eq!(first.l1_candidates, 1, "L1 线索候选应落 candidate");

    let active_after_first = active_facts(&storage, "char-0001").await;
    assert_eq!(active_after_first.len(), 1);

    let second = run_fact_extraction(&storage, &cfg, "char-0001", &[l1], &[source], None).await;
    assert_eq!(
        second.deduped, 2,
        "第二次同批输入应被内容级判重拦截（active + candidate）"
    );
    assert_eq!(second.promoted_active, 0, "重跑不应再次提升");
    assert_eq!(second.candidates_saved, 0, "重跑不应重复写 candidate");

    let active_after_second = active_facts(&storage, "char-0001").await;
    assert_eq!(active_after_second.len(), 1, "重跑不应新增 active（幂等）");
    let all_personal = all_facts(&storage, "char-0001", ProfileField::PersonalStatus).await;
    assert_eq!(all_personal.len(), 1, "重跑不应新增 candidate（幂等）");
}

// =========================================================
// (d) 仲裁红线：单事件 stable 候选不直接提升为 active
// =========================================================

#[tokio::test]
async fn single_stable_event_does_not_overwrite_existing_active() {
    let storage = MockStorage::new();
    // 预置同 field stable active（模拟既有稳定事实）
    let mut old = PersonaFact::new(
        "char-0001".into(),
        ProfileField::Interests,
        "旧兴趣：音乐".into(),
        ramaria_core::types::FactSource::Manual,
    );
    old.tier = FactTier::Stable;
    old.confidence = 0.9;
    storage.add_fact(old);

    let cfg = enabled_knowledge();
    // 新单事件（客观高置信，同 field）——应被 arbitrate 拦为 candidate，不覆盖 stable active
    let ev = event(
        "char-0001",
        10,
        "喜欢阅读科幻小说",
        "用户喜欢阅读科幻小说",
        "阅读,科幻",
        0.9,
        Presentation::Mixed,
        0.4,
    );

    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[], &[ev], None).await;

    assert_eq!(report.regular_candidates, 1, "客观达标事件应产出常规候选");
    assert_eq!(
        report.candidates_saved, 1,
        "单事件 stable 不覆盖 → 落 candidate"
    );
    assert_eq!(report.promoted_active, 0);
    assert_eq!(report.overwritten, 0);

    let active = active_facts(&storage, "char-0001").await;
    assert_eq!(active.len(), 1, "旧 stable active 应保持不变");
    assert!(active[0].content.contains("旧兴趣"), "不应被单事件覆盖");
}

// =========================================================
// (d) 仲裁红线：valence 方向冲突不提升（极性不一致不互证）
// =========================================================

#[tokio::test]
async fn valence_conflict_candidate_not_promoted() {
    let storage = MockStorage::new();
    let cfg = enabled_knowledge();

    // 佐证事件为正向（"压力缓解/放松"），候选为负向（"压力大难过"）→ 极性冲突不互证
    let l1_a = Uuid::new_v4();
    let l1_b = Uuid::new_v4();
    let ev_a = event(
        "char-0001",
        1,
        "压力缓解后很轻松",
        "压力终于缓解心情很轻松",
        "压力",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let ev_b = event(
        "char-0001",
        2,
        "放松下来很开心",
        "彻底放松后很开心",
        "压力",
        0.9,
        Presentation::Mixed,
        0.7,
    );
    let id_a = storage.save_event(&ev_a).await.unwrap();
    let id_b = storage.save_event(&ev_b).await.unwrap();
    storage.save_event_source(id_a, l1_a, 1.0).await.unwrap();
    storage.save_event_source(id_b, l1_b, 1.0).await.unwrap();

    let negative = event(
        "char-0001",
        9001,
        "最近压力很大很难过",
        "最近压力很大觉得很难过",
        "压力",
        0.7,
        Presentation::Subjective,
        -0.6,
    );

    let report = run_fact_extraction(&storage, &cfg, "char-0001", &[], &[negative], None).await;

    assert_eq!(report.implied_candidates, 1);
    assert_eq!(report.candidates_saved, 1, "极性冲突候选应保持 candidate");
    assert_eq!(report.promoted_active, 0, "极性冲突不提升");

    let active = active_facts(&storage, "char-0001").await;
    assert!(active.is_empty(), "不应产生任何 active");
}
