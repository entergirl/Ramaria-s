//! crates/ramaria-service/src/settings.rs - 设置键值与元信息读取用例
//!
//! 设计特点:
//! - 设置键值读写直通 `settings` 表：返回键集合与过滤口径保持现状，不做额外过滤
//! - 写入校验与桌面现状一致：空键拒绝（校验错误），已存在键覆盖写
//! - 后端配置读取直通 `backend_config` 表（无记录返回 None，回退口径由调用方决定）
//! - schema 版本读取复用存储层既有口径（键缺失按 1、非法值报错），入口不再直查 `schema_meta`
//! - 掩码工具按字符口径处理多字节密钥（ASCII 输出与桌面现状逐字一致），纯函数零 I/O
//!
//! 安全约束:
//! - 日志只记设置键名，不记设置值（值可能含用户偏好等敏感信息）
//! - 掩码不可逆：仅输出首尾少量字符，中间以 `****` 替代

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::BackendConfig;

use crate::engine::Engine;

// =========================================================
// 设置键值用例
// =========================================================

/// 读取全部设置项（`settings` 表键值对）。
///
/// 返回:
/// - 按存储层顺序返回全部键值对；空库返回空列表（非错误）。
///
/// 说明:
/// - 返回键集合与过滤口径保持存储层现状（调用方按各自契约决定展示范围）。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<Vec<(String, String)>> {
    engine.storage_ref().list_settings().await
}

/// 读取单个设置项。
///
/// 返回:
/// - `Ok(Some(value))`: 键存在；
/// - `Ok(None)`: 键不存在（空态，非错误；调用方按各自契约决定"未知键"提示）。
pub(crate) async fn get(engine: &Engine, key: &str) -> RamariaResult<Option<String>> {
    engine.storage_ref().get_setting(key).await
}

/// 写入单个设置项（已存在键覆盖写）。
///
/// 参数:
/// - `key`: 设置键名（空白视为非法）。
/// - `value`: 设置值（原样写入，不在此层做类型转换）。
///
/// 返回:
/// - 空键返回 `Validation` 错误（文案与桌面现状一致）；
/// - 存储写入失败返回 `Storage` 错误。
pub(crate) async fn set(engine: &Engine, key: &str, value: &str) -> RamariaResult<()> {
    if key.trim().is_empty() {
        return Err(RamariaError::validation("设置键名不能为空"));
    }

    engine.storage_ref().set_setting(key, value).await?;

    tracing::info!(key = %key, "设置已更新");
    Ok(())
}

// =========================================================
// 元信息读取用例
// =========================================================

/// 读取 DB 侧后端配置（`backend_config` 表）。
///
/// 返回:
/// - `Ok(Some(config))`: 已有保存的后端配置；
/// - `Ok(None)`: 无记录（调用方按各自现状决定回退口径，如本地 provider 默认值）。
pub(crate) async fn backend_config(engine: &Engine) -> RamariaResult<Option<BackendConfig>> {
    engine.storage_ref().get_backend_config().await
}

/// 读取数据库 schema 版本（`schema_meta` 表）。
///
/// 返回:
/// - 成功时返回存储层解析后的版本号（键缺失按 1，与存储层既有口径一致）；
/// - 版本值非法时返回 `Storage` 错误（不静默回退）。
pub(crate) async fn schema_version(engine: &Engine) -> RamariaResult<i32> {
    engine.storage_ref().get_schema_version().await
}

// =========================================================
// API key 掩码
// =========================================================

