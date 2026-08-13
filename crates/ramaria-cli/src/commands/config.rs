//! rust/crates/ramaria-cli/src/commands/config.rs - 配置管理命令
//!
//! 设计特点:
//! - list: 显示当前完整配置（API key 遮蔽为 "***"）
//! - get: 获取单个配置项
//! - set: 设置单个配置项（provider/base_url/temperature/max_tokens）
//! - 写操作自动保存到存储层
//! - 敏感信息（API key）只通过 keychain 操作，config 命令不直接读写

use anyhow::Context;
use ramaria_core::error::RamariaError;
use std::sync::Arc;

/// config 命令的子命令。
pub enum ConfigCmd {
    /// 列出所有配置
    List,
    /// 获取单个配置项
    Get { key: String },
    /// 设置配置项
    Set { key: String, value: String },
}

/// 支持点分路径的配置组（走 ConfigSyncService 双写，含 config.toml 与 DB）。
const SECTIONED_KEYS: &[&str] = &[
    "utt.enabled",
    "utt.theta_gap_minutes",
    "utt.max_msgs_per_block",
    "utt.retrieve_top_k",
    "utt.max_block_chars",
    "bridge.enabled",
    "bridge.max_chars",
];

/// 构造 ConfigSyncService（config.toml 位于 config_dir 下）。
fn config_sync(app: &Arc<ramaria_app::App>) -> ramaria_app::ConfigSyncService {
    let config_path = std::path::PathBuf::from(&app.config().paths.config_dir).join("config.toml");
    ramaria_app::ConfigSyncService::new(app.storage().clone(), config_path)
}

/// 从生效配置读取点分键的当前值。
fn get_sectioned(cfg: &ramaria_core::config::RamariaConfig, key: &str) -> Option<String> {
    match key {
        "utt.enabled" => Some(cfg.utt.enabled.to_string()),
        "utt.theta_gap_minutes" => Some(cfg.utt.theta_gap_minutes.to_string()),
        "utt.max_msgs_per_block" => Some(cfg.utt.max_msgs_per_block.to_string()),
        "utt.retrieve_top_k" => Some(cfg.utt.retrieve_top_k.to_string()),
        "utt.max_block_chars" => Some(cfg.utt.max_block_chars.to_string()),
        "bridge.enabled" => Some(cfg.bridge.enabled.to_string()),
        "bridge.max_chars" => Some(cfg.bridge.max_chars.to_string()),
        _ => None,
    }
}

/// 将点分键的值写入配置。
fn set_sectioned(
    cfg: &mut ramaria_core::config::RamariaConfig,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match key {
        "utt.enabled" => {
            cfg.utt.enabled = parse_bool(key, value)?;
        }
        "utt.theta_gap_minutes" => {
            cfg.utt.theta_gap_minutes = parse_u32(key, value)?;
        }
        "utt.max_msgs_per_block" => {
            cfg.utt.max_msgs_per_block = parse_u32(key, value)?;
        }
        "utt.retrieve_top_k" => {
            cfg.utt.retrieve_top_k = parse_u32(key, value)?;
        }
        "utt.max_block_chars" => {
            cfg.utt.max_block_chars = parse_u32(key, value)?;
        }
        "bridge.enabled" => {
            cfg.bridge.enabled = parse_bool(key, value)?;
        }
        "bridge.max_chars" => {
            cfg.bridge.max_chars = parse_u32(key, value)?;
        }
        _ => anyhow::bail!("未知配置项: '{key}'"),
    }
    Ok(())
}

fn parse_bool(key: &str, value: &str) -> anyhow::Result<bool> {
    match value {
        "true" | "false" => Ok(value == "true"),
        _ => Err(anyhow::anyhow!(RamariaError::validation(format!(
            "{key} 必须是布尔值（true / false）"
        )))),
    }
}

fn parse_u32(key: &str, value: &str) -> anyhow::Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!(RamariaError::validation(format!("{key} 必须是正整数"))))
}

/// 执行 config 命令。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: ConfigCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        ConfigCmd::List => list_config(app, json).await,
        ConfigCmd::Get { key } => get_config(app, &key, json).await,
        ConfigCmd::Set { key, value } => set_config(app, &key, &value, json).await,
    }
}

