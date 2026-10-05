//! crates/ramaria-service/src/proactive/switch/tests.rs - 主动对话人格开关读写单元测试
//!
//! 设计特点:
//! - 覆盖三态往返、缺失回退、非法值回退、键形稳定、人格隔离与显式存储
//! - 使用真实 SQLite 临时库（`settings` 表读写与生产同路径）
//! - 只读写开关键，不依赖网络 / LLM

use super::*;
use crate::test_support::engine_with_db;
use ramaria_core::traits::StoreInfrastructure;

/// 三态往返：save → load 无损（含显式 Auto）。
#[tokio::test]
async fn mode_roundtrip_all_states() {
    let (_engine, storage, dir) = engine_with_db("proactive-switch-roundtrip").await;

    for mode in [
        ProactivePersonaMode::Auto,
        ProactivePersonaMode::On,
        ProactivePersonaMode::Off,
    ] {
        save_mode(storage.as_ref(), "char-0001", mode)
            .await
            .expect("保存应成功");
        let loaded = load_mode(storage.as_ref(), "char-0001")
            .await
            .expect("读取应成功");
        assert_eq!(loaded, mode, "三态应无损往返");
    }

    // trim 后合法值仍可解析
    storage
        .set_setting("proactive.persona.char-0001", "  on  ")
        .await
        .expect("写入应成功");
    let loaded = load_mode(storage.as_ref(), "char-0001")
        .await
        .expect("读取应成功");
    assert_eq!(loaded, ProactivePersonaMode::On, "两侧空白应被容忍");

    let _ = std::fs::remove_dir_all(dir);
}

/// 键缺失 → 自动（空态非错误）。
#[tokio::test]
async fn missing_key_falls_back_to_auto() {
    let (_engine, storage, dir) = engine_with_db("proactive-switch-missing").await;

    let mode = load_mode(storage.as_ref(), "char-none")
        .await
        .expect("缺失键应回退自动而非报错");
    assert_eq!(mode, ProactivePersonaMode::Auto);

    let _ = std::fs::remove_dir_all(dir);
}

/// 非法值 → warn 回退自动（大小写不敏感、布尔文本等均不算合法）。
#[tokio::test]
async fn corrupted_value_falls_back_to_auto() {
    let (_engine, storage, dir) = engine_with_db("proactive-switch-corrupt").await;

    for raw in ["garbage", "ON", "true", "", "au to"] {
        storage
            .set_setting("proactive.persona.char-0001", raw)
            .await
            .expect("写入非法值应成功");
        let mode = load_mode(storage.as_ref(), "char-0001")
            .await
            .expect("非法值应回退自动而非报错");
        assert_eq!(
            mode,
            ProactivePersonaMode::Auto,
            "非法值 {raw:?} 应回退自动"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// 键形稳定：`proactive.persona.{persona_uid}`；save Auto 后键存在（显式存储）。
#[tokio::test]
async fn switch_key_shape_and_explicit_storage() {
    let (_engine, storage, dir) = engine_with_db("proactive-switch-key").await;

    save_mode(storage.as_ref(), "char-0001", ProactivePersonaMode::Auto)
        .await
        .expect("保存应成功");
    let raw = storage
        .get_setting("proactive.persona.char-0001")
        .await
        .expect("读取应成功");
    assert_eq!(
        raw.as_deref(),
        Some("auto"),
        "显式 Auto 应落键（三态均显式存储）"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 人格隔离：不同画像的开关互不串扰。
#[tokio::test]
async fn persona_isolation() {
    let (_engine, storage, dir) = engine_with_db("proactive-switch-isolation").await;

    save_mode(storage.as_ref(), "char-0001", ProactivePersonaMode::Off)
        .await
        .expect("保存应成功");
    save_mode(storage.as_ref(), "char-0002", ProactivePersonaMode::On)
        .await
        .expect("保存应成功");

    assert_eq!(
        load_mode(storage.as_ref(), "char-0001")
            .await
            .expect("读取应成功"),
        ProactivePersonaMode::Off
    );
    assert_eq!(
        load_mode(storage.as_ref(), "char-0002")
            .await
            .expect("读取应成功"),
        ProactivePersonaMode::On
    );

    let _ = std::fs::remove_dir_all(dir);
}
