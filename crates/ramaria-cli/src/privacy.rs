//! rust/crates/ramaria-cli/src/privacy.rs - 线上隐私确认流程
//!
//! 设计特点:
//! - 调用 ramaria-app 的隐私检查 API（复用隐私确认逻辑）
//! - 线上 provider 首次使用必须显式确认
//! - `--yes` 参数允许跳过交互确认（需显式指定 provider）
//! - 本地 LM Studio 无需隐私确认
//! - 记录 tracing 日志用于审计

use ramaria_core::error::RamariaResult;

/// 确保线上 provider 的隐私已确认。
///
/// 参数:
/// - `app`: App 实例引用。
/// - `auto_yes`: 是否使用 `--yes` 跳过交互确认。
///
/// 返回:
/// - `Ok(())`: 确认完成或非线上 provider。
/// - `Err`: 用户拒绝确认或确认操作失败。
///
/// 说明:
/// - 本地 LM Studio 直接通过（不触发确认流程）。
/// - 线上 provider（DeepSeek/OpenAI）需要用户交互确认。
/// - `--yes` 只在显式指定了线上 provider 时生效（T-CLI-010 规则）。
pub async fn ensure_privacy(app: &ramaria_app::App, auto_yes: bool) -> RamariaResult<()> {
    // 委托给 ramaria-app 的隐私检查
    let status = app.check_privacy().await?;

    match status {
        ramaria_app::privacy::PrivacyStatus::NotNeeded => {
            // 本地 provider，无需确认
            tracing::debug!("本地 provider，无需隐私确认");
            Ok(())
        }
        ramaria_app::privacy::PrivacyStatus::Confirmed { .. } => {
            tracing::info!("隐私已确认，继续");
            Ok(())
        }
        ramaria_app::privacy::PrivacyStatus::NeedsConfirmation {
            provider_name,
            base_url,
        } => {
            if auto_yes {
                // --yes 模式：自动持久确认（T-CLI-010 规则）
                tracing::warn!(
                    provider = %provider_name,
                    base_url = %base_url,
                    "--yes 自动确认隐私提醒"
                );
                eprintln!("\x1b[33m⚠ 隐私提醒: 消息将发送至 {provider_name} ({base_url})\x1b[0m");
                eprintln!("  使用 --yes 已自动确认。数据将离开本机。");
                app.confirm_privacy(true).await?;
                return Ok(());
            }

            // 交互确认
            eprintln!();
            eprintln!("\x1b[33m══════════════════════════════════════════\x1b[0m");
            eprintln!("\x1b[33m  隐私提醒\x1b[0m");
            eprintln!("\x1b[33m══════════════════════════════════════════\x1b[0m");
            eprintln!();
            eprintln!("  你正在使用线上 AI 服务：");
            eprintln!("    服务商: {provider_name}");
            eprintln!("    地址  : {base_url}");
            eprintln!();
            eprintln!("  你的对话内容将发送至该服务商的服务器。");
            eprintln!("  请确认你已阅读并同意该服务商的隐私政策。");
            eprintln!();

            let confirmed = crate::ui::confirm("是否同意将数据发送至线上服务？")?;

            if !confirmed {
                tracing::warn!(provider = %provider_name, "用户拒绝隐私确认");
                return Err(ramaria_core::error::RamariaError::validation(
                    "用户拒绝隐私确认。无法使用线上 AI 服务。请切换为本地 LM Studio 或重新确认。",
                ));
            }

            // 确认是否持久化（下次不再询问）
            let persistent = crate::ui::confirm("是否记住此选择（下次不再询问）？")?;
            app.confirm_privacy(persistent).await?;

            crate::ui::success("隐私确认完成");
            Ok(())
        }
        _ => {
            // PrivacyStatus 为 #[non_exhaustive]，保守拒绝未知状态
            Err(ramaria_core::error::RamariaError::validation(
                "未知的隐私状态。请检查应用配置后重试。",
            ))
        }
    }
}
