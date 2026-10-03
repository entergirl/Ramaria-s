//! crates/ramaria-service/tests/suites/behavior.rs - 行为规则管理、学习与证据链用例
//!
//! 设计特点:
//! - 覆盖全量学习（事件聚类 → 规则生成 → 替换旧自动规则）与关闭门禁
//! - 覆盖情境路由命中 / 静默降级 / 禁用与空规则集
//! - 覆盖导入校验（非法 JSON / 缺字段 / 空情境拒绝）与合法导入落库
//! - 覆盖编辑、启停与删除的反馈日志语义（禁用写 S1 反馈、启用不写）
//! - 覆盖证据链、人格隔离与增量更新（归簇 / 衰减）

use std::sync::Arc;

use ramaria_core::behavior::{
    BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource, SignalType, TargetType,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{
    AppState, MemoryEvent, Message, MessageRole, MessageSource, Persona, PersonaKind, Presentation,
};
use ramaria_memory::behavior::RoutingResult;
use ramaria_service::Engine;
use uuid::Uuid;

use crate::support::engine_env::build_engine;
use crate::support::mock_backend::{MockLlm, MockStorage};

// =========================================================
// 辅助函数
// =========================================================

/// 构造引擎（MockStorage + MockLlm，行为层开启）。
fn make_engine() -> (Arc<MockStorage>, Arc<Engine>) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(
        r#"{"reaction": "当聊到加班时，倾向安静陪伴。", "avoid": ["深夜"]}"#,
    ));
    let config = RamariaConfig::default();
    let engine = build_engine(Arc::clone(&storage), llm, config);
    (storage, engine)
}

/// 构造行为层关闭的引擎（关闭门禁与空态用例）。
fn make_engine_behavior_disabled() -> (Arc<MockStorage>, Arc<Engine>) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("{}"));
    let mut config = RamariaConfig::default();
    config.behavior.enabled = false;
    let engine = build_engine(Arc::clone(&storage), llm, config);
    (storage, engine)
}

/// 注册 persona。
async fn setup_persona(storage: &MockStorage, uid: &str) {
    storage
        .create_persona(&Persona::new(
            uid.to_string(),
            format!("测试 {uid}"),
            PersonaKind::Char,
            0,
            "local".into(),
        ))
        .await
        .expect("persona 创建成功");
}

/// 构造一条事件（关键词/valence/paraphrase 固定）。
async fn make_event(storage: &MockStorage, persona: &str, keywords: &str, valence: f64) -> i64 {
    let mut ev = MemoryEvent::new(
        persona.to_string(),
        "加班事件".into(),
        "连续加班一周，身心俱疲".into(),
        1,
        2,
    );
    ev.keywords = Some(keywords.to_string());
    ev.valence = valence;
    ev.presentation = Presentation::Subjective;
    ev.salience = 0.8;
    ev.paraphrase = Some("对加班感到疲惫".into());
    ev.attitude = Some("加班好累".into());
    storage.save_event(&ev).await.expect("事件保存成功")
}

/// 构造一条用户消息（独立会话，不落库）。
fn make_msg(content: &str) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        role: MessageRole::User,
        content: content.to_string(),
        source: MessageSource::Local,
        created_at: 0,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
    }
}

/// 情境路由：读规则 + 查询构造 → 路由决策。
async fn behavior_route(
    engine: &Engine,
    persona_uid: &str,
    messages: &[Message],
) -> RamariaResult<RoutingResult> {
    let config = engine.config();
    let embedding = engine.embedding();
    ramaria_memory::behavior::orchestrate::route(
        engine.storage().as_ref(),
        &config.behavior,
        embedding.as_deref(),
        persona_uid,
        messages,
    )
    .await
}

// =========================================================
// 学习管线
// =========================================================

