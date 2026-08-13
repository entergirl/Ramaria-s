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
    let result = ramaria_cli::commands::session::run(
        &app,
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
    let (app, _storage) = build_app_with_data().await;
    let result = ramaria_cli::commands::session::run(
        &app,
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
    let (app, _storage) = build_app_with_data().await;
    let sid = "11111111-1111-1111-1111-111111111111";
    let result = ramaria_cli::commands::session::run(
        &app,
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
    let (app, _storage) = build_test_app();
    let sid = "99999999-9999-9999-9999-999999999999";
    let result = ramaria_cli::commands::session::run(
        &app,
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
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::session::run(
        &app,
        ramaria_cli::commands::session::SessionCmd::Show {
            session_id: "not-a-uuid".to_string(),
        },
        false,
        false,
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
    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::List,
        false,
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn config_list_with_settings() {
    let (app, storage) = build_test_app();
    storage.add_setting("theme", "dark");
    storage.add_setting("language", "zh-CN");

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::List,
        false,
    )
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
        false,
    )
    .await;
    assert!(result.is_ok());

    // state
    let result = ramaria_cli::commands::config::run(
        &app,
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
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
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
    let (app, storage) = build_test_app();
    storage.add_setting("theme", "dark");

    let result = ramaria_cli::commands::config::run(
        &app,
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
    let (app, _storage) = build_test_app();

    let result = ramaria_cli::commands::config::run(
        &app,
        ramaria_cli::commands::config::ConfigCmd::Set {
            key: "temperature".to_string(),
            value: "0.8".to_string(),
        },
        false,
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
        false,
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
        false,
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
        false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
            offset: 0,
            json: false,
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
    let (app, _storage) = build_test_app();

    // → output: Some("-") 输出到 stdout，避免依赖 exports/ 目录存在。
    let result = ramaria_cli::commands::export::run(
        &app,
        ramaria_cli::commands::export::ExportArgs {
            format: "json".to_string(),
            persona: None,
            output: Some("-".to_string()),
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
            output: Some("-".to_string()),
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
            output: Some("-".to_string()),
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
            output: Some("-".to_string()),
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
            output: Some("-".to_string()),
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
    let result = ramaria_cli::commands::persona::run(
        &app,
        ramaria_cli::commands::persona::PersonaCmd::Show,
        false,
    )
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
        &app,
        ramaria_cli::commands::persona::PersonaCmd::Show,
        false,
    )
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

    let result = ramaria_cli::commands::persona::run(
        &app,
        ramaria_cli::commands::persona::PersonaCmd::Show,
        false,
    )
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
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::persona::run(
        &app,
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
    let (app, storage) = build_test_app();

    storage.add_persona(make_test_persona(
        "rama-0001",
        "旧名称",
        PersonaKind::Rama,
        Some("old config"),
    ));

    // 通过 storage trait 更新
    app.storage()
        .update_persona("rama-0001", "新名称", None, Some("new config"), None)
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
        .update_persona("nonexistent-uid", "name", None, None, None)
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

// =========================================================
// M1 CLI 契约测试（T-V15-1-008）
// =========================================================
// 说明:
// - 进程级测试运行真实二进制（CARGO_BIN_EXE_ramaria）+ 临时 DB，验证
//   stdout 纯净性 / --json 信封结构 / 非 TTY 不挂起 / exit code / alias。
// - 命令级测试复用 MockStorage，验证层级别名、persona list、status 等行为。
// =========================================================

/// 临时 DB 目录序号（避免并行测试共享目录）。
static DB_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 以真实二进制运行 CLI（临时 DB，进程退出后清理）。
fn run_cli(args: &[&str]) -> std::process::Output {
    let seq = DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db_dir = std::env::temp_dir().join(format!(
        "ramaria_cli_contract_{}_{}",
        std::process::id(),
        seq
    ));
    let _ = std::fs::create_dir_all(&db_dir);
    let db = db_dir.join("contract.db");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(&db)
        .output()
        .expect("运行 ramaria 二进制失败");
    let _ = std::fs::remove_dir_all(&db_dir);
    out
}

/// `--json` 信封结构 + stdout 纯净性：stdout 仅含一行合法 JSON 信封。
#[test]
fn json_envelope_stdout_purity() {
    let out = run_cli(&["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "status --json 应成功退出");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout 应只含一行 JSON，实际: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], true, "信封 ok 应为 true");
    assert!(parsed["data"]["state"].is_string(), "data.state 应存在");
    assert!(parsed["data"]["db_path"].is_string(), "data.db_path 应存在");
    // stderr 应包含状态提示（信息/日志走 stderr，不污染 stdout）
    assert!(!out.stderr.is_empty(), "stderr 应含日志/提示");
}

/// `--json` 错误信封：业务校验失败 → ok=false + error.code=4（D-V15-011）。
#[test]
fn json_error_envelope_validation_code() {
    let out = run_cli(&["memory", "l4", "--json"]);
    assert_eq!(out.status.code(), Some(4), "业务校验失败应退出 code 4");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout 必须是合法 JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], 4);
    assert!(parsed["error"]["message"].is_string());
    // 文本错误同时走 stderr
    assert!(!out.stderr.is_empty());
}

/// 非 TTY 且无 --yes 不挂起：session delete 直接失败并提示 --yes（M1 B 项）。
#[test]
fn non_tty_without_yes_does_not_hang() {
    let out = run_cli(&["session", "delete", "11111111-1111-1111-1111-111111111111"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "非 TTY 无 --yes 应失败退出（不挂起）"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "提示应包含 --yes，实际: {stderr}");
}

/// `--yes` 自动确认：非 TTY + --yes 跳过确认（M1 B 项）。
#[test]
fn yes_flag_skips_confirmation() {
    let out = run_cli(&[
        "session",
        "delete",
        "11111111-1111-1111-1111-111111111111",
        "--yes",
    ]);
    assert_eq!(out.status.code(), Some(0), "有 --yes 应通过确认并成功退出");
}

/// help 分组：--help 显示 对话/记忆/数据/管理/高级 分组（§2.9）。
#[test]
fn help_grouped_sections() {
    let out = run_cli(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for section in ["对话", "记忆", "数据", "管理", "高级"] {
        assert!(stdout.contains(section), "--help 应包含分组 {section}");
    }
}

/// blocks canonical + utt alias 双支持（D-V15-007）。
#[test]
fn blocks_and_utt_alias() {
    let out_blocks = run_cli(&["blocks", "rebuild", "--help"]);
    assert_eq!(out_blocks.status.code(), Some(0), "blocks 命令应可用");
    let out_utt = run_cli(&["utt", "rebuild", "--help"]);
    assert_eq!(out_utt.status.code(), Some(0), "utt alias 应可用");
}

// =========================================================
// M1 命令级契约测试
// =========================================================

/// memory 层级别名双支持：summary/events/profile 与 l1/l2/l3 等价（D-V15-007）。
#[tokio::test]
async fn memory_layer_aliases_ok() {
    let (app, _storage) = build_test_app();
    for layer in ["summary", "events", "profile"] {
        let result = ramaria_cli::commands::memory::run(
            &app,
            ramaria_cli::commands::memory::MemoryArgs {
                layer: layer.to_string(),
                persona: None,
                limit: 10,
                offset: 0,
                json: false,
            },
        )
        .await;
        assert!(result.is_ok(), "层级别名 {layer} 应可用");
    }
}

/// memory 未知层级纠错提示：可用值 summary/events/profile（或 l1/l2/l3）。
#[tokio::test]
async fn memory_unknown_layer_suggestion() {
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::memory::run(
        &app,
        ramaria_cli::commands::memory::MemoryArgs {
            layer: "l4".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        },
    )
    .await;
    let err = result.expect_err("未知层级应报错");
    let msg = format!("{err}");
    for hint in ["summary", "events", "profile"] {
        assert!(msg.contains(hint), "纠错提示应含 {hint}: {msg}");
    }
}

/// persona list：空数据不报错。
#[tokio::test]
async fn persona_list_empty() {
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::persona::run(
        &app,
        ramaria_cli::commands::persona::PersonaCmd::List {
            limit: None,
            offset: 0,
        },
        false,
    )
    .await;
    assert!(result.is_ok());
}

/// persona list：结构化字段（uid/name/kind）不报错。
#[tokio::test]
async fn persona_list_with_data() {
    let (app, storage) = build_test_app();
    storage.add_persona(make_test_persona(
        "rama-0001",
        "黎杋枫",
        PersonaKind::Rama,
        None,
    ));
    storage.add_persona(make_test_persona(
        "user-0001",
        "用户",
        PersonaKind::User,
        None,
    ));
    let result = ramaria_cli::commands::persona::run(
        &app,
        ramaria_cli::commands::persona::PersonaCmd::List {
            limit: None,
            offset: 0,
        },
        false,
    )
    .await;
    assert!(result.is_ok());
}

/// status 命令（agent 探活）：mock app 可执行。
#[tokio::test]
async fn status_command_ok() {
    let (app, _storage) = build_test_app();
    let result = ramaria_cli::commands::status::run(
        &app,
        ramaria_cli::commands::status::StatusArgs {
            db_path: std::path::PathBuf::from("data/test.db"),
            json: false,
        },
    )
    .await;
    assert!(result.is_ok());
}

/// import --dry-run 不产生任何数据库写入（命令级：无文件时不报 panic）。
/// 注：真实 dry-run 路径依赖 qq-chat-exporter 文件，进程级验证由 M1 手动验收覆盖；
/// 此处验证 dry_run 参数不影响现有命令行为（文件缺失仍为业务校验错误）。
#[tokio::test]
async fn import_dry_run_missing_file_is_validation_error() {
    let (app, _storage) = build_test_app();
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let result = ramaria_cli::commands::import_cmd::run(
        &app,
        &pool,
        ramaria_cli::commands::import_cmd::ImportArgs {
            file: "nonexistent_file.json".to_string(),
            deep: false,
            dry_run: true,
            persona_self_name: None,
            persona_self_uid: None,
            persona_other_name: None,
            persona_other_uid: None,
            gap: 10,
            yes: false,
            json: false,
        },
    )
    .await;
    assert!(result.is_err(), "文件不存在应报错");
}
