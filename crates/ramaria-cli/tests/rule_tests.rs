//! tests/rule_tests.rs - 行为规则管理命令测试（v1.5 规则管理）
//!
//! 覆盖:
//! - list: 空数据（JSON 信封）
//! - import: 非法 JSON 拒绝 / 合法导入成功（Manual）
//! - show: 不存在报错
//! - edit: 修改 reaction/avoid → Manual 化 + S1 反馈
//! - enable / disable: 状态切换 + disable 写 S1 反馈
//! - delete: --yes 确认删除
//! - evidence: 溯源链展示
//!
//! 安全约束:
//! - 全部使用 MockStorage + MockLlm，不触碰真实数据库/LLM。
//! - clap 命令定义在 main.rs（进程级解析由项目负责人手动验收），
//!   本文件直接测 `commands::rule::run` 分发逻辑。

mod common;

use common::{MockStorage, build_test_app, make_test_event};
use ramaria_core::behavior::{
    BehaviorEvidence, BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource, SignalType,
};
use ramaria_core::traits::StoreInfrastructure;

use std::sync::Arc;

use ramaria_cli::commands::rule::{RuleCmd, run};

// =========================================================
// 辅助函数
// =========================================================

/// 预置一条 Auto 规则（含证据链）。
async fn seed_rule(storage: &Arc<MockStorage>, persona: &str) -> i64 {
    let mut rule = BehaviorRule::new(
        persona,
        BehaviorSituation {
            keywords: vec!["加班".into(), "累".into()],
            centroid: None,
            response_centroid: None,
            valence_mean: -0.4,
            valence_std: 0.2,
            sample_count: 6,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 10.0,
            trait_refs: Vec::new(),
        },
        Some("当聊到加班时，倾向表达疲惫并安慰对方。".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    rule.confidence = 0.8;
    rule.evidence = vec![
        BehaviorEvidence {
            event_id: 1,
            weight: 0.9,
        },
        BehaviorEvidence {
            event_id: 2,
            weight: 0.6,
        },
    ];
    storage
        .save_behavior_rule(&rule)
        .await
        .expect("规则预置成功")
}

// =========================================================
// list
// =========================================================

#[tokio::test]
async fn rule_list_empty_json_ok() {
    let (app, _storage) = build_test_app();
    let cmd = RuleCmd::List {
        persona: None,
        limit: Some(100),
        offset: 0,
    };
    // 空数据 → 成功输出空信封（agent 可区分「成功无数据」与异常）
    run(&app, cmd, true, false).await.expect("list 成功");
}

// =========================================================
// import
// =========================================================

#[tokio::test]
async fn rule_import_invalid_json_rejected() {
    let (app, _storage) = build_test_app();
    let err = ramaria_app::commands::behavior::behavior_import_rule(&app, "rama-0001", "不是 JSON")
        .await
        .expect_err("非法 JSON 应拒绝");
    assert!(matches!(
        err,
        ramaria_core::error::RamariaError::Validation { .. }
    ));
}

#[tokio::test]
async fn rule_import_missing_fields_rejected() {
    let (app, _storage) = build_test_app();
    let err = ramaria_app::commands::behavior::behavior_import_rule(
        &app,
        "rama-0001",
        r#"{"reaction": "x"}"#,
    )
    .await
    .expect_err("缺 situation 应拒绝");
    assert!(matches!(
        err,
        ramaria_core::error::RamariaError::Validation { .. }
    ));
}

#[tokio::test]
async fn rule_import_valid_creates_manual_rule() {
    let (app, storage) = build_test_app();
    let json = r#"{
        "situation": {"keywords": ["失眠"], "valence_mean": -0.6},
        "reaction": "当聊到失眠时，倾向轻声安慰。",
        "avoid": ["睡前聊工作"]
    }"#;
    let id = ramaria_app::commands::behavior::behavior_import_rule(&app, "rama-0001", json)
        .await
        .expect("合法导入成功");
    let rule = storage
        .get_behavior_rule(id)
        .await
        .expect("查询成功")
        .expect("应命中");
    assert_eq!(rule.source, RuleSource::Manual, "导入规则为 Manual");
    assert!(rule.has_reaction());
    assert_eq!(rule.avoid, vec!["睡前聊工作"]);
    assert_eq!(rule.situation.keywords, vec!["失眠"]);
}

// =========================================================
// show / edit / enable / disable / delete / evidence
// =========================================================

#[tokio::test]
async fn rule_show_missing_errors() {
    let (app, _storage) = build_test_app();
    let cmd = RuleCmd::Show { id: 999 };
    assert!(run(&app, cmd, true, false).await.is_err(), "不存在应报错");
}

#[tokio::test]
async fn rule_edit_manualizes_and_writes_feedback() {
    let (app, storage) = build_test_app();
    let id = seed_rule(&storage, "rama-0001").await;
    let cmd = RuleCmd::Edit {
        id,
        reaction: Some("编辑后的规则文本".into()),
        avoid: Some("深夜,加班".into()),
    };
    run(&app, cmd, true, false).await.expect("编辑成功");

    let rule = storage.get_behavior_rule(id).await.unwrap().unwrap();
    assert_eq!(
        rule.source,
        RuleSource::Manual,
        "编辑后转为 Manual（强锚点）"
    );
    assert_eq!(rule.reaction.as_deref(), Some("编辑后的规则文本"));
    assert_eq!(rule.avoid, vec!["深夜", "加班"]);

    // S1 反馈日志
    let logs = storage
        .list_feedback_logs_by_persona("rama-0001")
        .await
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].signal_type, SignalType::Edit);
    assert_eq!(logs[0].weight, 1.0);
}

