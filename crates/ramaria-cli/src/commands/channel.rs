//! crates/ramaria-cli/src/commands/channel.rs - 社交平台接入命令
//!
//! 设计特点:
//! - 子命令遵循动词词表：status（配置摘要）/ bindings（绑定列表）/ bind（绑定写入）
//! - 绑定读写委托服务层通道用例；生效人格由服务层单点计算，本层不重算
//! - 通道运行时由桌面应用托管：本命令只做配置与绑定维护，不启动连接
//! - 全部支持全局 `--json` 信封；stdout 只输出数据
//!
//! 安全约束:
//! - 凭据只回显"是否已配置"（令牌明文不进入任何输出与日志）
//! - 绑定的外部标识属用户数据，仅出现在 stdout 数据行（不写日志）

use anyhow::Context;
use ramaria_core::types::{ChannelBinding, ChannelPlatform};
use ramaria_service::Engine;
use std::sync::Arc;

use crate::json;

// =========================================================
// 公共枚举与入口
// =========================================================

/// Channel 子命令。
#[derive(Debug, Clone)]
pub enum ChannelCmd {
    /// 查看通道配置摘要与凭据配置状态
    Status,
    /// 列出通道绑定（外部会话 ↔ 本机侧人格）
    Bindings,
    /// 写入 / 更新通道绑定（不存在则自动建档）
    Bind {
        /// 通道名（qq / telegram）
        channel: String,
        /// 外部会话标识（如 c2c:10001 / group:123456）
        external_ref: String,
        /// 绑定级人格覆盖（缺省保持现值）
        persona: Option<String>,
        /// 启用绑定
        enable: bool,
        /// 禁用绑定
        disable: bool,
        /// 允许该绑定接收主动消息
        allow_proactive: bool,
        /// 禁止该绑定接收主动消息
        deny_proactive: bool,
    },
}

/// 运行 channel 子命令分发。
///
/// 参数:
/// - `engine`: 服务层引擎引用。
/// - `cmd`: Channel 子命令。
/// - `json`: JSON 信封输出。
pub async fn run(engine: &Arc<Engine>, cmd: ChannelCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        ChannelCmd::Status => run_status(engine, json).await,
        ChannelCmd::Bindings => run_bindings(engine, json).await,
        ChannelCmd::Bind {
            channel,
            external_ref,
            persona,
            enable,
            disable,
            allow_proactive,
            deny_proactive,
        } => {
            run_bind(
                engine,
                &channel,
                &external_ref,
                persona.as_deref(),
                flag_to_option(enable, disable),
                flag_to_option(allow_proactive, deny_proactive),
                json,
            )
            .await
        }
    }
}

// =========================================================
// status
// =========================================================

/// 查看通道配置摘要与凭据配置状态。
async fn run_status(engine: &Arc<Engine>, json: bool) -> anyhow::Result<()> {
    let config = engine.load_full_config().await.context("读取配置失败")?;
    let qq_token_configured = credential_configured(engine, ChannelPlatform::Qq);
    let telegram_token_configured = credential_configured(engine, ChannelPlatform::Telegram);
    let bindings_count = engine
        .channel_list_bindings()
        .await
        .context("查询通道绑定失败")?
        .len();

    if json {
        let data = serde_json::json!({
            "enabled": config.channel.enabled,
            "qq_enabled": config.channel.qq_enabled,
            "telegram_enabled": config.channel.telegram_enabled,
            "qq_ws_url": config.channel.qq_ws_url,
            "default_persona_uid": config.channel.default_persona_uid,
            "proactive_delivery_enabled": config.channel.proactive_delivery_enabled,
            "qq_token_configured": qq_token_configured,
            "telegram_token_configured": telegram_token_configured,
            "bindings_count": bindings_count,
        });
        return json::emit_ok(&data);
    }

    crate::ui::separator();
    crate::ui::labeled("总开关", switch_label(config.channel.enabled));
    crate::ui::labeled("QQ 通道", switch_label(config.channel.qq_enabled));
    crate::ui::labeled("QQ 地址", &config.channel.qq_ws_url);
    crate::ui::labeled(
        "Telegram 通道",
        switch_label(config.channel.telegram_enabled),
    );
    crate::ui::labeled("默认人格", &config.channel.default_persona_uid);
    crate::ui::labeled(
        "主动投递",
        switch_label(config.channel.proactive_delivery_enabled),
    );
    crate::ui::labeled("QQ 凭据", configured_label(qq_token_configured));
    crate::ui::labeled("Telegram 凭据", configured_label(telegram_token_configured));
    crate::ui::labeled("绑定数", &bindings_count.to_string());
    crate::ui::separator();
    crate::ui::info("通道运行时由桌面应用托管（连接状态与启停在桌面查看）");
    Ok(())
}

// =========================================================
// bindings
// =========================================================

/// 列出全部通道绑定。
async fn run_bindings(engine: &Arc<Engine>, json: bool) -> anyhow::Result<()> {
    let rows = load_binding_rows(engine).await?;

    if json {
        return json::emit_ok(&rows);
    }
    if rows.is_empty() {
        crate::ui::info("暂无绑定（收到外部会话消息后自动建档，或用 bind 手动创建）");
        return Ok(());
    }
    for row in &rows {
        println!("{}", format_binding_row(row));
    }
    Ok(())
}