/// 列出所有配置。
async fn list_config(app: &Arc<ramaria_app::App>, json: bool) -> anyhow::Result<()> {
    let cfg = app.backend_config();

    // 读取 API key（遮蔽）
    let api_key_status = match app.keychain().get_api_key(cfg.provider.as_str()) {
        Ok(Some(key)) => format!("已设置 ({})", crate::ui::mask_key(&key)),
        Ok(None) => "(未设置)".to_string(),
        Err(_) => "(读取失败)".to_string(),
    };

    if json {
        let settings = app
            .storage()
            .list_settings()
            .await
            .context("读取设置失败")?;
        let settings_map: serde_json::Map<String, serde_json::Value> = settings
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        let data = serde_json::json!({
            "provider": cfg.provider.as_str(),
            "base_url": cfg.base_url,
            "api_key": api_key_status,
            "model_id": cfg.capability.model_id,
            "temperature": cfg.temperature,
            "max_tokens": cfg.max_tokens,
            "streaming": cfg.capability.supports_streaming,
            "json_mode": cfg.capability.supports_json_mode,
            "context_window": cfg.capability.context_window,
            "state": app.current_state().as_str(),
            "settings": settings_map,
        });
        return crate::json::emit_ok(&data);
    }

    println!();
    crate::ui::separator();
    println!("  Ramaria 配置");
    crate::ui::separator();
    println!();
    crate::ui::labeled("Provider", cfg.provider.as_str());
    crate::ui::labeled("Base URL", &cfg.base_url);
    crate::ui::labeled("API Key", &api_key_status);
    crate::ui::labeled("Model ID", &cfg.capability.model_id);
    crate::ui::labeled("Temperature", &format!("{:.2}", cfg.temperature));
    crate::ui::labeled("Max Tokens", &cfg.max_tokens.to_string());
    crate::ui::labeled(
        "Streaming",
        &format!("{}", cfg.capability.supports_streaming),
    );
    crate::ui::labeled(
        "JSON Mode",
        &format!("{}", cfg.capability.supports_json_mode),
    );
    crate::ui::labeled("Context Window", &cfg.capability.context_window.to_string());
    println!();

    // 显示应用状态
    let state = app.current_state();
    crate::ui::labeled("应用状态", state.as_str());
    println!();

    // 显示 settings 表内容
    let settings = app
        .storage()
        .list_settings()
        .await
        .context("读取设置失败")?;
    if !settings.is_empty() {
        crate::ui::separator();
        println!("  自定义设置");
        crate::ui::separator();
        for (key, value) in &settings {
            crate::ui::labeled(key, value);
        }
        println!();
    }

    Ok(())
}

/// 获取单个配置项。
async fn get_config(app: &Arc<ramaria_app::App>, key: &str, json: bool) -> anyhow::Result<()> {
    let cfg = app.backend_config();

    // 统一取值函数：返回 String，未知 key 时由调用方决定错误信息。
    let value: Result<String, anyhow::Error> = match key {
        "provider" => Ok(cfg.provider.as_str().to_string()),
        "base_url" => Ok(cfg.base_url.clone()),
        "model_id" => Ok(cfg.capability.model_id.clone()),
        "temperature" => Ok(format!("{:.2}", cfg.temperature)),
        "max_tokens" => Ok(cfg.max_tokens.to_string()),
        "api_key" => {
            // API key 从 keychain 读取
            match app.keychain().get_api_key(cfg.provider.as_str()) {
                Ok(Some(key)) => Ok(crate::ui::mask_key(&key)),
                Ok(None) => Ok("(未设置)".to_string()),
                Err(e) => anyhow::bail!("读取 API key 失败: {e}"),
            }
        }
        "state" => Ok(app.current_state().as_str().to_string()),
        _ => {
            // 点分路径配置组（utt.* / bridge.*）：读生效配置（文件 + DB 合并）
            if SECTIONED_KEYS.contains(&key) {
                let full = config_sync(app)
                    .load_config_only()
                    .await
                    .context("读取配置失败")?;
                match get_sectioned(&full, key) {
                    Some(value) => Ok(value),
                    None => Err(anyhow::anyhow!(RamariaError::validation(format!(
                        "未知配置项: '{key}'"
                    )))),
                }
            } else {
                // 尝试从 settings 表读取自定义设置
                match app.storage().get_setting(key).await? {
                    Some(value) => Ok(value),
                    None => Err(anyhow::anyhow!(RamariaError::validation(format!(
                        "未知配置项: '{key}'。支持: provider / base_url / model_id / temperature / max_tokens / api_key / state / utt.* / bridge.*"
                    )))),
                }
            }
        }
    };
    let value = value?;

    if json {
        let data = serde_json::json!({"key": key, "value": value});
        return crate::json::emit_ok(&data);
    }
    println!("{value}");
    Ok(())
}

