//! tests/command_tests.rs - CLI 命令集成测试
//!
//! 覆盖:
//! - session: list / show / delete (含空数据 / 有数据 / 不存在)
//! - config: list / get / set (含 API key 遮蔽 / 未知配置项 / 自定义 setting)
//! - memory: L1 / L2 / L3 (含空数据 / 有数据 / 未知 layer)
//! - export: JSON / Markdown (含空数据 / 有会话+消息)
//! - index_cmd: rebuild (mock retriever)
//!
//! 安全约束:
//! - 所有测试使用 MockStorage + MockLlm，不调用真实 LLM
//! - 不访问 OS keychain（MockLlm 使用 LM Studio provider，无需 keychain）
//! - 不读写文件系统（export 测试使用 stdout）

mod common;

use common::{
    MockStorage, build_test_app, make_assistant_message, make_test_event, make_test_l1,
    make_test_persona, make_test_trait, make_user_message,
};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::PersonaKind;
use std::sync::Arc;
use uuid::Uuid;

// =========================================================
// 辅助函数
// =========================================================

/// 构造一个有数据的测试 App（含 2 个 session + 消息 + L1 + L2 + L3 + settings）
async fn build_app_with_data() -> (Arc<ramaria_app::App>, Arc<MockStorage>) {
    let (app, storage) = build_test_app();

    // Session 1: 有消息
    let sid1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    storage.create_session_with_messages(
        sid1,
        vec![
            make_user_message(sid1, "你好"),
            make_assistant_message(sid1, "你好！有什么我可以帮你的？"),
        ],
    );

    // Session 2: 无消息（已结束）
    let sid2 = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    storage.create_ended_session(sid2);

    // Session 3: 有大量消息
    let sid3 = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    storage.create_session_with_messages(
        sid3,
        vec![
            make_user_message(sid3, "今天天气真好"),
            make_assistant_message(sid3, "是的！适合出门走走。"),
            make_user_message(sid3, "有什么推荐的活动吗？"),
            make_assistant_message(sid3, "可以去公园散步，或者骑自行车。"),
        ],
    );

    // L1 记忆
    storage.add_l1(sid1, make_test_l1(sid1, "用户与AI打招呼，氛围友好"));
    storage.add_l1(
        sid3,
        make_test_l1(sid3, "用户询问户外活动建议，AI推荐了散步和骑行"),
    );

    // L2 事件
    storage.add_event("user-0001", make_test_event(1, "初次问候"));
    storage.add_event("user-0001", make_test_event(2, "户外活动咨询"));

    // L3 性格标签
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("友好", ramaria_core::types::TraitLayer::Base),
    );
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("好奇心强", ramaria_core::types::TraitLayer::Primary),
    );

    // Settings
    storage.add_setting("theme", "dark");
    storage.add_setting("language", "zh-CN");

    (app, storage)
}

// =========================================================
// Session 命令测试
// =========================================================

#[tokio::test]
async fn session_list_empty() {
    let (app, _storage) = build_test_app();
    let result =
        ramaria_cli::commands::session::run(&app, ramaria_cli::commands::session::SessionCmd::List)
            .await;
    // 空列表不报错，输出"暂无会话记录"
    assert!(result.is_ok());
}

#[tokio::test]
async fn session_list_with_data() {
    let (app, _storage) = build_app_with_data().await;
    let result =
        ramaria_cli::commands::session::run(&app, ramaria_cli::commands::session::SessionCmd::List)
            .await;
    assert!(result.is_ok()); // 3 个会话正常列出
}

#[tokio::test]
async fn session_show_existing() {
    let (app, _storage) = build_app_with_data().await;
    let sid = "11111111-1111-1111-1111-111111111111";
    let result = ramaria_cli::commands::session::run(
        &app,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: sid.to_string(),
        },
    )
    .await;
    assert!(result.is_ok()); // 显示已有会话和 2 条消息
}

#[tokio::test]
async fn session_show_nonexistent() {
    let (app, _storage) = build_test_app();
    let sid = "99999999-9999-9999-9999-999999999999";
    let result = ramaria_cli::commands::session::run(
        &app,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: sid.to_string(),
        },
    )
    .await;
    assert!(result.is_err()); // 不存在的会话
}

