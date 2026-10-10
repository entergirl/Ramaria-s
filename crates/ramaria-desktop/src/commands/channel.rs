//! crates/ramaria-desktop/src/commands/channel.rs - 社交通道 Tauri Commands
//!
//! 设计特点:
//! - 启停 + 查询两组命令：运行时装配与互斥在 `channels` 模块，本层只做参数与视图映射
//! - 启停失败降级为视图（running 状态如实回显），不上抛可恢复错误
//! - 凭据只回显"是否已配置"（令牌明文永不进命令返回与日志）
//! - 绑定读写委托服务层用例；生效人格由服务层单点计算，桌面不重算

use crate::DesktopState;
use ramaria_core::types::{ChannelBinding, ChannelPlatform};
use ramaria_service::Engine;
use serde::Serialize;
use tauri::State;

use crate::channels::ChannelStatusView;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 通道面板视图（设置页「社交接入」面板展示用）。
///
/// 序列化约定：camelCase 对齐前端 JS 访问（`panel.qqEnabled` / `panel.running`）。
///
/// 字段约定:
/// - `enabled` / `qq_enabled` / `telegram_enabled`: `[channel]` 生效开关；
/// - `qq_ws_url` / `default_persona_uid` / `proactive_enabled`: 面板回显配置值；
/// - `*_token_configured`: 平台凭据是否已保存（读取失败按未配置）；
/// - `running` / `channels`: 运行时运行态与各通道状态列表（未运行为空）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPanelView {
    pub enabled: bool,
    pub qq_enabled: bool,
    pub telegram_enabled: bool,
    pub qq_ws_url: String,
    pub default_persona_uid: String,
    pub proactive_enabled: bool,
    pub qq_token_configured: bool,
    pub telegram_token_configured: bool,
    pub running: bool,
    pub channels: Vec<ChannelStatusView>,
}

/// 通道绑定视图（外部会话 ↔ 本机侧人格）。
///
/// 序列化约定：camelCase 对齐前端 JS 访问（`binding.externalRef` / `binding.effectivePersonaUid`）。
///
/// 字段约定:
/// - `persona_uid`: 绑定级覆盖；None = 未覆盖；
/// - `effective_persona_uid`: 生效人格（绑定级 > 通道级默认 > 兜底默认，服务层计算）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBindingView {
    pub channel: String,
    pub external_ref: String,
    pub persona_uid: Option<String>,
    pub effective_persona_uid: String,
    pub enabled: bool,
    pub allow_proactive: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 由服务层绑定行与生效人格构造视图（字段映射的唯一入口，便于单测锁定）。
pub(crate) fn binding_to_view(
    binding: &ChannelBinding,
    effective_persona_uid: String,
) -> ChannelBindingView {
    ChannelBindingView {
        channel: binding.channel.clone(),
        external_ref: binding.external_ref.clone(),
        persona_uid: binding.persona_uid.clone(),
        effective_persona_uid,
        enabled: binding.enabled,
        allow_proactive: binding.allow_proactive,
        created_at: binding.created_at,
        updated_at: binding.updated_at,
    }
}

// =========================================================
// 面板组装
// =========================================================

/// 汇总通道面板视图（配置 + 凭据状态 + 运行时状态）。
///
/// 说明:
/// - 凭据读取失败按未配置处理（面板仍可展示与保存配置）；
/// - 配置读取失败上抛（面板无配置可显，属不可恢复的展示前置）。
async fn build_panel_view(state: &DesktopState) -> Result<ChannelPanelView, String> {
    let config = state
        .engine
        .load_full_config()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "读取配置失败"))?;

    Ok(ChannelPanelView {
        enabled: config.channel.enabled,
        qq_enabled: config.channel.qq_enabled,
        telegram_enabled: config.channel.telegram_enabled,
        qq_ws_url: config.channel.qq_ws_url.clone(),
        default_persona_uid: config.channel.default_persona_uid.clone(),
        proactive_enabled: config.channel.proactive_enabled,
        qq_token_configured: credential_configured_silent(&state.engine, ChannelPlatform::Qq),
        telegram_token_configured: credential_configured_silent(
            &state.engine,
            ChannelPlatform::Telegram,
        ),
        running: state.channel_manager.is_running().await,
        channels: state.channel_manager.statuses().await,
    })
}

/// 查询平台凭据是否已配置（读取失败按未配置降级）。
fn credential_configured_silent(engine: &Engine, platform: ChannelPlatform) -> bool {
    match engine.channel_credential_configured(platform) {
        Ok(configured) => configured,
        Err(error) => {
            tracing::warn!(platform = %platform.as_str(), error = %error, "通道凭据状态读取失败，按未配置处理");
            false
        }
    }
}

// =========================================================
// get_channel_status — 综合视图
// =========================================================

/// 获取通道面板综合视图（配置摘要 + 凭据状态 + 运行时状态）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_channel_status(
    state: State<'_, DesktopState>,
) -> Result<ChannelPanelView, String> {
    build_panel_view(state.inner()).await
}

// =========================================================
// start_channel / stop_channel — 运行时启停
// =========================================================

