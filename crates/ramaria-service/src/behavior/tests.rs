//! crates/ramaria-service/src/behavior/tests.rs - Ramaria 行为规则用例测试
//!
//! 设计特点:
//! - 由 behavior.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖规则管理 / 导入校验 /
//!   证据链 / 学习管线与增量更新四条路径
//! - 真实 SQLite（临时文件库 + 全量 migration）：断言入口返回口径与落库结果
//! - 学习管线用例以 mock LLM 与确定性配置驱动（关闭 / 无事件场景直接跳过重活）
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::*;
use crate::test_support::{MockLlm, engine_with_db, engine_with_llm_and_config, seed_persona};
use ramaria_core::behavior::{BehaviorEvidence, BehaviorParams};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::MemoryEvent;
use ramaria_storage::SqliteStorage;

/// 构造一条 Auto 规则（测试造数）。
fn auto_rule(persona: &str, reaction: &str) -> BehaviorRule {
    BehaviorRule::new(
        persona,
        BehaviorSituation::empty(),
        Some(reaction.to_string()),
        BehaviorParams::default(),
        RuleSource::Auto,
    )
}

/// 造一条事件（测试造数，载荷带脱敏字段供证据链断言）。
async fn seed_event(storage: &SqliteStorage, persona: &str, title: &str) -> i64 {
    let mut event = MemoryEvent::new(
        persona.to_string(),
        title.to_string(),
        format!("{title}的摘要说明"),
        1_000,
        2_000,
    );
    event.paraphrase = Some(format!("{title}的态度重述"));
    event.keywords = Some("测试,证据".to_string());
    storage.save_event(&event).await.expect("写入事件应成功")
}