// =========================================================
// bind
// =========================================================

/// 写入 / 更新通道绑定（不存在则自动建档）。
///
/// 参数:
/// - `enabled` / `allow_proactive`: `None` = 保持现值（`--enable` / `--disable` 合成）。
async fn run_bind(
    engine: &Arc<Engine>,
    channel: &str,
    external_ref: &str,
    persona: Option<&str>,
    enabled: Option<bool>,
    allow_proactive: Option<bool>,
    json: bool,
) -> anyhow::Result<()> {
    let binding = engine
        .channel_set_binding(channel, external_ref, persona, enabled, allow_proactive)
        .await?;
    let row = binding_row(engine, &binding).await;

    if json {
        return json::emit_ok(&row);
    }
    crate::ui::success("通道绑定已保存");
    println!("{}", format_binding_row(&row));
    Ok(())
}

/// 布尔开关对合成三态：都没给 = None（保持现值）。
fn flag_to_option(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

// =========================================================
// 行构造与格式化
// =========================================================

/// 读取全部绑定并附加生效人格（JSON 行形态，字段 snake_case）。
async fn load_binding_rows(engine: &Arc<Engine>) -> anyhow::Result<Vec<serde_json::Value>> {
    let bindings = engine
        .channel_list_bindings()
        .await
        .context("查询通道绑定失败")?;
    let mut rows = Vec::with_capacity(bindings.len());
    for binding in &bindings {
        rows.push(binding_row(engine, binding).await);
    }
    Ok(rows)
}

/// 单行绑定 → JSON 值（生效人格取服务层计算结果）。
async fn binding_row(engine: &Engine, binding: &ChannelBinding) -> serde_json::Value {
    let effective = engine.channel_effective_persona_uid(binding).await;
    serde_json::json!({
        "channel": binding.channel,
        "external_ref": binding.external_ref,
        "persona_uid": binding.persona_uid,
        "effective_persona_uid": effective,
        "enabled": binding.enabled,
        "allow_proactive": binding.allow_proactive,
    })
}

/// 单行绑定 → 文本行（外部标识属用户数据，仅走 stdout）。
fn format_binding_row(row: &serde_json::Value) -> String {
    let channel = row["channel"].as_str().unwrap_or_default();
    let external_ref = row["external_ref"].as_str().unwrap_or_default();
    let effective = row["effective_persona_uid"].as_str().unwrap_or_default();
    let persona_note = match row["persona_uid"].as_str() {
        Some(uid) => format!("（绑定覆盖 {uid}）"),
        None => "（通道默认）".to_string(),
    };
    let enabled = if row["enabled"].as_bool().unwrap_or(false) {
        "启用"
    } else {
        "禁用"
    };
    let proactive = if row["allow_proactive"].as_bool().unwrap_or(false) {
        "主动投递开"
    } else {
        "主动投递关"
    };
    format!("  {channel} {external_ref} → 人格 {effective}{persona_note} | {enabled} | {proactive}")
}

/// 查询平台凭据是否已配置（读取失败按未配置降级并提示）。
fn credential_configured(engine: &Engine, platform: ChannelPlatform) -> bool {
    match engine.channel_credential_configured(platform) {
        Ok(configured) => configured,
        Err(error) => {
            crate::ui::warn(&format!(
                "读取 {} 凭据状态失败，按未配置处理: {error}",
                platform.as_str()
            ));
            false
        }
    }
}

/// 开关文本。
fn switch_label(enabled: bool) -> &'static str {
    if enabled { "已启用" } else { "未启用" }
}

/// 凭据文本。
fn configured_label(configured: bool) -> &'static str {
    if configured { "已配置" } else { "未配置" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 开关对合成三态：单给一侧取对应值；都没给保持现值（None）。
    #[test]
    fn flag_pairs_combine_to_three_states() {
        assert_eq!(flag_to_option(true, false), Some(true));
        assert_eq!(flag_to_option(false, true), Some(false));
        assert_eq!(flag_to_option(false, false), None);
    }

    /// 文本行渲染：覆盖 / 通道默认与两个开关口径。
    #[test]
    fn binding_row_renders_text() {
        let row = serde_json::json!({
            "channel": "qq",
            "external_ref": "group:123456",
            "persona_uid": "char-0002",
            "effective_persona_uid": "char-0002",
            "enabled": true,
            "allow_proactive": false,
        });
        let line = format_binding_row(&row);
        assert!(line.contains("qq group:123456"), "{line}");
        assert!(line.contains("char-0002"), "{line}");
        assert!(line.contains("绑定覆盖"), "{line}");
        assert!(line.contains("启用"), "{line}");
        assert!(line.contains("主动投递关"), "{line}");

        let row = serde_json::json!({
            "channel": "telegram",
            "external_ref": "c2c:10001",
            "persona_uid": null,
            "effective_persona_uid": "rama-0001",
            "enabled": false,
            "allow_proactive": true,
        });
        let line = format_binding_row(&row);
        assert!(line.contains("通道默认"), "{line}");
        assert!(line.contains("禁用"), "{line}");
        assert!(line.contains("主动投递开"), "{line}");
    }
}
