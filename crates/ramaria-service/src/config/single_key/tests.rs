//! crates/ramaria-service/src/config/single_key/tests.rs - 单键配置写入用例测试
//!
//! 设计特点:
//! - 真实 SQLite + 临时 config.toml：断言文件 / DB 双侧与内存快照三处一致
//! - 重点覆盖「只改目标键」：同组其它键与其它配置组保持原值

use super::*;
use crate::engine::EngineOptions;
use crate::test_support::temp_dir;

/// 单键写入：双侧生效、快照热替换，同组与其它配置组不变。
#[tokio::test]
async fn set_vision_support_writes_both_sides_and_keeps_other_keys() {
    let dir = temp_dir("config-vision-single-key");
    let config_path = dir.join("config.toml");
    let engine = Engine::open_with(
        EngineOptions::new(dir.join("ramaria.db")).with_config_path(config_path.clone()),
    )
    .await
    .expect("引擎装配应成功");

    // 预置同组另一个键与其它组，验证单键写入不会覆盖
    let mut seeded = engine.load_full_config().await.expect("读取配置应成功");
    seeded.vision.batch_limit = 7;
    seeded.retrieval.rrf_k = 42;
    engine.save_config(&seeded).await.expect("预置配置应成功");

    let outcome = engine
        .set_vision_support(true)
        .await
        .expect("单键写入应成功");
    assert!(outcome.file_ok, "config.toml 应写入成功");
    assert!(outcome.db_ok, "DB 侧应写入成功");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

    // 内存快照：目标键生效，其余键保持
    assert!(engine.config().vision.model_supports_vision);
    assert_eq!(engine.config().vision.batch_limit, 7);
    assert_eq!(engine.config().retrieval.rrf_k, 42);

    // DB 侧：扁平键已写入
    assert_eq!(
        engine
            .setting_get("config.vision.model_supports_vision")
            .await
            .expect("读取设置应成功")
            .as_deref(),
        Some("true")
    );

    // 文件侧：目标键为 true，batch_limit 保持 7
    let text = std::fs::read_to_string(&config_path).expect("读取配置文件应成功");
    assert!(text.contains("model_supports_vision = true"), "{text}");
    assert!(text.contains("batch_limit = 7"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 关闭写入：内存快照与文件同步回落为 false。
#[tokio::test]
async fn set_vision_support_can_disable() {
    let dir = temp_dir("config-vision-disable");
    let config_path = dir.join("config.toml");
    let engine = Engine::open_with(
        EngineOptions::new(dir.join("ramaria.db")).with_config_path(config_path.clone()),
    )
    .await
    .expect("引擎装配应成功");

    engine.set_vision_support(true).await.expect("开启应成功");
    assert!(engine.config().vision.model_supports_vision);

    engine.set_vision_support(false).await.expect("关闭应成功");
    assert!(!engine.config().vision.model_supports_vision);
    let text = std::fs::read_to_string(&config_path).expect("读取配置文件应成功");
    assert!(text.contains("model_supports_vision = false"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}
