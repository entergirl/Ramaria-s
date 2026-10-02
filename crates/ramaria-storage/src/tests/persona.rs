//! crates/ramaria-storage/src/tests/persona.rs - 人格与相关配置存储测试
//!
//! 设计特点:
//! - 覆盖 persona CRUD 与部分字段更新（未指定字段保持旧值）
//! - 覆盖风格统计、隐私确认与 settings 键值读写
//! - update_persona 以 persona_uid 定位并回读校验

use super::*;

#[tokio::test]
async fn persona_crud() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "测试用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    let id = storage.create_persona(&p).await.unwrap();
    assert!(id > 0, "INSERT 后应返回有效的自增 id");

    let got = storage
        .get_persona_by_uid("user-0001")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.name, "测试用户");
    assert_eq!(got.id, id);

    let all = storage.list_personas().await.unwrap();
    assert!(!all.is_empty());
}

/// 单基线包含风格统计表（此前为独立增量迁移），空库初始化后可直接读写。
#[tokio::test]
async fn style_stats_usable_after_single_baseline() {
    use ramaria_core::types::{PersonaStyleStats, StyleRuleSource, StyleStatsStatus, now_ms};
    let storage = setup().await;
    let p = Persona::new(
        "char-style".into(),
        "风格角色".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let stats = PersonaStyleStats {
        persona_uid: "char-style".to_string(),
        sample_count: 3,
        stats_json: r#"{"metric":"mock"}"#.to_string(),
        baseline_version: 0,
        rule_text: None,
        rule_source: StyleRuleSource::None,
        status: StyleStatsStatus::Insufficient,
        updated_at: now_ms(),
    };
    storage.upsert_style_stats(&stats).await.unwrap();
    let got = storage
        .get_style_stats("char-style")
        .await
        .unwrap()
        .expect("风格统计应存在");
    assert_eq!(got.persona_uid, "char-style");
    assert_eq!(got.sample_count, 3);
}

#[tokio::test]
async fn privacy_consent_crud() {
    let storage = setup().await;
    let consent = PrivacyConsent::new(
        ramaria_core::types::LlmProvider::DeepSeek,
        "https://api.deepseek.com/v1".into(),
        true,
    );
    storage.save_privacy_consent(&consent).await.unwrap();
    let got = storage
        .get_privacy_consent("deepseek", "https://api.deepseek.com/v1")
        .await
        .unwrap();
    assert!(got.is_some());
    assert_eq!(
        got.unwrap().provider,
        ramaria_core::types::LlmProvider::DeepSeek
    );
}

#[tokio::test]
async fn settings_crud() {
    let storage = setup().await;
    storage.set_setting("profile_mode", "full").await.unwrap();
    let val = storage.get_setting("profile_mode").await.unwrap();
    assert_eq!(val.as_deref(), Some("full"));
    let all = storage.list_settings().await.unwrap();
    assert!(!all.is_empty());
}

#[tokio::test]
async fn update_persona_name() {
    let (storage, persona_uid, _, _) = setup_with_persona().await;

    // 更新名称
    storage
        .update_persona(&persona_uid, "新名称", None, None, None)
        .await
        .unwrap();

    let updated = storage
        .get_persona_by_uid(&persona_uid)
        .await
        .unwrap()
        .expect("persona 应存在");
    assert_eq!(updated.name, "新名称");
}

#[tokio::test]
async fn update_persona_avatar_and_config() {
    let (storage, persona_uid, _, _) = setup_with_persona().await;

    // 更新头像和 config JSON
    storage
        .update_persona(
            &persona_uid,
            "测试角色", // name 不变
            Some("avatar_url_here"),
            Some(r#"{"description":"更新后的描述"}"#),
            None, // description 保持旧值
        )
        .await
        .unwrap();

    let updated = storage
        .get_persona_by_uid(&persona_uid)
        .await
        .unwrap()
        .expect("persona 应存在");
    assert_eq!(updated.avatar.as_deref(), Some("avatar_url_here"));
    assert!(updated.config.is_some());
    assert!(updated.config.unwrap().contains("更新后的描述"));
}

#[tokio::test]
async fn update_persona_partial_fields() {
    // 只更新部分字段，验证未指定的字段不被覆盖
    let (storage, persona_uid, _, _) = setup_with_persona().await;

    // 先设置头像
    storage
        .update_persona(&persona_uid, "测试角色", Some("old_avatar"), None, None)
        .await
        .unwrap();

    // 再只更新 config，头像应保持不变
    storage
        .update_persona(
            &persona_uid,
            "测试角色",
            None, // avatar 传 None 不更新
            Some(r#"{"key":"value"}"#),
            None, // description 保持旧值
        )
        .await
        .unwrap();

    let updated = storage
        .get_persona_by_uid(&persona_uid)
        .await
        .unwrap()
        .expect("persona 应存在");
    assert_eq!(
        updated.avatar.as_deref(),
        Some("old_avatar"),
        "未传入 avatar 时应保持旧值"
    );
    assert!(updated.config.is_some());
}