#[tokio::test]
async fn learn_generates_rules_from_events() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    // 8 条同质事件（关键词"加班,累"）→ 聚成 1 簇 → 1 条规则
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }

    let outcome = engine.behavior_learn("char-0001").await.expect("学习成功");
    assert_eq!(outcome.event_count, 8);
    assert_eq!(outcome.cluster_count, 1);
    assert_eq!(outcome.full_rule_count, 1, "质控通过生成完整规则");
    assert_eq!(outcome.replaced_rule_count, 0, "无旧 Auto 规则");

    let rules = engine
        .behavior_list_rules("char-0001")
        .await
        .expect("列表成功");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].source, RuleSource::Auto, "Auto 规则自动生效");
    assert!(rules[0].enabled);
    assert!(rules[0].has_reaction());
    assert!(rules[0].situation.keywords.contains(&"加班".to_string()));
}

#[tokio::test]
async fn learn_replaces_old_auto_rules() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    // 预置一条旧 Auto 规则（模拟上次学习产物）
    let old = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        Some("旧规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    storage
        .save_behavior_rule(&old)
        .await
        .expect("旧规则保存成功");

    let outcome = engine.behavior_learn("char-0001").await.expect("学习成功");
    assert_eq!(outcome.replaced_rule_count, 1, "旧 Auto 规则被替换");

    let rules = engine
        .behavior_list_rules("char-0001")
        .await
        .expect("列表成功");
    assert_eq!(rules.len(), 1, "旧规则被替换，只剩新规则");
    assert_ne!(rules[0].reaction.as_deref(), Some("旧规则"));
}

#[tokio::test]
async fn learn_with_behavior_disabled_returns_empty() {
    let (storage, engine) = make_engine_behavior_disabled();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    // 行为关闭 → 学习为空
    let outcome = engine
        .behavior_learn("char-0001")
        .await
        .expect("学习不报错");
    assert_eq!(outcome.event_count, 0);
    assert_eq!(outcome.cluster_count, 0);
    assert!(
        engine
            .behavior_list_rules("char-0001")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn learn_no_events_no_rules() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    let outcome = engine.behavior_learn("char-0001").await.expect("学习成功");
    assert_eq!(outcome.event_count, 0);
    assert_eq!(outcome.cluster_count, 0);
}

// =========================================================
// 情境路由
// =========================================================

#[tokio::test]
async fn route_hits_rule_with_matching_topic() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    engine.behavior_learn("char-0001").await.expect("学习成功");

    let result = behavior_route(&engine, "char-0001", &[make_msg("加班")])
        .await
        .expect("路由成功");
    assert!(result.matched, "话题词命中规则");
    let primary = result.primary.expect("主规则");
    assert!(primary.rule.has_reaction());
}

#[tokio::test]
async fn route_silent_degrade_on_unrelated_topic() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    engine.behavior_learn("char-0001").await.expect("学习成功");

    let result = behavior_route(&engine, "char-0001", &[make_msg("今天聊点完全无关的话题")])
        .await
        .expect("路由成功");
    assert!(!result.matched, "全部低于阈值 → 静默降级（等同 v1.4）");
    assert!(result.primary.is_none());
}

#[tokio::test]
async fn route_disabled_behavior_returns_unmatched() {
    let (storage, engine) = make_engine_behavior_disabled();
    setup_persona(&storage, "char-0001").await;
    let result = behavior_route(&engine, "char-0001", &[make_msg("加班")])
        .await
        .expect("路由成功");
    assert!(!result.matched, "行为关闭 → 不路由（回退 v1.4）");
}

/// 空规则库 + 行为层开启 → 路由不 panic、返回 unmatched（调用方据此不注入行为块）。
#[tokio::test]
async fn route_empty_rule_set_returns_unmatched() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    // 未 learn / 未 import：该 persona 规则库为空
    let result = behavior_route(&engine, "char-0001", &[make_msg("加班")])
        .await
        .expect("路由成功");
    assert!(!result.matched, "空规则库不应命中");
    assert!(result.primary.is_none());
    assert!(result.secondary.is_empty());
}

// =========================================================
// 规则管理
// =========================================================