#[tokio::test]
async fn rule_edit_without_fields_errors() {
    let (app, storage) = build_test_app();
    let id = seed_rule(&storage, "rama-0001").await;
    let cmd = RuleCmd::Edit {
        id,
        reaction: None,
        avoid: None,
    };
    assert!(
        run(&app, cmd, true, false).await.is_err(),
        "无修改字段应报错"
    );
}

#[tokio::test]
async fn rule_disable_writes_feedback() {
    let (app, storage) = build_test_app();
    let id = seed_rule(&storage, "rama-0001").await;
    let cmd = RuleCmd::Disable { id };
    run(&app, cmd, true, false).await.expect("禁用成功");
    assert!(
        !storage
            .get_behavior_rule(id)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );

    let logs = storage
        .list_feedback_logs_by_persona("rama-0001")
        .await
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].signal_type, SignalType::Disable);
}

#[tokio::test]
async fn rule_enable_reenables() {
    let (app, storage) = build_test_app();
    let id = seed_rule(&storage, "rama-0001").await;
    storage.set_rule_enabled(id, false).await.expect("先禁用");
    let cmd = RuleCmd::Enable { id };
    run(&app, cmd, true, false).await.expect("启用成功");
    assert!(
        storage
            .get_behavior_rule(id)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn rule_delete_with_yes_confirms() {
    let (app, storage) = build_test_app();
    let id = seed_rule(&storage, "rama-0001").await;
    let cmd = RuleCmd::Delete { id, force: true };
    run(&app, cmd, true, true).await.expect("删除成功");
    assert!(storage.get_behavior_rule(id).await.unwrap().is_none());
}

#[tokio::test]
async fn rule_delete_missing_errors() {
    let (app, _storage) = build_test_app();
    let cmd = RuleCmd::Delete {
        id: 999,
        force: true,
    };
    assert!(
        run(&app, cmd, true, true).await.is_err(),
        "删除不存在应报错"
    );
}

#[tokio::test]
async fn rule_evidence_traces_chain() {
    let (app, storage) = build_test_app();
    // 预置事件（persona 归属不影响 id 溯源）
    let mut ev = make_test_event(1, "加班事件");
    ev.keywords = Some("加班,累".into());
    ev.paraphrase = Some("对加班感到疲惫".into());
    storage.add_event("rama-0001", ev);

    let mut rule = BehaviorRule::new(
        "rama-0001",
        BehaviorSituation {
            keywords: vec!["加班".into()],
            centroid: None,
            response_centroid: None,
            valence_mean: -0.4,
            valence_std: 0.1,
            sample_count: 2,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 5.0,
            trait_refs: Vec::new(),
        },
        Some("规则文本".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    rule.evidence = vec![BehaviorEvidence {
        event_id: 1,
        weight: 0.8,
    }];
    let id = storage.save_behavior_rule(&rule).await.expect("保存成功");

    let items = ramaria_app::commands::behavior::behavior_rule_evidence(&app, id)
        .await
        .expect("证据链查询成功");
    assert_eq!(items.len(), 1, "溯源到预置事件");
    assert_eq!(items[0].event_id, 1);
    assert_eq!(items[0].title, "加班事件");
    assert_eq!(items[0].paraphrase.as_deref(), Some("对加班感到疲惫"));
}

#[tokio::test]
async fn rule_evidence_missing_rule_errors() {
    let (app, _storage) = build_test_app();
    let cmd = RuleCmd::Evidence { id: 999 };
    assert!(run(&app, cmd, true, false).await.is_err(), "不存在应报错");
}
