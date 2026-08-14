//! crates/ramaria-app/tests/behavior_integration.rs - v1.5 M5 行为层集成测试（D7+H1）
//!
//! 覆盖:
//! - 学习管线：事件 → 聚类 → 规则生成 → 替换旧 Auto 规则落库
//! - 情境路由：命中 / 全低于阈值静默降级 / 行为关闭不路由
//! - 规则管理：list / import（非法 JSON 拒绝、合法成功）/ edit（S1 反馈 + Manual 化）/
//!   disable（S1 反馈）/ evidence 溯源链
//! - 增量更新：归簇 / 证据衰减失效 / 行为关闭回退 v1.4（不触发）
//!
//! 安全约束:
//! - 全部使用 mock（MockStorage + MockLlm），不触碰真实数据库/LLM。
//! - 断言不含完整对话原文。

#![allow(unused_must_use)]

mod mock_backend;

use ramaria_app::App;
use ramaria_app::commands::behavior::{
    behavior_delete_rule, behavior_edit_rule, behavior_get_rule, behavior_import_rule,
    behavior_incremental_update, behavior_learn, behavior_list_rules, behavior_route,
    behavior_rule_evidence, behavior_set_rule_enabled,
};
use ramaria_core::behavior::{
    BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource, SignalType, TargetType,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    AppState, MemoryEvent, Message, MessageRole, MessageSource, Persona, PersonaKind, Presentation,
};
use ramaria_llm::keychain::Keychain;
use std::sync::Arc;
use uuid::Uuid;

use mock_backend::{MockLlm, MockStorage};

// =========================================================
// 辅助函数
// =========================================================

/// 构造 Ready 状态 App（MockStorage + MockLlm，行为层开启）。
fn make_app() -> (Arc<MockStorage>, App) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(
        r#"{"reaction": "当聊到加班时，倾向安静陪伴。", "avoid": ["深夜"]}"#,
    ));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        None,
        config,
        keychain,
    );
    (storage, app)
}

/// 构造行为层关闭的 App（回退 v1.4 断言用）。
fn make_app_behavior_disabled() -> (Arc<MockStorage>, App) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("{}"));
    let mut config = RamariaConfig::default();
    config.behavior.enabled = false;
    let app = App::new(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        None,
        config,
        Arc::new(Keychain::new()),
    );
    (storage, app)
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
    }
}

// =========================================================
// 学习管线
// =========================================================

#[tokio::test]
async fn learn_generates_rules_from_events() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    // 8 条同质事件（关键词"加班,累"）→ 聚成 1 簇 → 1 条规则
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }

    let outcome = behavior_learn(&app, "char-0001").await.expect("学习成功");
    assert_eq!(outcome.event_count, 8);
    assert_eq!(outcome.cluster_count, 1);
    assert_eq!(outcome.full_rule_count, 1, "质控通过生成完整规则");
    assert_eq!(outcome.replaced_rule_count, 0, "无旧 Auto 规则");

    let rules = behavior_list_rules(&app, "char-0001")
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
    let (storage, app) = make_app();
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

    let outcome = behavior_learn(&app, "char-0001").await.expect("学习成功");
    assert_eq!(outcome.replaced_rule_count, 1, "旧 Auto 规则被替换");

    let rules = behavior_list_rules(&app, "char-0001")
        .await
        .expect("列表成功");
    assert_eq!(rules.len(), 1, "旧规则被替换，只剩新规则");
    assert_ne!(rules[0].reaction.as_deref(), Some("旧规则"));
}

#[tokio::test]
async fn learn_with_behavior_disabled_returns_empty() {
    let (storage, app) = make_app_behavior_disabled();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    // 行为关闭 → 学习为空（等同 v1.4 行为断言）
    let outcome = behavior_learn(&app, "char-0001").await.expect("学习不报错");
    assert_eq!(outcome.event_count, 0);
    assert_eq!(outcome.cluster_count, 0);
    assert!(
        behavior_list_rules(&app, "char-0001")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn learn_no_events_no_rules() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    let outcome = behavior_learn(&app, "char-0001").await.expect("学习成功");
    assert_eq!(outcome.event_count, 0);
    assert_eq!(outcome.cluster_count, 0);
}

// =========================================================
// 情境路由
// =========================================================

#[tokio::test]
async fn route_hits_rule_with_matching_topic() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    behavior_learn(&app, "char-0001").await.expect("学习成功");

    let result = behavior_route(&app, "char-0001", &[make_msg("加班")])
        .await
        .expect("路由成功");
    assert!(result.matched, "话题词命中规则");
    let primary = result.primary.expect("主规则");
    assert!(primary.rule.has_reaction());
}

#[tokio::test]
async fn route_silent_degrade_on_unrelated_topic() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    behavior_learn(&app, "char-0001").await.expect("学习成功");

    let result = behavior_route(&app, "char-0001", &[make_msg("今天聊点完全无关的话题")])
        .await
        .expect("路由成功");
    assert!(!result.matched, "全部低于阈值 → 静默降级（等同 v1.4）");
    assert!(result.primary.is_none());
}

#[tokio::test]
async fn route_disabled_behavior_returns_unmatched() {
    let (storage, app) = make_app_behavior_disabled();
    setup_persona(&storage, "char-0001").await;
    let result = behavior_route(&app, "char-0001", &[make_msg("加班")])
        .await
        .expect("路由成功");
    assert!(!result.matched, "行为关闭 → 不路由（回退 v1.4）");
}

// =========================================================
// 规则管理（D7）
// =========================================================