#[tokio::test]
async fn import_rule_invalid_json_rejected() {
    let (_storage, engine) = make_engine();
    let err = engine
        .behavior_import_rule("char-0001", "这不是 JSON")
        .await
        .expect_err("非法 JSON 应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn import_rule_missing_situation_rejected() {
    let (_storage, engine) = make_engine();
    let err = engine
        .behavior_import_rule("char-0001", r#"{"reaction": "测试"}"#)
        .await
        .expect_err("缺 situation 应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn import_rule_empty_situation_rejected() {
    let (_storage, engine) = make_engine();
    let json = r#"{"situation": {"keywords": [], "valence_mean": 0.0}, "reaction": "测试"}"#;
    let err = engine
        .behavior_import_rule("char-0001", json)
        .await
        .expect_err("空情境应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn import_rule_valid_json_creates_manual_rule() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    let json = r#"{
        "situation": {"keywords": ["失眠"], "valence_mean": -0.6, "valence_std": 0.1, "sample_count": 1},
        "reaction": "当聊到失眠时，倾向轻声安慰。",
        "params": {"emotional_intensity": -0.6, "proactiveness": 0.8, "detail_level": 0.4, "formality": 0.2},
        "avoid": ["睡前聊工作"]
    }"#;
    let id = engine
        .behavior_import_rule("char-0001", json)
        .await
        .expect("合法导入成功");
    let rule = engine
        .behavior_get_rule(id)
        .await
        .expect("查询成功")
        .expect("应命中");
    assert_eq!(rule.source, RuleSource::Manual, "导入规则为 Manual");
    assert!(rule.enabled);
    assert_eq!(rule.avoid, vec!["睡前聊工作"]);
    assert_eq!(rule.situation.keywords, vec!["失眠"]);
}

#[tokio::test]
async fn edit_rule_writes_s1_feedback_and_manualizes() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    let mut rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: vec!["加班".into()],
            centroid: None,
            response_centroid: None,
            valence_mean: -0.4,
            valence_std: 0.1,
            sample_count: 6,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 10.0,
            trait_refs: Vec::new(),
        },
        Some("原规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");
    rule.id = id;
    rule.reaction = Some("编辑后的规则".into());

    engine
        .behavior_edit_rule(&mut rule, Some("sess-1"))
        .await
        .expect("编辑成功");

    let updated = engine
        .behavior_get_rule(id)
        .await
        .expect("查询成功")
        .unwrap();
    assert_eq!(updated.reaction.as_deref(), Some("编辑后的规则"));
    assert_eq!(
        updated.source,
        RuleSource::Manual,
        "编辑后转为 Manual（强锚点）"
    );

    // S1 反馈日志断言
    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("查询成功");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].signal_type, SignalType::Edit);
    assert_eq!(logs[0].weight, 1.0, "S1 强信号 weight=1.0");
    assert_eq!(logs[0].target_type, TargetType::BehaviorRule);
    assert_eq!(logs[0].target_id, id.to_string());
    assert_eq!(logs[0].session_id.as_deref(), Some("sess-1"));
    let detail = logs[0].detail.as_deref().unwrap();
    assert!(
        detail.contains("原规则") && detail.contains("编辑后的规则"),
        "detail 含编辑前后快照"
    );
}

#[tokio::test]
async fn disable_rule_writes_s1_feedback() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        Some("测试规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");

    engine
        .behavior_set_rule_enabled(id, false, None)
        .await
        .expect("禁用成功");
    assert!(!engine.behavior_get_rule(id).await.unwrap().unwrap().enabled);

    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("查询成功");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].signal_type, SignalType::Disable);
    assert_eq!(logs[0].weight, 1.0);
}

#[tokio::test]
async fn enable_rule_no_feedback() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    let mut rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        Some("测试规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    rule.enabled = false;
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");

    engine
        .behavior_set_rule_enabled(id, true, None)
        .await
        .expect("启用成功");
    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("查询成功");
    assert!(logs.is_empty(), "启用非用户干预，不写 S1");
}

#[tokio::test]
async fn delete_rule_removes() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        Some("待删规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");
    engine.behavior_delete_rule(id).await.expect("删除成功");
    assert!(engine.behavior_get_rule(id).await.unwrap().is_none());
}

