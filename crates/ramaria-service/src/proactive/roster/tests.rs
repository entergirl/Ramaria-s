//! crates/ramaria-service/src/proactive/roster/tests.rs - 主动消息名单用例单元测试
//!
//! 设计特点:
//! - 读用例矩阵覆盖：三态开关 × 对话有无（自动解锁判定）× 全局总开关 × user 类硬排除
//! - 写用例覆盖校验链（uid 非空 / 值域 / uid 存在 / 非 user）与落键结果
//! - 使用真实 SQLite 临时库（settings / personas / messages 表读写与生产同路径）
//! - 排序断言与存储层人格列表逐项对照，不硬编码具体顺序
//! - 只读写本地库，不依赖网络 / LLM

use super::*;
use crate::proactive::switch::{self, ProactivePersonaMode};
use crate::test_support::{
    MockLlm, engine_with_db, engine_with_llm_and_config, seed_dialogue_history, seed_persona,
    seed_persona_kind,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::PersonaKind;

// =========================================================
// 读用例
// =========================================================

/// 读用例矩阵：三态开关 × 对话有无的生效结论与字段逐项正确（自动态按对话解锁）。
#[tokio::test]
async fn list_mode_matrix_reflects_dialogue_and_switch() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-matrix").await;

    seed_persona(storage.as_ref(), "char-0001").await;
    seed_persona(storage.as_ref(), "char-0002").await;
    seed_dialogue_history(storage.as_ref(), "char-0001").await;

    let cases = [
        (ProactivePersonaMode::Auto, true, false),
        (ProactivePersonaMode::On, true, true),
        (ProactivePersonaMode::Off, false, false),
    ];
    for (mode, effective_with_dialogue, effective_without_dialogue) in cases {
        switch::save_mode(storage.as_ref(), "char-0001", mode)
            .await
            .expect("写入开关应成功");
        switch::save_mode(storage.as_ref(), "char-0002", mode)
            .await
            .expect("写入开关应成功");

        let views = list_personas(&engine).await.expect("读取名单应成功");
        let with_dialogue = views
            .iter()
            .find(|v| v.uid == "char-0001")
            .expect("名单应包含 char-0001");
        let without_dialogue = views
            .iter()
            .find(|v| v.uid == "char-0002")
            .expect("名单应包含 char-0002");

        assert_eq!(with_dialogue.mode, mode.as_str(), "mode 文本应与写入一致");
        assert_eq!(
            without_dialogue.mode,
            mode.as_str(),
            "mode 文本应与写入一致"
        );
        assert!(with_dialogue.has_local_dialogue, "char-0001 应有本地对话");
        assert!(
            !without_dialogue.has_local_dialogue,
            "char-0002 应无本地对话"
        );
        assert_eq!(
            with_dialogue.effective,
            effective_with_dialogue,
            "mode={} 有对话时生效结论应正确",
            mode.as_str()
        );
        assert_eq!(
            without_dialogue.effective,
            effective_without_dialogue,
            "mode={} 无对话时生效结论应正确",
            mode.as_str()
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// user 类硬排除：行值为 user 与 uid 前缀兜底两种形态即使强开也不生效。
#[tokio::test]
async fn list_user_persona_never_effective() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-user").await;

    seed_persona_kind(storage.as_ref(), "user-0001", PersonaKind::User).await;
    seed_dialogue_history(storage.as_ref(), "user-0001").await;
    switch::save_mode(storage.as_ref(), "user-0001", ProactivePersonaMode::On)
        .await
        .expect("写入开关应成功");

    seed_persona_kind(storage.as_ref(), "user-0002", PersonaKind::Char).await;
    seed_dialogue_history(storage.as_ref(), "user-0002").await;
    switch::save_mode(storage.as_ref(), "user-0002", ProactivePersonaMode::On)
        .await
        .expect("写入开关应成功");

    let views = list_personas(&engine).await.expect("读取名单应成功");

    let row = views
        .iter()
        .find(|v| v.uid == "user-0001")
        .expect("名单应包含 user-0001");
    assert_eq!(row.kind, "user", "行值 kind 应透传");
    assert_eq!(row.mode, "on", "显式强开应如实展示");
    assert!(!row.effective, "user 类即使强开也不生效");

    let fallback = views
        .iter()
        .find(|v| v.uid == "user-0002")
        .expect("名单应包含 user-0002");
    assert_eq!(fallback.kind, "char", "行值 kind 应透传");
    assert!(fallback.has_local_dialogue, "前置条件：应有本地对话");
    assert!(!fallback.effective, "uid 前缀兜底为 user 类也不生效");

    let _ = std::fs::remove_dir_all(dir);
}

/// 全局总开关：`[proactive].enabled=false` 时任何人格都不生效（mode 仍如实展示）。
#[tokio::test]
async fn list_respects_global_switch() {
    let mut config = RamariaConfig::default();
    config.proactive.enabled = false;
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-roster-global", MockLlm::local(), config).await;

    seed_persona(storage.as_ref(), "char-0001").await;
    seed_dialogue_history(storage.as_ref(), "char-0001").await;
    switch::save_mode(storage.as_ref(), "char-0001", ProactivePersonaMode::On)
        .await
        .expect("写入开关应成功");

    let views = list_personas(&engine).await.expect("读取名单应成功");
    let row = views
        .iter()
        .find(|v| v.uid == "char-0001")
        .expect("名单应包含 char-0001");
    assert_eq!(row.mode, "on", "全局关闭不影响开关文本展示");
    assert!(!row.effective, "全局总开关关闭时不应生效");

    let _ = std::fs::remove_dir_all(dir);
}

/// 排序稳定性：读用例返回的 uid 序列与存储层人格列表完全一致（不硬编码具体顺序）。
#[tokio::test]
async fn list_order_matches_storage_listing() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-order").await;

    seed_persona(storage.as_ref(), "char-0001").await;
    seed_persona_kind(storage.as_ref(), "user-0001", PersonaKind::User).await;
    seed_persona_kind(storage.as_ref(), "rama-0001", PersonaKind::Rama).await;

    let expected: Vec<String> = storage
        .list_personas()
        .await
        .expect("存储层列表应成功")
        .into_iter()
        .map(|persona| persona.uid)
        .collect();
    let views = list_personas(&engine).await.expect("读取名单应成功");
    let actual: Vec<String> = views.into_iter().map(|view| view.uid).collect();
    assert_eq!(actual, expected, "读用例应沿用人格列表口径（kind, seq）");

    let _ = std::fs::remove_dir_all(dir);
}

/// 空态：库中无人格时返回空数组（非错误）。
#[tokio::test]
async fn list_empty_without_personas() {
    let (engine, _storage, dir) = engine_with_db("proactive-roster-empty").await;

    let views = list_personas(&engine).await.expect("空库读取应成功");
    assert!(views.is_empty(), "无人格时应返回空数组");

    let _ = std::fs::remove_dir_all(dir);
}

/// 坏值回退：开关值为非法文本时读用例按自动态展示（有对话即生效）。
#[tokio::test]
async fn list_corrupted_mode_falls_back_auto() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-corrupt").await;

    seed_persona(storage.as_ref(), "char-0001").await;
    seed_dialogue_history(storage.as_ref(), "char-0001").await;
    storage
        .set_setting("proactive.persona.char-0001", "garbage")
        .await
        .expect("写入非法值应成功");

    let views = list_personas(&engine).await.expect("读取名单应成功");
    let row = views
        .iter()
        .find(|v| v.uid == "char-0001")
        .expect("名单应包含 char-0001");
    assert_eq!(row.mode, "auto", "非法值应回退自动态展示");
    assert!(row.effective, "有对话时自动态应生效");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 写用例
// =========================================================

/// 写用例往返：三态逐个保存后 load_mode 与读用例 mode 均一致，且键值显式存储。
#[tokio::test]
async fn set_roundtrip_all_modes() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-roundtrip").await;

    seed_persona(storage.as_ref(), "char-0001").await;

    for (mode, text) in [
        (ProactivePersonaMode::Auto, "auto"),
        (ProactivePersonaMode::On, "on"),
        (ProactivePersonaMode::Off, "off"),
    ] {
        set_persona_mode(&engine, "char-0001", text)
            .await
            .expect("写用例应成功");

        let loaded = switch::load_mode(storage.as_ref(), "char-0001")
            .await
            .expect("读取开关应成功");
        assert_eq!(loaded, mode, "load_mode 应与写入一致");
        let raw = storage
            .get_setting("proactive.persona.char-0001")
            .await
            .expect("读取原始键应成功");
        assert_eq!(raw.as_deref(), Some(text), "键值应显式存储");

        let views = list_personas(&engine).await.expect("读取名单应成功");
        let row = views
            .iter()
            .find(|v| v.uid == "char-0001")
            .expect("名单应包含 char-0001");
        assert_eq!(row.mode, text, "读用例 mode 文本应与写入一致");
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// 写用例容错：uid 与 mode 两侧空白被容忍，键形落在裁剪后的 uid 上。
#[tokio::test]
async fn set_trims_uid_and_mode() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-trim").await;

    seed_persona(storage.as_ref(), "char-0001").await;

    set_persona_mode(&engine, " char-0001 ", " on ")
        .await
        .expect("带空白的入参应成功");
    let raw = storage
        .get_setting("proactive.persona.char-0001")
        .await
        .expect("读取原始键应成功");
    assert_eq!(raw.as_deref(), Some("on"), "按裁剪后的 uid 落键");

    let _ = std::fs::remove_dir_all(dir);
}

/// 写用例拒绝非法值域：大小写 / 布尔文本 / 空白 / 含空格 / 近似词均不写入。
#[tokio::test]
async fn set_rejects_invalid_mode() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-badmode").await;

    seed_persona(storage.as_ref(), "char-0001").await;

    for bad in ["ON", "true", "", "au to", "enable"] {
        let err = set_persona_mode(&engine, "char-0001", bad)
            .await
            .expect_err("非法值应被拒绝");
        assert!(
            matches!(&err, RamariaError::Validation { .. }),
            "非法值 {bad:?} 应为业务校验错误，实际: {err}"
        );
        let raw = storage
            .get_setting("proactive.persona.char-0001")
            .await
            .expect("读取原始键应成功");
        assert!(raw.is_none(), "非法值 {bad:?} 不应写入开关");
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// 写用例拒绝 user 类：行值为 user 与 uid 前缀兜底两种形态均不可设置。
#[tokio::test]
async fn set_rejects_user_persona() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-userwrite").await;

    seed_persona_kind(storage.as_ref(), "user-0001", PersonaKind::User).await;
    seed_persona_kind(storage.as_ref(), "user-0009", PersonaKind::Char).await;

    for uid in ["user-0001", "user-0009"] {
        let err = set_persona_mode(&engine, uid, "on")
            .await
            .expect_err("user 类应被拒绝");
        assert!(
            matches!(&err, RamariaError::Validation { .. }),
            "uid={uid} 应为业务校验错误，实际: {err}"
        );
        let raw = storage
            .get_setting(&format!("proactive.persona.{uid}"))
            .await
            .expect("读取原始键应成功");
        assert!(raw.is_none(), "被拒的 user 类不应落键: uid={uid}");
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// 写用例拒绝未知 uid 与空 uid：均不产生写入。
#[tokio::test]
async fn set_rejects_unknown_uid() {
    let (engine, storage, dir) = engine_with_db("proactive-roster-unknown").await;

    let err = set_persona_mode(&engine, "char-9999", "on")
        .await
        .expect_err("不存在的人格应被拒绝");
    assert!(
        matches!(&err, RamariaError::Validation { .. }),
        "未知 uid 应为业务校验错误，实际: {err}"
    );

    let err = set_persona_mode(&engine, "   ", "on")
        .await
        .expect_err("空 uid 应被拒绝");
    assert!(
        matches!(&err, RamariaError::Validation { .. }),
        "空 uid 应为业务校验错误，实际: {err}"
    );

    let raw = storage
        .get_setting("proactive.persona.char-9999")
        .await
        .expect("读取原始键应成功");
    assert!(raw.is_none(), "校验失败的调用不应落键");

    let _ = std::fs::remove_dir_all(dir);
}
