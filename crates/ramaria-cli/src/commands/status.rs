//! crates/ramaria-cli/src/commands/status.rs - 应用状态探活命令
//!
//! 设计特点:
//! - agent 探活入口：应用状态 / 配置摘要 / DB 路径 / 版本
//! - --json 输出遵循 M1 信封（stdout 仅含 JSON），非 TTY 可执行
//! - 文本输出保持既有风格（labeled 对齐）
//! - 不访问 LLM、不触发网络，仅读取已初始化的 App 状态

use std::path::PathBuf;
use std::sync::Arc;

/// status 命令参数。
pub struct StatusArgs {
    /// 数据库文件路径（来自全局 --db）
    pub db_path: PathBuf,
    /// JSON 信封输出
    pub json: bool,
}

/// 执行 status 命令。
pub async fn run(app: &Arc<ramaria_app::App>, args: StatusArgs) -> anyhow::Result<()> {
    let cfg = app.backend_config();
    let state = app.current_state();
    let version = env!("CARGO_PKG_VERSION");

    if args.json {
        let data = serde_json::json!({
            "state": state.as_str(),
            "provider": cfg.provider.as_str(),
            "base_url": cfg.base_url,
            "model_id": cfg.capability.model_id,
            "temperature": cfg.temperature,
            "max_tokens": cfg.max_tokens,
            "db_path": args.db_path.display().to_string(),
            "version": version,
        });
        return crate::json::emit_ok(&data);
    }

    println!();
    crate::ui::separator();
    println!("  Ramaria 状态");
    crate::ui::separator();
    println!();
    crate::ui::labeled("应用状态", state.as_str());
    crate::ui::labeled("Provider", cfg.provider.as_str());
    crate::ui::labeled("Base URL", &cfg.base_url);
    crate::ui::labeled("Model ID", &cfg.capability.model_id);
    crate::ui::labeled("Temperature", &format!("{:.2}", cfg.temperature));
    crate::ui::labeled("Max Tokens", &cfg.max_tokens.to_string());
    crate::ui::labeled("数据库", &args.db_path.display().to_string());
    crate::ui::labeled("版本", version);
    Ok(())
}
