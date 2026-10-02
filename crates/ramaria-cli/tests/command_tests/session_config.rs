//! tests/command_tests/session_config.rs - CLI 会话 / 配置命令集成测试
//!
//! 设计特点:
//! - session list / show / delete（空数据 / 有数据 / 不存在）
//! - config list / get / set（API key 遮蔽 / 未知项 / 自定义 setting）
//! - 共享 Mock 基建与装配辅助经 `use super::*` 复用（不调用真实 LLM）

use super::common::{build_test_engine, make_assistant_message, make_user_message};
use super::*;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::StoreInfrastructure;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn session_list_empty() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::session::run(
        &engine,
        ramaria_cli::commands::session::SessionCmd::List {
            limit: None,
            offset: 0,
        },
        false,
        false,
    )
    .await;
    // 空列表不报错，输出"暂无会话记录"
    assert!(result.is_ok());
}

#[tokio::test]
async fn session_list_with_data() {
    let (engine, _storage) = build_engine_with_data().await;
    let result = ramaria_cli::commands::session::run(
        &engine,
        ramaria_cli::commands::session::SessionCmd::List {
            limit: None,
            offset: 0,
        },
        false,
        false,
    )
    .await;
    assert!(result.is_ok()); // 3 个会话正常列出
}

#[tokio::test]
async fn session_show_existing() {
    let (engine, _storage) = build_engine_with_data().await;
    let sid = "11111111-1111-1111-1111-111111111111";
    let result = ramaria_cli::commands::session::run(
        &engine,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: sid.to_string(),
        },
        false,
        false,
    )
    .await;
    assert!(result.is_ok()); // 显示已有会话和 2 条消息
}

#[tokio::test]
async fn session_show_nonexistent() {
    let (engine, _storage) = build_test_engine();
    let sid = "99999999-9999-9999-9999-999999999999";
    let result = ramaria_cli::commands::session::run(
        &engine,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: sid.to_string(),
        },
        false,
        false,
    )
    .await;
    assert!(result.is_err()); // 不存在的会话
}

#[tokio::test]
async fn session_show_invalid_uuid() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::session::run(
        &engine,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: "not-a-uuid".to_string(),
        },
        false,
        false,
    )
    .await;
    assert!(result.is_err()); // 无效 UUID
}

/// session summarize 在 LLM 恒失败时返回的错误必须保留 RamariaError source
/// （否则退出码退化为 1，破坏 3=LLM/Embedding/Storage 契约）。
/// 注：L1 生成走 JobManager 自带重试（指数退避 ~3s），属预期耗时。
#[tokio::test]
async fn session_summarize_preserves_ramaria_error_for_exit_code() {
    let (engine, storage) = build_engine_with_llm(Arc::new(FailingLlm::new()));
    let sid = Uuid::new_v4();
    storage.create_session_with_messages(
        sid,
        vec![
            make_user_message(sid, "你好"),
            make_assistant_message(sid, "你好！有什么我可以帮你的？"),
        ],
    );

    let result = ramaria_cli::commands::session::run(
        &engine,
        ramaria_cli::commands::session::SessionCmd::Summarize {
            session_id: sid.to_string(),
            persona_uid: None,
            progressive: false,
        },
        false,
        true,
    )
    .await;

    let err = result.expect_err("LLM 恒失败应返回 Err");
    let re = err
        .chain()
        .find_map(|e| e.downcast_ref::<RamariaError>())
        .expect("必须保留 RamariaError source（否则退出码退化为 1）");
    assert!(
        matches!(re, RamariaError::Llm { .. }),
        "L1 失败应映射为 Llm 类错误，实际: {re:?}"
    );
}

// =========================================================
// Config 命令测试
// =========================================================

#[tokio::test]
async fn config_list_default() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::List,
        false,
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_list_with_settings() {
    let (engine, storage) = build_test_engine();
    storage.add_setting("theme", "dark");
    storage.add_setting("language", "zh-CN");

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::List,
        false,
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_get_known_keys() {
    let (engine, _storage) = build_test_engine();

    // provider
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "provider".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());

    // state
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "state".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_get_unknown_key() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "nonexistent_key_xyz".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_err()); // 未知 key 或 settings 中不存在 → 报错
}