/// 设置配置项。
async fn set_config(
    app: &Arc<ramaria_app::App>,
    key: &str,
    value: &str,
    json: bool,
) -> anyhow::Result<()> {
    let mut cfg = app.backend_config().clone();

    match key {
        "provider" => {
            let provider = match value {
                "lm_studio" | "lmstudio" => ramaria_core::types::LlmProvider::LmStudio,
                "deepseek" => ramaria_core::types::LlmProvider::DeepSeek,
                "openai" => ramaria_core::types::LlmProvider::OpenAI,
                _ => {
                    return Err(anyhow::anyhow!(RamariaError::validation(format!(
                        "不支持的 provider: '{value}'。支持: lm_studio / deepseek / openai"
                    ))));
                }
            };
            cfg.provider = provider;
        }
        "base_url" => {
            cfg.base_url = value.to_string();
        }
        "temperature" => {
            cfg.temperature = value
                .parse::<f64>()
                .context("temperature 必须是浮点数（如 0.7）")?;
        }
        "max_tokens" => {
            cfg.max_tokens = value
                .parse::<u32>()
                .context("max_tokens 必须是正整数（如 1024）")?;
        }
        "api_key" => {
            // API key 写入 keychain，不写入 config
            let provider = cfg.provider;
            if !provider.is_online() {
                crate::ui::warn("本地 provider 无需 API key");
                return Ok(());
            }
            app.keychain()
                .set_api_key(provider.as_str(), value)
                .context("保存 API key 失败")?;
            crate::ui::success(&format!("{} API key 已更新", provider.as_str()));
            if json {
                crate::json::emit_ok(&serde_json::json!({"key": key, "value": "已更新"}))?;
            }
            return Ok(());
        }
        _ => {
            // 点分路径配置组（utt.* / bridge.*）：读生效配置 → 修改 → 双写
            // （config.toml + DB settings，与桌面设置页同一通道，D-V14-006）。
            if SECTIONED_KEYS.contains(&key) {
                let sync = config_sync(app);
                let mut full = sync.load_config_only().await.context("读取配置失败")?;
                set_sectioned(&mut full, key, value)?;
                let result = sync.save_config(&full).await;
                if !result.file_ok {
                    crate::ui::warn("config.toml 写入失败（DB 侧仍生效，下次启动校验提示）");
                }
                if !result.db_ok {
                    crate::ui::warn("DB 侧配置写入失败（文件侧仍生效）");
                }
                if result.file_ok || result.db_ok {
                    crate::ui::success(&format!("{key} 已更新为 {value}"));
                    crate::ui::info("utt/bridge 参数在会话封存/桥接时生效；桌面端需重启应用");
                    if json {
                        crate::json::emit_ok(&serde_json::json!({"key": key, "value": value}))?;
                    }
                } else {
                    anyhow::bail!("{key} 更新失败：文件与 DB 两侧均写入失败");
                }
                return Ok(());
            }
            // 自定义设置项写入 settings 表
            app.storage()
                .set_setting(key, value)
                .await
                .context("保存设置失败")?;
            crate::ui::success(&format!("设置 {key} 已更新"));
            if json {
                crate::json::emit_ok(&serde_json::json!({"key": key, "value": value}))?;
            }
            return Ok(());
        }
    }

    // 保存后端配置
    app.storage()
        .save_backend_config(&cfg)
        .await
        .context("保存配置失败")?;

    // 刷新状态
    app.refresh_setup_state().await?;

    crate::ui::success(&format!("{key} 已更新为 {value}"));
    crate::ui::info(&format!("当前状态: {}", app.current_state().as_str()));

    if json {
        crate::json::emit_ok(&serde_json::json!({"key": key, "value": value}))?;
    }

    Ok(())
}