/// 遮蔽 API key（展示用，不可逆）。
///
/// 规则:
/// - 字符数 ≤ 6 → 仅显示首字符 + `****`（如 `"short"` → `"s****"`）；
/// - 字符数 > 6 → 显示前 3 与后 3 个字符，中间 `****`
///   （如 `"sk-abc123def456"` → `"sk-****456"`）；
/// - 空串 → `****`（安全兜底，不 panic）。
///
/// 说明:
/// - 按字符（而非字节）处理，多字节密钥不切 UTF-8；ASCII 密钥输出与既有
///   桌面命令的掩码口径逐字一致。
/// - 短密钥不保留可推断长度信息；返回值只用于界面展示，不得回写存储。
pub fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len <= 6 {
        let head: String = chars.first().map(|c| c.to_string()).unwrap_or_default();
        format!("{head}****")
    } else {
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[len - 3..].iter().collect();
        format!("{head}****{tail}")
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::engine_with_db;
    use ramaria_core::traits::StoreInfrastructure;
    use ramaria_core::types::LlmProvider;

    /// 设置键值：空库为空、缺失键 None、写入往返、覆盖写、空键拒绝。
    #[tokio::test]
    async fn settings_roundtrip_and_blank_key_rejection() {
        let (engine, _storage, dir) = engine_with_db("settings-roundtrip").await;

        // 空库：列表为空；缺失键为 None（不报错）
        assert!(
            engine
                .settings_list()
                .await
                .expect("列表读取应成功")
                .is_empty()
        );
        assert!(
            engine
                .setting_get("profile_mode")
                .await
                .expect("单键读取应成功")
                .is_none()
        );

        // 写入往返
        engine
            .setting_set("profile_mode", "work")
            .await
            .expect("写入应成功");
        assert_eq!(
            engine
                .setting_get("profile_mode")
                .await
                .expect("单键读取应成功")
                .as_deref(),
            Some("work")
        );
        let list = engine.settings_list().await.expect("列表读取应成功");
        assert!(
            list.contains(&("profile_mode".to_string(), "work".to_string())),
            "列表应包含刚写入的键: {list:?}"
        );

        // 覆盖写：同一键更新为新值
        engine
            .setting_set("profile_mode", "rest")
            .await
            .expect("覆盖写应成功");
        assert_eq!(
            engine
                .setting_get("profile_mode")
                .await
                .expect("单键读取应成功")
                .as_deref(),
            Some("rest")
        );

        // 空键（含纯空白）：校验错误，且不写入
        let err = engine
            .setting_set("   ", "x")
            .await
            .expect_err("空键应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.to_string().contains("设置键名不能为空"),
            "错误文案应与桌面现状一致: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 后端配置读取：空库为 None；保存记录后按字段读回。
    #[tokio::test]
    async fn backend_config_reads_db_row() {
        let (engine, storage, dir) = engine_with_db("settings-backend").await;

        assert!(
            engine.backend_config().await.expect("读取应成功").is_none(),
            "空库无后端配置记录"
        );

        storage
            .save_backend_config(&ramaria_core::types::BackendConfig::deepseek_default())
            .await
            .expect("保存后端配置应成功");
        let config = engine
            .backend_config()
            .await
            .expect("读取应成功")
            .expect("应有后端配置记录");
        assert_eq!(config.provider, LlmProvider::DeepSeek);
        assert_eq!(config.base_url, "https://api.deepseek.com/v1");
        assert_eq!(config.capability.model_id, "deepseek-chat");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// schema 版本：migration 后默认 1；非法值按存储层口径报错（不静默回退）。
    #[tokio::test]
    async fn schema_version_follows_storage_semantics() {
        let (engine, _storage, dir) = engine_with_db("settings-schema").await;

        // 新库：migration 写入 '1'
        assert_eq!(
            engine.schema_version().await.expect("读取应成功"),
            1,
            "新库 schema 版本应为 1"
        );

        // 直写非法值 → 存储层报错（结构化错误可见）
        let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
            .await
            .expect("测试库连接应成功");
        sqlx::query("UPDATE schema_meta SET value = 'abc' WHERE key = 'schema_version'")
            .execute(&pool)
            .await
            .expect("写入非法版本值应成功");
        let err = engine.schema_version().await.expect_err("非法版本值应报错");
        assert_eq!(err.category(), "storage");
        pool.close().await;

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 掩码口径：ASCII 与既有桌面策略一致；多字节按字符处理（不 panic）。
    #[test]
    fn mask_api_key_matches_desktop_policy() {
        // 长密钥：前 3 + **** + 后 3
        assert_eq!(mask_api_key("sk-abc123def456"), "sk-****456");
        // 7 字符边界：走长密钥分支
        assert_eq!(mask_api_key("abcdefg"), "abc****efg");
        // 6 字符边界：走短密钥分支（仅首字符）
        assert_eq!(mask_api_key("abcdef"), "a****");
        assert_eq!(mask_api_key("short"), "s****");
        assert_eq!(mask_api_key("a"), "a****");
        // 空串：安全兜底（不 panic）
        assert_eq!(mask_api_key(""), "****");
        // 多字节：按字符计数与截取，不切 UTF-8
        assert_eq!(mask_api_key("密钥一二三四五六七"), "密钥一****五六七");
        assert_eq!(mask_api_key("短密钥abc"), "短****");
    }
}
