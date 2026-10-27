//! crates/ramaria-service/src/channel/tests.rs - 社交通道域用例单元测试
//!
//! 设计特点:
//! - 以真实 SQLite 引擎覆盖绑定用例：自动建档 / 改绑 / 开关 / 删除 / 生效人格
//! - 覆盖校验分支（通道 / 外部标识 / 人格空白与不存在）
//! - 凭据全链路（写入 → 读取 → 删除）使用随机服务名并自清理，不触碰生产凭据键
//! - 脱敏断言：凭据写入期间日志不含令牌明文（进程级日志捕获）

use std::sync::{Arc, Mutex};

use super::*;
use crate::test_support::{engine_with_db, seed_persona};

// =========================================================
// 绑定用例
// =========================================================

/// 自动建档：新键解析建行且默认值正确；重复解析同键不重复建。
#[tokio::test]
async fn resolve_auto_creates_binding_once() {
    let (engine, _storage, dir) = engine_with_db("channel-resolve-auto").await;

    let binding = resolve_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("首次解析应自动建档");
    assert_eq!(binding.channel, "qq");
    assert_eq!(binding.external_ref, "c2c:10001");
    assert_eq!(binding.persona_uid, None, "自动建档人格未覆盖");
    assert!(!binding.enabled, "自动建档默认不启用（显式放行后才响应）");
    assert!(!binding.allow_proactive, "自动建档主动投递默认关");

    let again = resolve_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("重复解析应命中已有行");
    assert_eq!(again.id, binding.id, "重复解析不应重复建档");

    let all = list_bindings(&engine).await.expect("列出绑定应成功");
    assert_eq!(all.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 白名单：自动建档默认禁用；显式启用放行、显式关闭再次禁用。
#[tokio::test]
async fn auto_created_binding_requires_explicit_enable() {
    let (engine, _storage, dir) = engine_with_db("channel-whitelist").await;

    let created = resolve_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("首次解析应自动建档");
    assert!(!created.enabled, "自动建档默认禁用");

    let allowed = set_binding(&engine, "qq", "c2c:10001", None, Some(true), None)
        .await
        .expect("显式启用应成功");
    assert!(allowed.enabled, "显式启用后应放行");
    assert_eq!(allowed.id, created.id, "放行不应另起绑定行");

    let again = resolve_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("重复解析应命中已有行");
    assert!(again.enabled, "显式放行结果应被后续解析读到");

    let disabled = set_binding(&engine, "qq", "c2c:10001", None, Some(false), None)
        .await
        .expect("显式关闭应成功");
    assert!(!disabled.enabled, "显式关闭后应再次禁用");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 改绑：设置已存在人格生效；不存在人格为校验错误，不写库。
#[tokio::test]
async fn set_binding_validates_persona_exists() {
    let (engine, storage, dir) = engine_with_db("channel-set-persona").await;
    seed_persona(&storage, "char-0001").await;

    let updated = set_binding(&engine, "qq", "group:20002", Some("char-0001"), None, None)
        .await
        .expect("设置存在人格应成功");
    assert_eq!(updated.persona_uid.as_deref(), Some("char-0001"));

    let err = set_binding(&engine, "qq", "group:20002", Some("char-9999"), None, None)
        .await
        .expect_err("不存在人格应报校验错误");
    assert_eq!(err.category(), "validation");
    let current = resolve_binding(&engine, "qq", "group:20002")
        .await
        .expect("解析应成功");
    assert_eq!(
        current.persona_uid.as_deref(),
        Some("char-0001"),
        "失败时不应改写既有绑定"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 开关：单独设置生效、全 None 保持现值。
#[tokio::test]
async fn set_binding_switches_independently() {
    let (engine, _storage, dir) = engine_with_db("channel-set-switches").await;

    // 自动建档默认 enabled=false / allow_proactive=false
    resolve_binding(&engine, "telegram", "c2c:42")
        .await
        .expect("建档应成功");

    let disabled = set_binding(&engine, "telegram", "c2c:42", None, Some(false), None)
        .await
        .expect("单独关闭启用应成功");
    assert!(!disabled.enabled, "enabled=false 应单独生效");
    assert!(!disabled.allow_proactive, "未指定的开关应保持现值");

    let allowed = set_binding(&engine, "telegram", "c2c:42", None, None, Some(true))
        .await
        .expect("单独打开主动投递应成功");
    assert!(!allowed.enabled, "未指定的开关应保持现值");
    assert!(allowed.allow_proactive, "allow_proactive=true 应单独生效");

    let kept = set_binding(&engine, "telegram", "c2c:42", None, None, None)
        .await
        .expect("全 None 应成功");
    assert!(!kept.enabled);
    assert!(kept.allow_proactive);
    assert_eq!(kept.persona_uid, None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 删除：删后列表与查询均无（幂等）。
#[tokio::test]
async fn delete_binding_removes_row() {
    let (engine, _storage, dir) = engine_with_db("channel-delete").await;
    resolve_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("建档应成功");

    delete_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("删除应成功");
    assert!(
        list_bindings(&engine).await.expect("列出应成功").is_empty(),
        "删除后列表应为空"
    );
    assert!(
        engine
            .storage()
            .get_channel_binding("qq", "c2c:10001")
            .await
            .expect("查询应成功")
            .is_none(),
        "删除后按键查询应无"
    );
    delete_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("重复删除应幂等成功");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空白通道 / 外部标识 / 人格为校验错误。
#[tokio::test]
async fn blank_inputs_are_validation_errors() {
    let (engine, _storage, dir) = engine_with_db("channel-validate-blank").await;

    let err = resolve_binding(&engine, "   ", "c2c:1")
        .await
        .expect_err("空白通道应报校验错误");
    assert_eq!(err.category(), "validation");
    let err = resolve_binding(&engine, "qq", "  ")
        .await
        .expect_err("空白外部标识应报校验错误");
    assert_eq!(err.category(), "validation");

    resolve_binding(&engine, "qq", "c2c:1")
        .await
        .expect("建档应成功");
    let err = set_binding(&engine, "qq", "c2c:1", Some("   "), None, None)
        .await
        .expect_err("空白人格应报校验错误");
    assert_eq!(err.category(), "validation");

    let err = delete_binding(&engine, "  ", "c2c:1")
        .await
        .expect_err("空白通道删除应报校验错误");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 解析与写入口的空白容忍：两侧空白按 trim 后落库。
#[tokio::test]
async fn inputs_are_trimmed() {
    let (engine, _storage, dir) = engine_with_db("channel-trim").await;

    let binding = resolve_binding(&engine, "  qq  ", "  c2c:10001  ")
        .await
        .expect("空白容忍解析应成功");
    assert_eq!(binding.channel, "qq");
    assert_eq!(binding.external_ref, "c2c:10001");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 生效人格
// =========================================================

/// engine 门面：绑定级 > 通道级 > 默认对话人格；设置后按绑定值。
#[tokio::test]
async fn effective_persona_uid_via_engine() {
    let (engine, storage, dir) = engine_with_db("channel-effective").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;

    let uncovered = resolve_binding(&engine, "qq", "c2c:10001")
        .await
        .expect("建档应成功");
    assert_eq!(
        engine.channel_effective_persona_uid(&uncovered).await,
        "rama-0001",
        "未覆盖且无设置时应回退内置默认"
    );

    engine
        .setting_set("default_persona_uid", "char-0001")
        .await
        .expect("写入默认人格应成功");
    assert_eq!(
        engine.channel_effective_persona_uid(&uncovered).await,
        "char-0001",
        "通道级默认空白时应采用默认对话人格"
    );

    let covered = set_binding(&engine, "qq", "c2c:10001", Some("char-0002"), None, None)
        .await
        .expect("设置人格应成功");
    assert_eq!(
        engine.channel_effective_persona_uid(&covered).await,
        "char-0002",
        "绑定级人格应优先"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 纯函数：绑定级 → 通道级 → 默认对话人格 → 内置默认的回退与 trim 归一。
#[test]
fn effective_persona_uid_fallback_chain() {
    let binding = |persona: Option<&str>| ChannelBinding {
        id: 1,
        channel: "qq".to_string(),
        external_ref: "c2c:1".to_string(),
        persona_uid: persona.map(str::to_string),
        enabled: true,
        allow_proactive: false,
        created_at: 0,
        updated_at: 0,
    };
    let config = |default: &str| ChannelConfig {
        default_persona_uid: default.to_string(),
        ..ChannelConfig::default()
    };

    // 绑定级覆盖（含 trim）
    assert_eq!(
        effective_persona_uid(
            &config("rama-0001"),
            &binding(Some("  char-0007  ")),
            "char-0009"
        ),
        "char-0007"
    );
    // 未覆盖 → 通道级默认
    assert_eq!(
        effective_persona_uid(&config("char-0002"), &binding(None), "char-0009"),
        "char-0002"
    );
    // 绑定级空白 → 视为未覆盖
    assert_eq!(
        effective_persona_uid(&config("char-0002"), &binding(Some("   ")), "char-0009"),
        "char-0002"
    );
    // 通道级默认空白 → 默认对话人格（含 trim）
    assert_eq!(
        effective_persona_uid(&config("   "), &binding(None), "  char-0009  "),
        "char-0009"
    );
    // 默认对话人格也空白 → 内置默认
    assert_eq!(
        effective_persona_uid(&config(""), &binding(Some("  ")), "   "),
        "rama-0001"
    );
}

// =========================================================
// 凭据用例
// =========================================================

/// 凭据服务名映射（平台名即 keychain 键）。
#[test]
fn credential_service_maps_platform_names() {
    assert_eq!(credential_service(ChannelPlatform::Qq), "qq");
    assert_eq!(credential_service(ChannelPlatform::Telegram), "telegram");
}

/// 空白令牌为校验错误（不触碰 keychain）。
#[tokio::test]
async fn save_credential_rejects_blank_token() {
    let (engine, _storage, dir) = engine_with_db("channel-credential-blank").await;
    for blank in ["", "   "] {
        let err =
            save_credential(&engine, ChannelPlatform::Qq, blank).expect_err("空白令牌应报校验错误");
        assert_eq!(err.category(), "validation");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 清除凭据的 keychain 键 = 平台名（与保存 / 读取 / 状态查询共用同一映射）。
///
/// 说明:
/// - 不直接调用 `clear_credential`：其键固定为 `qq` / `telegram`，会删除本机生产凭据；
/// - 组合行为 = 本用例（键映射）+ 下方 `credential_write_read_delete_roundtrip`（删除幂等）。
#[test]
fn credential_service_maps_to_platform_key() {
    assert_eq!(credential_service(ChannelPlatform::Qq), "qq");
    assert_eq!(credential_service(ChannelPlatform::Telegram), "telegram");
}

/// 凭据全链路：写入 → 读取 → 删除 → 读取无（随机服务名，自清理）。
#[cfg(windows)]
#[tokio::test]
async fn credential_write_read_delete_roundtrip() {
    let (engine, _storage, dir) = engine_with_db("channel-credential-roundtrip").await;
    let service = format!("channel-tests-{}", uuid::Uuid::new_v4());
    let token = "test-token-9f1c7a2b";

    write_credential(&engine, &service, token).expect("写入 keychain 应成功");
    let read_back = read_credential(&engine, &service).expect("读取 keychain 应成功");
    assert_eq!(read_back.as_deref(), Some(token), "读取值应与写入一致");

    delete_credential(&engine, &service).expect("删除 keychain 应成功");
    let after_delete = read_credential(&engine, &service).expect("读取 keychain 应成功");
    assert_eq!(after_delete, None, "删除后应无凭据");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 脱敏：凭据写入期间的日志不含令牌明文。
///
/// 说明:
/// - 用线程本地默认订阅者（`set_default`）捕获当前测试线程的日志到内存缓冲，
///   作用域退出即恢复——不安装进程级订阅者，与既有日志用例互不干扰；
/// - 作用域内先产一条 WARN 探针：全量并行时若已有进程级订阅者，
///   更低的 DEBUG 级 callsite（keychain 操作提示）可能被兴趣缓存屏蔽，
///   探针确认本作用域捕获生效后，负向断言（不含令牌）才有意义。
#[cfg(windows)]
#[tokio::test]
async fn credential_logs_do_not_leak_token() {
    /// 内存缓冲写入器（订阅者输出落到共享 Vec）。
    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufferWriter {
        type Writer = BufferWriter;

        fn make_writer(&'a self) -> Self::Writer {
            BufferWriter(Arc::clone(&self.0))
        }
    }

    impl std::io::Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("日志缓冲锁不应中毒")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let (engine, _storage, dir) = engine_with_db("channel-credential-log").await;
    let service = format!("channel-tests-{}", uuid::Uuid::new_v4());
    let sentinel = "SENTINEL-token-7f3a9c-不应出现在日志中";
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::warn!("通道凭据日志捕获探针");
        write_credential(&engine, &service, sentinel).expect("写入应成功");
        delete_credential(&engine, &service).expect("清理应成功");
    }

    let logs = String::from_utf8(buffer.lock().expect("日志缓冲锁不应中毒").clone())
        .expect("日志应为合法 UTF-8");
    assert!(logs.contains("捕获探针"), "本作用域日志捕获应生效: {logs}");
    assert!(!logs.contains(sentinel), "日志不应包含令牌明文: {logs}");
    assert!(
        !logs.contains("SENTINEL"),
        "日志不应包含令牌任何片段: {logs}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