#[tokio::test]
async fn rule_evidence_traces_to_events() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    // 生成规则（含证据链）
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    engine.behavior_learn("char-0001").await.expect("学习成功");
    let rules = engine
        .behavior_list_rules("char-0001")
        .await
        .expect("列表成功");
    assert!(!rules[0].evidence.is_empty(), "规则含证据链");

    let items = engine
        .behavior_rule_evidence(rules[0].id)
        .await
        .expect("证据链查询成功");
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i.event_id > 0), "证据指向真实事件");
    assert!(items.iter().all(|i| i.title == "加班事件"));
    // 权重降序
    let weights: Vec<f64> = items.iter().map(|i| i.weight).collect();
    let mut sorted = weights.clone();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(weights, sorted, "按权重降序");
}

#[tokio::test]
async fn rule_evidence_missing_rule_errors() {
    let (_storage, engine) = make_engine();
    let err = engine
        .behavior_rule_evidence(999)
        .await
        .expect_err("规则不存在应报错");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn rules_isolated_by_persona() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    setup_persona(&storage, "char-0002").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    engine.behavior_learn("char-0001").await.expect("学习成功");

    assert_eq!(
        engine.behavior_list_rules("char-0001").await.unwrap().len(),
        1
    );
    assert!(
        engine
            .behavior_list_rules("char-0002")
            .await
            .unwrap()
            .is_empty(),
        "跨 persona 隔离"
    );
}

// =========================================================
// 增量更新（封存钩子核心）
// =========================================================

#[tokio::test]
async fn incremental_assigns_new_event_to_existing_rule() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    // 先学习出规则（事件已吸收，mock 语义：learn 不改变 absorbed）
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    engine.behavior_learn("char-0001").await.expect("学习成功");
    let before = engine.behavior_list_rules("char-0001").await.unwrap();
    let evidence_before = before[0].evidence.len();

    // 新事件（关键词与规则重合）→ 归入规则 → 证据追加
    make_event(&storage, "char-0001", "加班,累", -0.5).await;
    engine
        .behavior_incremental_update("char-0001")
        .await
        .expect("增量更新成功");

    let after = engine.behavior_list_rules("char-0001").await.unwrap();
    assert!(
        after[0].evidence.len() > evidence_before,
        "新事件证据已追加（{} → {}）",
        evidence_before,
        after[0].evidence.len()
    );
}

#[tokio::test]
async fn incremental_disabled_behavior_is_noop() {
    let (storage, engine) = make_engine_behavior_disabled();
    setup_persona(&storage, "char-0001").await;
    make_event(&storage, "char-0001", "加班,累", -0.5).await;
    // 行为关闭 → 增量更新直接返回（不产生规则）
    engine
        .behavior_incremental_update("char-0001")
        .await
        .expect("关闭时不报错");
    assert!(
        engine
            .behavior_list_rules("char-0001")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn incremental_decays_old_rule_to_disabled() {
    let (storage, engine) = make_engine();
    setup_persona(&storage, "char-0001").await;
    // 预置一条一年前的旧规则（证据会衰减失效）
    let mut old = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: vec!["旧话题".into()],
            centroid: None,
            response_centroid: None,
            valence_mean: -0.3,
            valence_std: 0.1,
            sample_count: 6,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 300.0,
            trait_refs: Vec::new(),
        },
        Some("旧规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    old.created_at = ramaria_core::types::now_ms() - 400 * 86_400_000;
    old.evidence = (1..=3)
        .map(|i| ramaria_core::behavior::BehaviorEvidence {
            event_id: i,
            weight: 0.5,
        })
        .collect();
    storage.save_behavior_rule(&old).await.expect("保存成功");

    // 新事件（关键词与旧规则不匹配 → 进待定池，不干扰衰减判定）
    make_event(&storage, "char-0001", "加班,累", -0.5).await;
    engine
        .behavior_incremental_update("char-0001")
        .await
        .expect("增量更新成功");

    let rule = engine.behavior_list_rules("char-0001").await.unwrap();
    assert_eq!(rule.len(), 1);
    assert!(!rule[0].enabled, "证据衰减失效 → 降级禁用（保留审计）");
    assert!(
        rule[0].evidence.iter().all(|e| e.weight < 0.5),
        "证据已衰减"
    );
}

#[tokio::test]
async fn app_state_ready_for_behavior() {
    // 引擎构造后状态正常（装配阶段不推进状态）
    let (_storage, engine) = make_engine();
    assert_eq!(engine.current_state(), AppState::NeedsSetup);
}
