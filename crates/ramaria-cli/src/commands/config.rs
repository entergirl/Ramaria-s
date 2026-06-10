//! rust/crates/ramaria-cli/src/commands/config.rs - 配置管理命令
//!
//! 设计特点:
//! - list: 显示当前完整配置（API key 遮蔽为 "***"）
//! - get:  获取单个配置项
//! - set:  设置单个配置项（provider/base_url/temperature/max_tokens）
//! - 写操作自动保存到存储层
//! - 敏感信息（API key）只通过 keychain 操作，config 命令不直接读写

use anyhow::Context;
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

/// 执行 config 命令。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: ConfigCmd) -> anyhow::Result<()> {
    match cmd {
        ConfigCmd::List => list_config(app).await,
        ConfigCmd::Get { key } => get_config(app, &key).await,
        ConfigCmd::Set { key, value } => set_config(app, &key, &value).await,
    }
}

/// 列出所有配置。
async fn list_config(app: &Arc<ramaria_app::App>) -> anyhow::Result<()> {
    let cfg = app.backend_config();

    // 读取 API key（遮蔽）
    let api_key_status = match app.keychain().get_api_key(cfg.provider.as_str()) {
        Ok(Some(key)) => format!("已设置 ({})", crate::ui::mask_key(&key)),
        Ok(None) => "(未设置)".to_string(),
        Err(_) => "(读取失败)".to_string(),
    };

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
async fn get_config(app: &Arc<ramaria_app::App>, key: &str) -> anyhow::Result<()> {
    let cfg = app.backend_config();

    match key {
        "provider" => println!("{}", cfg.provider.as_str()),
        "base_url" => println!("{}", cfg.base_url),
        "model_id" => println!("{}", cfg.capability.model_id),
        "temperature" => println!("{:.2}", cfg.temperature),
        "max_tokens" => println!("{}", cfg.max_tokens),
        "api_key" => {
            // API key 从 keychain 读取
            match app.keychain().get_api_key(cfg.provider.as_str()) {
                Ok(Some(key)) => println!("{}", crate::ui::mask_key(&key)),
                Ok(None) => println!("(未设置)"),
                Err(e) => anyhow::bail!("读取 API key 失败: {e}"),
            }
        }
        "state" => println!("{}", app.current_state().as_str()),
        _ => {
            // 尝试从 settings 表读取自定义设置
            match app.storage().get_setting(key).await? {
                Some(value) => println!("{value}"),
                None => anyhow::bail!(
                    "未知配置项: '{key}'。支持: provider / base_url / model_id / temperature / max_tokens / api_key / state"
                ),
            }
        }
    }

    Ok(())
}

/// 设置配置项。
async fn set_config(app: &Arc<ramaria_app::App>, key: &str, value: &str) -> anyhow::Result<()> {
    let mut cfg = app.backend_config().clone();

    match key {
        "provider" => {
            let provider = match value {
                "lm_studio" | "lmstudio" => ramaria_core::types::LlmProvider::LmStudio,
                "deepseek" => ramaria_core::types::LlmProvider::DeepSeek,
                "openai" => ramaria_core::types::LlmProvider::OpenAI,
                _ => anyhow::bail!(
                    "不支持的 provider: '{value}'。支持: lm_studio / deepseek / openai"
                ),
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
            return Ok(());
        }
        _ => {
            // 自定义设置项写入 settings 表
            app.storage()
                .set_setting(key, value)
                .await
                .context("保存设置失败")?;
            crate::ui::success(&format!("设置 {key} 已更新"));
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

    Ok(())
}