/// 启动通道运行时（已运行 / 未启用 / 互斥失败均返回当前视图，不视为命令错误）。
#[tauri::command]
#[tracing::instrument(skip(state, app))]
pub async fn start_channel(
    state: State<'_, DesktopState>,
    app: tauri::AppHandle,
) -> Result<ChannelPanelView, String> {
    let outcome = state.channel_manager.start(state.engine.clone(), app).await;
    if outcome.started {
        tracing::info!(channels = outcome.channels.len(), "通道运行时启动请求完成");
    } else {
        tracing::warn!(
            reason = outcome.reason.as_deref().unwrap_or(""),
            "通道运行时未启动"
        );
    }
    build_panel_view(state.inner()).await
}

/// 停止通道运行时（幂等；未运行时直接返回当前视图）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn stop_channel(state: State<'_, DesktopState>) -> Result<ChannelPanelView, String> {
    state.channel_manager.stop().await;
    build_panel_view(state.inner()).await
}

// =========================================================
// list_channel_bindings / set_channel_binding — 绑定读写
// =========================================================

/// 列出全部通道绑定（按 channel / external_ref 排序，服务层口径）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_channel_bindings(
    state: State<'_, DesktopState>,
) -> Result<Vec<ChannelBindingView>, String> {
    let bindings = state
        .engine
        .channel_list_bindings()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询通道绑定失败"))?;

    let views: Vec<ChannelBindingView> = bindings
        .iter()
        .map(|binding| {
            binding_to_view(binding, state.engine.channel_effective_persona_uid(binding))
        })
        .collect();

    tracing::debug!(count = views.len(), "list_channel_bindings 完成");
    Ok(views)
}

/// 写入 / 更新通道绑定（不存在则自动建档；`None` 参数保持现值）。
///
/// 参数:
/// - `channel`: 通道名（qq / telegram）。
/// - `external_ref`: 外部会话标识（如 `c2c:10001` / `group:123456`）。
/// - `persona_uid`: 绑定级人格覆盖；None = 保持现值。
/// - `enabled` / `allow_proactive`: None = 保持现值。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_channel_binding(
    state: State<'_, DesktopState>,
    channel: String,
    external_ref: String,
    persona_uid: Option<String>,
    enabled: Option<bool>,
    allow_proactive: Option<bool>,
) -> Result<ChannelBindingView, String> {
    let binding = state
        .engine
        .channel_set_binding(
            &channel,
            &external_ref,
            persona_uid.as_deref(),
            enabled,
            allow_proactive,
        )
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "保存通道绑定失败"))?;

    tracing::info!(
        channel = %binding.channel,
        enabled = binding.enabled,
        allow_proactive = binding.allow_proactive,
        "通道绑定已保存"
    );
    Ok(binding_to_view(
        &binding,
        state.engine.channel_effective_persona_uid(&binding),
    ))
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_binding() -> ChannelBinding {
        ChannelBinding {
            id: 1,
            channel: "qq".to_string(),
            external_ref: "group:123456".to_string(),
            persona_uid: Some("char-0002".to_string()),
            enabled: true,
            allow_proactive: false,
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_000,
        }
    }

    /// 映射：绑定行字段逐项透传，生效人格取入参（服务层计算，桌面不重算）。
    #[test]
    fn binding_to_view_maps_fields() {
        let binding = sample_binding();
        let view = binding_to_view(&binding, "char-0002".to_string());
        assert_eq!(view.channel, "qq");
        assert_eq!(view.external_ref, "group:123456");
        assert_eq!(view.persona_uid.as_deref(), Some("char-0002"));
        assert_eq!(view.effective_persona_uid, "char-0002");
        assert!(view.enabled);
        assert!(!view.allow_proactive);
        assert_eq!(view.created_at, binding.created_at);
        assert_eq!(view.updated_at, binding.updated_at);
    }

    /// 未覆盖人格：`persona_uid` 保持 None，生效人格取通道级结果。
    #[test]
    fn binding_to_view_keeps_unset_persona() {
        let mut binding = sample_binding();
        binding.persona_uid = None;
        let view = binding_to_view(&binding, "rama-0001".to_string());
        assert!(view.persona_uid.is_none(), "未覆盖时应为 None");
        assert_eq!(view.effective_persona_uid, "rama-0001");
    }

    /// 面板视图序列化：字段为 camelCase（前端 JS 访问口径）。
    #[test]
    fn panel_view_serializes_camel_case() {
        let view = ChannelPanelView {
            enabled: true,
            qq_enabled: true,
            telegram_enabled: false,
            qq_ws_url: "ws://127.0.0.1:3001".to_string(),
            default_persona_uid: "rama-0001".to_string(),
            proactive_enabled: false,
            qq_token_configured: true,
            telegram_token_configured: false,
            running: false,
            channels: Vec::new(),
        };
        let json = serde_json::to_value(&view).expect("序列化应成功");
        assert!(json.get("qqEnabled").is_some(), "应为 camelCase：{json}");
        assert!(json.get("qqTokenConfigured").is_some(), "{json}");
        assert!(json.get("defaultPersonaUid").is_some(), "{json}");
        assert!(json.get("qq_enabled").is_none(), "不应出现蛇形字段：{json}");
    }
}