#[tokio::test]
async fn config_get_custom_setting() {
    let (engine, storage) = build_test_engine();
    storage.add_setting("theme", "dark");

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "theme".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok()); // 从 settings 表读取自定义设置
}

#[tokio::test]
async fn config_set_valid_temperature() {
    // 配置双写需要真实库与配置文件：本测试使用临时目录承接双写产物，
    // 不得改写到 crate 根（cwd）下的被跟踪文件。
    let dir = temp_config_dir("temperature");
    let engine = build_config_test_engine(&dir).await;

    let repo_config = std::path::Path::new("config.toml");
    let before = std::fs::read_to_string(repo_config).ok();

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "temperature".to_string(),
            value: "0.8".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());

    let after = std::fs::read_to_string(repo_config).ok();
    assert_eq!(
        before, after,
        "测试不得改写仓库内 crates/ramaria-cli/config.toml"
    );
    // 写入路径确实落在配置目录（临时目录），确保上面的断言不是在空路径上通过
    assert!(
        dir.join("config.toml").exists(),
        "config set 应把 config.toml 写入配置目录（临时目录）"
    );

    close_engine_pool(&engine).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn config_set_invalid_temperature() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "temperature".to_string(),
            value: "not-a-number".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn config_set_custom_setting() {
    let (engine, storage) = build_test_engine();
    storage.add_setting("theme", "dark");

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "theme".to_string(),
            value: "light".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok()); // 自定义设置项已存在时仍可更新（回归既有能力）
    let value = storage.get_setting("theme").await.unwrap();
    assert_eq!(value.as_deref(), Some("light"));
}

#[tokio::test]
async fn config_set_unknown_key_rejected() {
    let (engine, storage) = build_test_engine();

    // 未知 key（如 backend.provider）必须报错，且不得写入 settings 表（避免静默假成功）
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "backend.provider".to_string(),
            value: "deepseek".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_err());
    let setting = storage.get_setting("backend.provider").await.unwrap();
    assert!(setting.is_none());
}

#[tokio::test]
async fn config_set_provider_persists_to_config_toml() {
    let dir = temp_config_dir("provider");
    let engine = build_config_test_engine(&dir).await;

    // 设置 provider
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "provider".to_string(),
            value: "deepseek".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());

    // 1) DB 侧已更新
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.provider, ramaria_core::types::LlmProvider::DeepSeek);

    // 2) config.toml 文件侧已同步（[backend] 组）
    let config_path = dir.join("config.toml");
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("provider = \"deepseek\""),
        "config.toml 应包含 deepseek: {content}"
    );

    // 3) 模拟重启：配置同步以文件为准回写 →
    //    文件与 DB 一致 → 无 mismatch → DB 不被覆盖回默认值
    let outcome = engine.reload_config().await.unwrap();
    assert!(
        outcome.mismatches.is_empty(),
        "重启后文件与 DB 应一致: {:?}",
        outcome.mismatches
    );
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.provider,
        ramaria_core::types::LlmProvider::DeepSeek,
        "重启后 provider 不得被覆盖回默认值"
    );

    close_engine_pool(&engine).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn config_embedding_model_path_roundtrip() {
    let dir = temp_config_dir("embed");
    let engine = build_config_test_engine(&dir).await;

    // 设置
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "embedding_model_path".to_string(),
            value: "/models/bge-m3.gguf".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.embedding_model_path.as_deref(),
        Some("/models/bge-m3.gguf")
    );

    // 读取（未配置时应输出 (未设置)，设置后正常返回）
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "embedding_model_path".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());

    // 清空（空字符串视为清除）
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "embedding_model_path".to_string(),
            value: "".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .unwrap()
        .unwrap();
    assert!(saved.embedding_model_path.is_none());

    // 清空后读取仍成功
    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "embedding_model_path".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_ok());

    close_engine_pool(&engine).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn config_set_invalid_provider() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::config::run(
        &engine,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "provider".to_string(),
            value: "unknown_provider".to_string(),
        },
        false,
    )
    .await;
    assert!(result.is_err());
}

// =========================================================
// Memory 命令测试
// =========================================================