#[tokio::test]
async fn import_rule_invalid_json_rejected() {
    let (_storage, app) = make_app();
    let err = behavior_import_rule(&app, "char-0001", "这不是 JSON")
        .await
        .expect_err("非法 JSON 应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn import_rule_missing_situation_rejected() {
    let (_storage, app) = make_app();
    let err = behavior_import_rule(&app, "char-0001", r#"{"reaction": "测试"}"#)
        .await
        .expect_err("缺 situation 应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn import_rule_empty_situation_rejected() {
    let (_storage, app) = make_app();
    let json = r#"{"situation": {"keywords": [], "valence_mean": 0.0}, "reaction": "测试"}"#;
    let err = behavior_import_rule(&app, "char-0001", json)
        .await
        .expect_err("空情境应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn import_rule_valid_json_creates_manual_rule() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    let json = r#"{
        "situation": {"keywords": ["失眠"], "valence_mean": -0.6, "valence_std": 0.1, "sample_count": 1},
        "reaction": "当聊到失眠时，倾向轻声安慰。",
        "params": {"emotional_intensity": -0.6, "proactiveness": 0.8, "detail_level": 0.4, "formality": 0.2},
        "avoid": ["睡前聊工作"]
    }"#;
    let id = behavior_import_rule(&app, "char-0001", json)
        .await
        .expect("合法导入成功");
    let rule = behavior_get_rule(&app, id)
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
    let (storage, app) = make_app();
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

    behavior_edit_rule(&app, &mut rule, Some("sess-1"))
        .await
        .expect("编辑成功");

    let updated = behavior_get_rule(&app, id)
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
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        Some("测试规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");

    behavior_set_rule_enabled(&app, id, false, None)
        .await
        .expect("禁用成功");
    assert!(!behavior_get_rule(&app, id).await.unwrap().unwrap().enabled);

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
    let (storage, app) = make_app();
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

    behavior_set_rule_enabled(&app, id, true, None)
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
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        Some("待删规则".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");
    behavior_delete_rule(&app, id).await.expect("删除成功");
    assert!(behavior_get_rule(&app, id).await.unwrap().is_none());
}

#[tokio::test]
async fn rule_evidence_traces_to_events() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    // 生成规则（含证据链）
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    behavior_learn(&app, "char-0001").await.expect("学习成功");
    let rules = behavior_list_rules(&app, "char-0001")
        .await
        .expect("列表成功");
    assert!(!rules[0].evidence.is_empty(), "规则含证据链");

    let items = behavior_rule_evidence(&app, rules[0].id)
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
    let (_storage, app) = make_app();
    let err = behavior_rule_evidence(&app, 999)
        .await
        .expect_err("规则不存在应报错");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

#[tokio::test]
async fn rules_isolated_by_persona() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    setup_persona(&storage, "char-0002").await;
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    behavior_learn(&app, "char-0001").await.expect("学习成功");

    assert_eq!(
        behavior_list_rules(&app, "char-0001").await.unwrap().len(),
        1
    );
    assert!(
        behavior_list_rules(&app, "char-0002")
            .await
            .unwrap()
            .is_empty(),
        "跨 persona 隔离"
    );
}

// =========================================================
// 增量更新（D6，封存钩子核心）
// =========================================================

#[tokio::test]
async fn incremental_assigns_new_event_to_existing_rule() {
    let (storage, app) = make_app();
    setup_persona(&storage, "char-0001").await;
    // 先学习出规则（事件已吸收，mock 语义：learn 不改变 absorbed）
    for _ in 0..8 {
        make_event(&storage, "char-0001", "加班,累", -0.5).await;
    }
    behavior_learn(&app, "char-0001").await.expect("学习成功");
    let before = behavior_list_rules(&app, "char-0001").await.unwrap();
    let evidence_before = before[0].evidence.len();

    // 新事件（关键词与规则重合）→ 归入规则 → 证据追加
    make_event(&storage, "char-0001", "加班,累", -0.5).await;
    behavior_incremental_update(&app, "char-0001")
        .await
        .expect("增量更新成功");

    let after = behavior_list_rules(&app, "char-0001").await.unwrap();
    assert!(
        after[0].evidence.len() > evidence_before,
        "新事件证据已追加（{} → {}）",
        evidence_before,
        after[0].evidence.len()
    );
}

#[tokio::test]
async fn incremental_disabled_behavior_is_noop() {
    let (storage, app) = make_app_behavior_disabled();
    setup_persona(&storage, "char-0001").await;
    make_event(&storage, "char-0001", "加班,累", -0.5).await;
    // 行为关闭 → 增量更新直接返回（等同 v1.4，不产生规则）
    behavior_incremental_update(&app, "char-0001")
        .await
        .expect("关闭时不报错");
    assert!(
        behavior_list_rules(&app, "char-0001")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn incremental_decays_old_rule_to_disabled() {
    let (storage, app) = make_app();
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
    behavior_incremental_update(&app, "char-0001")
        .await
        .expect("增量更新成功");

    let rule = behavior_list_rules(&app, "char-0001").await.unwrap();
    assert_eq!(rule.len(), 1);
    assert!(!rule[0].enabled, "证据衰减失效 → 降级禁用（保留审计）");
    assert!(
        rule[0].evidence.iter().all(|e| e.weight < 0.5),
        "证据已衰减"
    );
}

#[tokio::test]
async fn app_state_ready_for_behavior() {
    // App 构造后状态正常（行为钩子注册不破坏初始化）
    let (_storage, app) = make_app();
    assert_eq!(app.current_state(), AppState::NeedsSetup);
}
