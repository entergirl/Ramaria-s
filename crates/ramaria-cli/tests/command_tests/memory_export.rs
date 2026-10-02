//! tests/command_tests/memory_export.rs - CLI 记忆 / 导出命令集成测试
//!
//! 设计特点:
//! - memory L1 / L2 / L3（空数据 / 有数据 / 未知 layer）
//! - export JSON / Markdown 与 index rebuild（mock retriever）
//! - 共享 Mock 基建与装配辅助经 `use super::*` 复用（不调用真实 LLM）

use super::common::{
    build_test_engine, make_test_event, make_test_l1, make_test_persona, make_test_trait,
    make_user_message,
};
use super::*;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::BackendConfig;
use ramaria_core::types::PersonaKind;
use uuid::Uuid;

#[tokio::test]
async fn memory_unknown_layer() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l4".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn memory_l1_empty() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l1".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok()); // 空列表无错误
}

#[tokio::test]
async fn memory_l1_with_data() {
    let (engine, storage) = build_test_engine();
    let sid = Uuid::new_v4();
    storage.add_l1(sid, make_test_l1(sid, "测试摘要内容"));

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l1".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l2_empty() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l2".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l2_with_data() {
    let (engine, storage) = build_test_engine();
    storage.add_event("user-0001", make_test_event(1, "测试事件"));

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l2".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l3_empty() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l3".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l3_with_data() {
    let (engine, storage) = build_test_engine();
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("测试标签", ramaria_core::types::TraitLayer::Base),
    );

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l3".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l3_all_layers() {
    let (engine, storage) = build_test_engine();
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("Base标签", ramaria_core::types::TraitLayer::Base),
    );
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("Primary标签", ramaria_core::types::TraitLayer::Primary),
    );
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("Accent标签", ramaria_core::types::TraitLayer::Accent),
    );

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l3".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_with_persona_filter() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::memory::run(
        &engine,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l1".to_string(),
            persona: Some("user-0001".to_string()),
            limit: 5,
            offset: 0,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

// =========================================================
// Export 命令测试
// =========================================================