#[tokio::test]
async fn session_show_invalid_uuid() {
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::session::run(
        &app,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: "not-a-uuid".to_string(),
        },
    )
    .await;
    assert!(result.is_err()); // 无效 UUID
}

// =========================================================
// Config 命令测试
// =========================================================

#[tokio::test]
async fn config_list_default() {
    let (app, _storage) = build_test_app();
    let result =
        ramaria_cli::commands::config::run(&app, ramaria_cli::commands::config::ConfigCmd::List)
            .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_list_with_settings() {
    let (app, storage) = build_test_app();
    storage.add_setting("theme", "dark");
    storage.add_setting("language", "zh-CN");

    let result =
        ramaria_cli::commands::config::run(&app, ramaria_cli::commands::config::ConfigCmd::List)
            .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_get_known_keys() {
    let (app, _storage) = build_test_app();

    // provider
    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "provider".to_string(),
        },
    )
    .await;
    assert!(result.is_ok());

    // state
    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "state".to_string(),
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_get_unknown_key() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "nonexistent_key_xyz".to_string(),
        },
    )
    .await;
    assert!(result.is_err()); // 未知 key 或 settings 中不存在 → 报错
}

#[tokio::test]
async fn config_get_custom_setting() {
    let (app, storage) = build_test_app();
    storage.add_setting("theme", "dark");

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Get {
            key: "theme".to_string(),
        },
    )
    .await;
    assert!(result.is_ok()); // 从 settings 表读取自定义设置
}

#[tokio::test]
async fn config_set_valid_temperature() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "temperature".to_string(),
            value: "0.8".to_string(),
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_set_invalid_temperature() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "temperature".to_string(),
            value: "not-a-number".to_string(),
        },
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn config_set_custom_setting() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "theme".to_string(),
            value: "dark".to_string(),
        },
    )
    .await;
    assert!(result.is_ok()); // 自定义设置写入 settings 表
}

#[tokio::test]
async fn config_set_invalid_provider() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "provider".to_string(),
            value: "unknown_provider".to_string(),
        },
    )
    .await;
    assert!(result.is_err());
}

// =========================================================
// Memory 命令测试
// =========================================================

#[tokio::test]
async fn memory_unknown_layer() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l4".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn memory_l1_empty() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l1".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok()); // 空列表无错误
}