/// 列表与启停：禁用写 S1 反馈日志（weight=1.0），启用不写（非干预）。
#[tokio::test]
async fn list_and_toggle_writes_feedback_on_disable() {
    let (engine, storage, dir) = engine_with_db("behavior-toggle").await;
    seed_persona(&storage, "char-0001").await;
    let rule_id = storage
        .save_behavior_rule(&auto_rule("char-0001", "先自嘲一句再聊具体事"))
        .await
        .expect("写入规则应成功");

    let rules = engine
        .behavior_list_rules("char-0001")
        .await
        .expect("规则列表应成功");
    assert_eq!(rules.len(), 1, "应列出刚写入的规则");
    assert_eq!(rules[0].id, rule_id);
    assert!(rules[0].enabled, "新规则默认启用");

    engine
        .behavior_set_rule_enabled(rule_id, false, None)
        .await
        .expect("禁用应成功");
    let disabled = storage
        .get_behavior_rule(rule_id)
        .await
        .expect("查询规则应成功")
        .expect("规则应存在");
    assert!(!disabled.enabled, "禁用后 enabled=false");

    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("查询反馈日志应成功");
    assert_eq!(logs.len(), 1, "禁用应写一条反馈日志");
    assert_eq!(logs[0].signal_type, SignalType::Disable);
    assert_eq!(logs[0].target_type, TargetType::BehaviorRule);
    assert_eq!(logs[0].target_id, rule_id.to_string());
    assert_eq!(logs[0].weight, 1.0, "S1 强信号 weight=1.0");

    engine
        .behavior_set_rule_enabled(rule_id, true, None)
        .await
        .expect("启用应成功");
    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("查询反馈日志应成功");
    assert_eq!(logs.len(), 1, "启用非干预信号，不写反馈日志");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 编辑：转 Manual 强锚点并写编辑前后快照反馈（detail 不含原文）。
#[tokio::test]
async fn edit_converts_to_manual_and_writes_snapshot() {
    let (engine, storage, dir) = engine_with_db("behavior-edit").await;
    seed_persona(&storage, "char-0001").await;
    let rule_id = storage
        .save_behavior_rule(&auto_rule("char-0001", "原规则文本"))
        .await
        .expect("写入规则应成功");

    let mut rule = engine
        .behavior_get_rule(rule_id)
        .await
        .expect("查询规则应成功")
        .expect("规则应存在");
    rule.reaction = Some("改为先倾听再回应".to_string());
    engine
        .behavior_edit_rule(&mut rule, Some("sess-1"))
        .await
        .expect("编辑应成功");

    let stored = storage
        .get_behavior_rule(rule_id)
        .await
        .expect("查询规则应成功")
        .expect("规则应存在");
    assert_eq!(stored.source, RuleSource::Manual, "编辑后应转为 Manual");
    assert_eq!(stored.reaction.as_deref(), Some("改为先倾听再回应"));

    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("查询反馈日志应成功");
    assert_eq!(logs.len(), 1, "编辑应写一条反馈日志");
    assert_eq!(logs[0].signal_type, SignalType::Edit);
    assert_eq!(logs[0].session_id.as_deref(), Some("sess-1"));
    let detail: serde_json::Value =
        serde_json::from_str(logs[0].detail.as_deref().expect("编辑反馈应含快照"))
            .expect("快照应为合法 JSON");
    assert_eq!(detail["before"]["reaction"], "原规则文本");
    assert_eq!(detail["after"]["reaction"], "改为先倾听再回应");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 删除：规则从库中消失（get 返回 None）。
#[tokio::test]
async fn delete_removes_rule() {
    let (engine, storage, dir) = engine_with_db("behavior-delete").await;
    seed_persona(&storage, "char-0001").await;
    let rule_id = storage
        .save_behavior_rule(&auto_rule("char-0001", "待删除规则"))
        .await
        .expect("写入规则应成功");

    engine
        .behavior_delete_rule(rule_id)
        .await
        .expect("删除应成功");
    assert!(
        engine
            .behavior_get_rule(rule_id)
            .await
            .expect("查询规则应成功")
            .is_none(),
        "删除后规则应不存在"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 导入校验：非法 JSON / 缺 situation / 空情境 / 空规则均拒绝，且不落库。
#[tokio::test]
async fn import_rejects_invalid_payloads() {
    let (engine, storage, dir) = engine_with_db("behavior-import-reject").await;
    seed_persona(&storage, "char-0001").await;

    let err = engine
        .behavior_import_rule("char-0001", "{ not json")
        .await
        .expect_err("非法 JSON 应拒绝");
    assert!(
        err.to_string().contains("规则 JSON 非法"),
        "错误应指明 JSON 非法: {err}"
    );

    let err = engine
        .behavior_import_rule("char-0001", r#"{"reaction": "没有情境"}"#)
        .await
        .expect_err("缺 situation 应拒绝");
    assert!(
        err.to_string().contains("缺少 situation"),
        "错误应指明缺 situation: {err}"
    );

    let err = engine
        .behavior_import_rule("char-0001", r#"{"situation": {}, "reaction": "回应"}"#)
        .await
        .expect_err("空情境应拒绝");
    assert!(
        err.to_string().contains("空情境拒绝导入"),
        "错误应指明空情境: {err}"
    );

    let err = engine
        .behavior_import_rule("char-0001", r#"{"situation": {"keywords": ["难过"]}}"#)
        .await
        .expect_err("空规则应拒绝");
    assert!(
        err.to_string().contains("至少一项"),
        "错误应指明空规则: {err}"
    );

    let rules = engine
        .behavior_list_rules("char-0001")
        .await
        .expect("规则列表应成功");
    assert!(rules.is_empty(), "拒绝导入不应落库");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 导入成功：宽松 JSON 落库为 Manual 规则（confidence / stability=1.0）。
#[tokio::test]
async fn import_success_persists_manual_rule() {
    let (engine, storage, dir) = engine_with_db("behavior-import-ok").await;
    seed_persona(&storage, "char-0001").await;

    let id = engine
        .behavior_import_rule(
            "char-0001",
            r#"{
                    "situation": {"keywords": ["难过", "低落"]},
                    "reaction": "用轻快的语气回应",
                    "avoid": ["说教"]
                }"#,
        )
        .await
        .expect("合法 JSON 应导入成功");

    let rule = engine
        .behavior_get_rule(id)
        .await
        .expect("查询规则应成功")
        .expect("导入的规则应存在");
    assert_eq!(rule.persona_uid, "char-0001");
    assert_eq!(rule.source, RuleSource::Manual);
    assert!(rule.enabled, "手工导入自动生效");
    assert_eq!(rule.reaction.as_deref(), Some("用轻快的语气回应"));
    assert_eq!(rule.avoid, vec!["说教"]);
    assert_eq!(rule.situation.keywords, vec!["难过", "低落"]);
    assert_eq!(rule.confidence, 1.0);
    assert_eq!(rule.stability, 1.0);
    // 宽松默认：缺失统计字段用默认值组装
    assert_eq!(rule.situation.valence_mean, 0.0);
    assert_eq!(rule.situation.situation_strength_mean, 3.0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 证据链：权重降序；脏引用（事件已不存在）跳过。
#[tokio::test]
async fn rule_evidence_orders_by_weight_desc_and_skips_dangling() {
    let (engine, storage, dir) = engine_with_db("behavior-evidence").await;
    seed_persona(&storage, "char-0001").await;

    let first = seed_event(&storage, "char-0001", "事件一").await;
    let second = seed_event(&storage, "char-0001", "事件二").await;

    let mut rule = auto_rule("char-0001", "携带证据的规则");
    rule.evidence = vec![
        BehaviorEvidence {
            event_id: first,
            weight: 0.3,
        },
        // 脏引用：事件不存在，应跳过
        BehaviorEvidence {
            event_id: 99_999,
            weight: 0.99,
        },
        BehaviorEvidence {
            event_id: second,
            weight: 0.9,
        },
    ];
    let rule_id = storage
        .save_behavior_rule(&rule)
        .await
        .expect("写入规则应成功");

    let items = engine
        .behavior_rule_evidence(rule_id)
        .await
        .expect("证据链应成功");
    assert_eq!(items.len(), 2, "脏引用应跳过");
    assert_eq!(items[0].weight, 0.9, "权重降序");
    assert_eq!(items[0].event_id, second);
    assert_eq!(items[0].title, "事件二");
    assert_eq!(
        items[0].paraphrase.as_deref(),
        Some("事件二的态度重述"),
        "证据项应携带脱敏态度字段"
    );
    assert_eq!(items[1].weight, 0.3);
    assert_eq!(items[1].event_id, first);
    assert_eq!(items[1].title, "事件一");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 学习：`[behavior].enabled=false` 时返回空统计（不读取事件、不替换规则）。
#[tokio::test]
async fn learn_with_behavior_disabled_returns_empty_outcome() {
    let mut config = RamariaConfig::default();
    config.behavior.enabled = false;
    let (engine, storage, dir) =
        engine_with_llm_and_config("behavior-learn-off", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;
    // 库内有事件与规则：关闭时学习应直接跳过（统计保持空）
    seed_event(&storage, "char-0001", "既有事件").await;
    storage
        .save_behavior_rule(&auto_rule("char-0001", "既有规则"))
        .await
        .expect("写入规则应成功");

    let outcome = engine
        .behavior_learn("char-0001")
        .await
        .expect("关闭时学习应成功返回");
    assert_eq!(outcome.event_count, 0, "关闭时不应读取事件");
    assert_eq!(outcome.cluster_count, 0);
    assert_eq!(outcome.full_rule_count, 0);
    assert_eq!(outcome.candidate_rule_count, 0);
    assert_eq!(outcome.replaced_rule_count, 0, "关闭时不应替换旧 Auto 规则");

    let rules = engine
        .behavior_list_rules("char-0001")
        .await
        .expect("规则列表应成功");
    assert_eq!(rules.len(), 1, "既有规则应保持原状");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 学习：无事件 → 返回空统计（不报错，手动补跑入口幂等）。
#[tokio::test]
async fn learn_without_events_returns_empty_outcome() {
    let (engine, storage, dir) = engine_with_db("behavior-learn-empty").await;
    seed_persona(&storage, "char-0001").await;

    let outcome = engine
        .behavior_learn("char-0001")
        .await
        .expect("无事件时学习应成功返回");
    assert_eq!(outcome.event_count, 0);
    assert_eq!(outcome.cluster_count, 0);
    assert_eq!(outcome.replaced_rule_count, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 增量更新门面：`[behavior].enabled=false` 时直接返回（不触达存储）。
#[tokio::test]
async fn incremental_update_disabled_returns_ok() {
    let mut config = RamariaConfig::default();
    config.behavior.enabled = false;
    let (engine, _storage, dir) =
        engine_with_llm_and_config("behavior-incr-off", MockLlm::local(), config).await;

    engine
        .behavior_incremental_update("char-0001")
        .await
        .expect("关闭时应静默返回");

    let _ = std::fs::remove_dir_all(&dir);
}