#[tokio::test]
async fn export_json_empty() {
    let (engine, _storage) = build_test_engine();

    // → output: Some("-") 输出到 stdout，避免依赖 exports/ 目录存在。
    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some("-".to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_json_with_data() {
    let (engine, _storage) = build_engine_with_data().await;

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some("-".to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_json_with_persona() {
    let (engine, _storage) = build_engine_with_data().await;

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: Some("user-0001".to_string()),
            output: Some("-".to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_markdown_empty() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: Some("-".to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_markdown_with_data() {
    let (engine, _storage) = build_engine_with_data().await;

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: Some("-".to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_invalid_format() {
    let (engine, _storage) = build_test_engine();

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "xml".to_string(),
            persona: None,
            output: None,
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn export_json_to_file() {
    let (engine, _storage) = build_engine_with_data().await;
    let tmp_file = std::env::temp_dir().join("ramaria_test_export.json");

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());

    // 验证文件存在且非空
    let content = std::fs::read_to_string(&tmp_file).unwrap();
    assert!(content.contains("ramaria_export"));
    assert!(content.contains("sessions"));

    // 清理
    let _ = std::fs::remove_file(&tmp_file);
}

#[tokio::test]
async fn export_markdown_to_file() {
    let (engine, _storage) = build_engine_with_data().await;
    let tmp_file = std::env::temp_dir().join("ramaria_test_export.md");

    let result = ramaria_cli::commands::export::run(
        &engine,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
            redact: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());

    // 验证 Markdown 文件内容
    let content = std::fs::read_to_string(&tmp_file).unwrap();
    assert!(content.contains("# Ramaria 对话导出"));
    assert!(content.contains("导出时间"));

    // 清理
    let _ = std::fs::remove_file(&tmp_file);
}

// =========================================================
// Index 命令测试
// =========================================================

#[tokio::test]
async fn index_rebuild() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::index_cmd::run(&engine, false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn index_rebuild_with_data() {
    let (engine, _storage) = build_engine_with_data().await;
    let result = ramaria_cli::commands::index_cmd::run(&engine, false).await;
    assert!(result.is_ok());
}

/// 缺索引的库：对话前索引确保完成一次构建并刷新状态（脱离待构建状态）。
#[tokio::test]
async fn ensure_retriever_loaded_refreshes_state_after_rebuild() {
    let dir = temp_config_dir("ensure-index");
    let engine = build_config_test_engine(&dir).await;

    // 配置就绪（本地 provider）+ 构造"缺索引"库（删除 migration 预置的索引版本键）
    engine
        .storage()
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("保存后端配置应成功");
    let pool = engine.sqlite_pool().expect("引擎应持有连接池");
    sqlx::query("DELETE FROM schema_meta WHERE key = 'index_version'")
        .execute(&pool)
        .await
        .expect("删除索引版本键应成功");

    // 入口装配刷新：缺索引 → Indexing
    assert_eq!(
        engine.refresh_setup_state().await.expect("刷新应成功"),
        ramaria_core::types::AppState::Indexing
    );

    // 对话前确保：重建写回版本 + 刷新状态；无嵌入模型 → Degraded
    ramaria_cli::commands::ask::ensure_retriever_loaded(&engine).await;
    assert_eq!(
        engine
            .storage()
            .get_index_version()
            .await
            .expect("读取索引版本应成功"),
        1,
        "重建完成后应写回索引版本 1"
    );
    assert_eq!(
        engine.current_state(),
        ramaria_core::types::AppState::Degraded,
        "重建 + 刷新后应脱离待构建状态"
    );

    close_engine_pool(&engine).await;
    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 隐私确认流程测试
// =========================================================

// =========================================================
// Persona 命令测试
// =========================================================

#[tokio::test]
async fn persona_show_empty() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::Show,
        false,
    )
    .await;
    // 空列表不报错，输出引导提示
    assert!(result.is_ok());
}

#[tokio::test]
async fn persona_show_with_data() {
    let (engine, storage) = build_test_engine();

    // 添加一个带完整 TOML config 的 persona
    let config = r#"[identity]
assistant_name = "黎杋枫"
user_name = "用户"

[blocks]
A_persona = """
你是黎杋枫。测试人格。
"""
E_rules = """
规则内容
"""
"#;
    storage.add_persona(make_test_persona(
        "rama-0001",
        "黎杋枫",
        PersonaKind::Rama,
        Some(config),
    ));
    storage.add_persona(make_test_persona(
        "user-0001",
        "用户",
        PersonaKind::User,
        None,
    ));

    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::Show,
        false,
    )
    .await;
    assert!(result.is_ok()); // 2 个 persona 正常展示
}

#[tokio::test]
async fn persona_show_with_minimal_persona() {
    let (engine, storage) = build_test_engine();

    // 无 config 的 persona（最简情况）
    storage.add_persona(make_test_persona(
        "char-0001",
        "测试角色",
        PersonaKind::Char,
        None,
    ));

    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::Show,
        false,
    )
    .await;
    assert!(result.is_ok()); // 无 config 也能正常展示基本信息
}

#[tokio::test]
async fn persona_reload_directory_not_found() {
    let (engine, _storage) = build_test_engine();
    // reload 依赖 `../config/personas/` 目录，测试环境中通常不存在
    // 此处验证错误提示是否正常
    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::Reload { uid: None },
        false,
    )
    .await;
    // 目录可能不存在，不应 panic，应返回清晰错误
    // （如果存在则通过，不存在则报错——两种情况都是合理的）
    if let Err(ref e) = result {
        let msg = format!("{e}");
        assert!(
            msg.contains("不存在") || msg.contains("未找到"),
            "错误信息应包含路径提示: {msg}"
        );
    }
}

#[tokio::test]
async fn persona_reload_specific_nonexistent_uid() {
    let (engine, _storage) = build_test_engine();
    let result = ramaria_cli::commands::persona::run(
        &engine,
        ramaria_cli::commands::persona::PersonaCmd::Reload {
            uid: Some("nonexistent-999".to_string()),
        },
        false,
    )
    .await;
    // 指定不存在的 UID 应该报错
    assert!(result.is_err());
}

#[tokio::test]
async fn persona_storage_update_works() {
    // 验证 MockStorage 的 update_persona 能正确更新数据
    let (engine, storage) = build_test_engine();

    storage.add_persona(make_test_persona(
        "rama-0001",
        "旧名称",
        PersonaKind::Rama,
        Some("old config"),
    ));

    // 通过 storage trait 更新
    engine
        .storage()
        .update_persona("rama-0001", "新名称", None, Some("new config"), None)
        .await
        .expect("update_persona 应成功");

    // 验证更新结果
    let updated = engine
        .storage()
        .get_persona_by_uid("rama-0001")
        .await
        .expect("查询应成功")
        .expect("persona 应存在");

    assert_eq!(updated.name, "新名称");
    assert_eq!(updated.config.as_deref(), Some("new config"));
}

#[tokio::test]
async fn persona_storage_update_nonexistent_fails() {
    let (engine, _storage) = build_test_engine();

    let result = engine
        .storage()
        .update_persona("nonexistent-uid", "name", None, None, None)
        .await;
    assert!(result.is_err());
}

// =========================================================
// 隐私确认流程测试
// =========================================================

#[tokio::test]
async fn privacy_local_provider_passes() {
    let (engine, _storage) = build_test_engine();
    // MockLlm 使用 LM Studio（本地 provider），确保隐私确认直接通过
    let result = ramaria_cli::privacy::ensure_privacy(&engine, false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn privacy_with_yes_flag() {
    let (engine, _storage) = build_test_engine();
    // --yes 标记，本地 provider 也应正常通过
    let result = ramaria_cli::privacy::ensure_privacy(&engine, true).await;
    assert!(result.is_ok());
}

// =========================================================
// Session 删除测试（通过直接调用 storage 避免交互确认问题）
// =========================================================

#[tokio::test]
async fn session_delete_via_storage() {
    let (_engine, storage) = build_test_engine();
    let sid = Uuid::new_v4();
    storage.create_session_with_messages(sid, vec![make_user_message(sid, "test")]);

    // 通过 storage 直接删除（跳过交互确认）
    storage.delete_session(sid).await.unwrap();
    assert!(storage.get_session(sid).await.unwrap().is_none());
}

// =========================================================
// M1 CLI 契约测试（进程级 CLI 契约）
// =========================================================
// 说明:
// - 进程级测试运行真实二进制（CARGO_BIN_EXE_ramaria）+ 临时 DB，验证
//   stdout 纯净性 / --json 信封结构 / 非 TTY 不挂起 / exit code / alias。
// - 命令级测试复用 MockStorage，验证层级别名、persona list、status 等行为。
// =========================================================