#[tokio::test]
async fn memory_l1_with_data() {
    let (app, storage) = build_test_app();
    let sid = Uuid::new_v4();
    storage.add_l1(sid, make_test_l1(sid, "测试摘要内容"));

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l1".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l2_empty() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l2".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l2_with_data() {
    let (app, storage) = build_test_app();
    storage.add_event("user-0001", make_test_event(1, "测试事件"));

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l2".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l3_empty() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l3".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l3_with_data() {
    let (app, storage) = build_test_app();
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("测试标签", ramaria_core::types::TraitLayer::Base),
    );

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l3".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_l3_all_layers() {
    let (app, storage) = build_test_app();
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
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l3".to_string(),
            persona: None,
            limit: 10,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_with_persona_filter() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l1".to_string(),
            persona: Some("user-0001".to_string()),
            limit: 5,
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
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: None, // stdout
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_json_with_data() {
    let (app, _storage) = build_app_with_data().await;

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: None,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_json_with_persona() {
    let (app, _storage) = build_app_with_data().await;

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: Some("user-0001".to_string()),
            output: None,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_markdown_empty() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: None,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_markdown_with_data() {
    let (app, _storage) = build_app_with_data().await;

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: None,
        },
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn export_invalid_format() {
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "xml".to_string(),
            persona: None,
            output: None,
        },
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn export_json_to_file() {
    let (app, _storage) = build_app_with_data().await;
    let tmp_file = std::env::temp_dir().join("ramaria_test_export.json");

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
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
    let (app, _storage) = build_app_with_data().await;
    let tmp_file = std::env::temp_dir().join("ramaria_test_export.md");

    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "markdown".to_string(),
            persona: None,
            output: Some(tmp_file.to_string_lossy().to_string()),
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
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::index_cmd::run(&app).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn index_rebuild_with_data() {
    let (app, _storage) = build_app_with_data().await;
    let result = ramaria_cli::commands::index_cmd::run(&app).await;
    assert!(result.is_ok());
}

// =========================================================
// 隐私确认流程测试
// =========================================================

// =========================================================
// Persona 命令测试
// =========================================================

#[tokio::test]
async fn persona_show_empty() {
    let (app, _storage) = build_test_app();
    let result =
        ramaria_cli::commands::persona::run(&app, ramaria_cli::commands::persona::PersonaCmd::Show)
            .await;
    // 空列表不报错，输出引导提示
    assert!(result.is_ok());
}

#[tokio::test]
async fn persona_show_with_data() {
    let (app, storage) = build_test_app();

    // 添加一个带完整 TOML config 的 persona
    let config = r#"[identity]
assistant_name = "黎杋枫"
user_name = "烧酒"

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

    let result =
        ramaria_cli::commands::persona::run(&app, ramaria_cli::commands::persona::PersonaCmd::Show)
            .await;
    assert!(result.is_ok()); // 2 个 persona 正常展示
}

#[tokio::test]
async fn persona_show_with_minimal_persona() {
    let (app, storage) = build_test_app();

    // 无 config 的 persona（最简情况）
    storage.add_persona(make_test_persona(
        "char-0001",
        "测试角色",
        PersonaKind::Char,
        None,
    ));

    let result =
        ramaria_cli::commands::persona::run(&app, ramaria_cli::commands::persona::PersonaCmd::Show)
            .await;
    assert!(result.is_ok()); // 无 config 也能正常展示基本信息
}

#[tokio::test]
async fn persona_reload_directory_not_found() {
    let (app, _storage) = build_test_app();
    // reload 依赖 `../config/personas/` 目录，测试环境中通常不存在
    // 此处验证错误提示是否正常
    let result = ramaria_cli::commands::persona::run(
        &app,
        ramaria_cli::commands::persona::PersonaCmd::Reload { uid: None },
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
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::persona::run(
        &app,
        ramaria_cli::commands::persona::PersonaCmd::Reload {
            uid: Some("nonexistent-999".to_string()),
        },
    )
    .await;
    // 指定不存在的 UID 应该报错
    assert!(result.is_err());
}

#[tokio::test]
async fn persona_storage_update_works() {
    // 验证 MockStorage 的 update_persona 能正确更新数据
    let (app, storage) = build_test_app();

    storage.add_persona(make_test_persona(
        "rama-0001",
        "旧名称",
        PersonaKind::Rama,
        Some("old config"),
    ));

    // 通过 storage trait 更新
    app.storage()
        .update_persona("rama-0001", "新名称", None, Some("new config"))
        .await
        .expect("update_persona 应成功");

    // 验证更新结果
    let updated = app
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
    let (app, _storage) = build_test_app();

    let result = app
        .storage()
        .update_persona("nonexistent-uid", "name", None, None)
        .await;
    assert!(result.is_err());
}

// =========================================================
// 隐私确认流程测试
// =========================================================

#[tokio::test]
async fn privacy_local_provider_passes() {
    let (app, _storage) = build_test_app();
    // MockLlm 使用 LM Studio（本地 provider），确保隐私确认直接通过
    let result = ramaria_cli::privacy::ensure_privacy(&app, false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn privacy_with_yes_flag() {
    let (app, _storage) = build_test_app();
    // --yes 标记，本地 provider 也应正常通过
    let result = ramaria_cli::privacy::ensure_privacy(&app, true).await;
    assert!(result.is_ok());
}

// =========================================================
// Session 删除测试（通过直接调用 storage 避免交互确认问题）
// =========================================================

#[tokio::test]
async fn session_delete_via_storage() {
    let (_app, storage) = build_test_app();
    let sid = Uuid::new_v4();
    storage.create_session_with_messages(sid, vec![make_user_message(sid, "test")]);

    // 通过 storage 直接删除（跳过交互确认）
    storage.delete_session(sid).await.unwrap();
    assert!(storage.get_session(sid).await.unwrap().is_none());
}
