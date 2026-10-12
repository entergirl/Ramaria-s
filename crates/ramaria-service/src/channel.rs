//! crates/ramaria-service/src/channel.rs - Ramaria 社交通道域用例模块
//!
//! 设计特点:
//! - 绑定解析单点：按 `(channel, external_ref)` 解析，不存在时自动建档
//!   （收到即建档，默认不放行；显式启用后才响应）
//! - 入站写入独立口径：平台消息幂等 + 发言人归属 + 附件定位（见 `ingest` 子模块）
//! - 生效人格解析：绑定级覆盖优先，其次通道级默认，再兜底默认人格（纯函数）
//! - 凭据用例经 OS keychain（凭据不落日志：仅记平台名，绝不打印令牌内容）
//! - 校验失败为业务校验错误（中文文案），不静默放行

use ramaria_core::config::ChannelConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::types::{ChannelBinding, ChannelPlatform};

use crate::engine::Engine;

pub(crate) mod ingest;

// =========================================================
// 绑定用例
// =========================================================

/// 列出全部通道绑定（按 channel / external_ref 排序）。
///
/// 返回:
/// - 绑定行列表（无绑定时为空列表）。
pub async fn list_bindings(engine: &Engine) -> RamariaResult<Vec<ChannelBinding>> {
    engine.storage().list_channel_bindings().await
}

/// 按 `(channel, external_ref)` 解析绑定；不存在时自动建档。
///
/// 自动建档默认值:
/// - `enabled = false`（收到即建档，但默认不放行；用户在面板 / CLI
///   显式启用后才响应，被拒消息不入库不回复）；
/// - `allow_proactive = false`（主动投递默认关）；
/// - `persona_uid = None`（未覆盖，生效时回退通道级默认）。
///
/// 参数:
/// - `channel`: 通道标识（两侧空白容忍）。
/// - `external_ref`: 会话外部标识（两侧空白容忍）。
///
/// 返回:
/// - 命中的绑定行（含本次自动建档的行）。
pub async fn resolve_binding(
    engine: &Engine,
    channel: &str,
    external_ref: &str,
) -> RamariaResult<ChannelBinding> {
    let channel = validate_channel(channel)?;
    let external_ref = validate_external_ref(external_ref)?;

    match engine
        .storage()
        .get_channel_binding(channel, external_ref)
        .await?
    {
        Some(binding) => Ok(binding),
        None => {
            let binding = engine
                .storage()
                .upsert_channel_binding(channel, external_ref, None, false, false)
                .await?;
            tracing::info!(
                channel = %binding.channel,
                external_ref = %mask_id(&binding.external_ref),
                "自动创建通道绑定（默认未启用，等待显式放行）"
            );
            Ok(binding)
        }
    }
}

/// 只读查询绑定（不自动建档）；不存在返回 None。
///
/// 说明:
/// - 与 [`resolve_binding`] 的差异：本查询不做自动建档，供投递门禁等
///   "绑定不存在即不投递"的消费方使用（不产生副作用）。
///
/// 参数:
/// - `channel`: 通道标识（两侧空白容忍）。
/// - `external_ref`: 会话外部标识（两侧空白容忍）。
///
/// 返回:
/// - `Some(binding)`: 命中的绑定行；
/// - `None`: 不存在（未建档）。
pub async fn find_binding(
    engine: &Engine,
    channel: &str,
    external_ref: &str,
) -> RamariaResult<Option<ChannelBinding>> {
    let channel = validate_channel(channel)?;
    let external_ref = validate_external_ref(external_ref)?;
    engine
        .storage()
        .get_channel_binding(channel, external_ref)
        .await
}

/// 绑定写用例：按 `(channel, external_ref)` 更新绑定（不存在则先自动建档）。
///
/// 语义:
/// - `persona_uid`: `Some(v)` 校验（trim 后非空且人格存在）后设置；`None` = 保持现值。
/// - `enabled` / `allow_proactive`: `None` = 保持现值。
///
/// 返回:
/// - 更新后的完整绑定行。
pub async fn set_binding(
    engine: &Engine,
    channel: &str,
    external_ref: &str,
    persona_uid: Option<&str>,
    enabled: Option<bool>,
    allow_proactive: Option<bool>,
) -> RamariaResult<ChannelBinding> {
    let current = resolve_binding(engine, channel, external_ref).await?;

    let persona_uid = match persona_uid {
        Some(raw) => {
            let uid = raw.trim();
            if uid.is_empty() {
                return Err(RamariaError::validation("绑定人格不能为空"));
            }
            if engine.storage().get_persona_by_uid(uid).await?.is_none() {
                return Err(RamariaError::validation(format!("绑定人格不存在: {uid}")));
            }
            Some(uid.to_string())
        }
        None => current.persona_uid.clone(),
    };
    let enabled = enabled.unwrap_or(current.enabled);
    let allow_proactive = allow_proactive.unwrap_or(current.allow_proactive);

    let binding = engine
        .storage()
        .upsert_channel_binding(
            &current.channel,
            &current.external_ref,
            persona_uid.as_deref(),
            enabled,
            allow_proactive,
        )
        .await?;
    tracing::info!(
        channel = %binding.channel,
        external_ref = %mask_id(&binding.external_ref),
        enabled = binding.enabled,
        allow_proactive = binding.allow_proactive,
        "通道绑定已更新"
    );
    Ok(binding)
}

/// 删除通道绑定（幂等：不存在视同成功）。
pub async fn delete_binding(
    engine: &Engine,
    channel: &str,
    external_ref: &str,
) -> RamariaResult<()> {
    let channel = validate_channel(channel)?;
    let external_ref = validate_external_ref(external_ref)?;
    engine
        .storage()
        .delete_channel_binding(channel, external_ref)
        .await?;
    tracing::info!(
        channel,
        external_ref = %mask_id(external_ref),
        "通道绑定已删除"
    );
    Ok(())
}

/// 绑定生效人格：绑定级 `persona_uid` 优先；否则通道级默认；再空则默认人格。
///
/// 说明:
/// - 三侧均 trim 归一；返回值为 trim 后的非空字符串（纯函数，不访问存储）。
pub fn effective_persona_uid(config: &ChannelConfig, binding: &ChannelBinding) -> String {
    let bound = binding
        .persona_uid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(uid) = bound {
        return uid.to_string();
    }
    let default_uid = config.default_persona_uid.trim();
    if default_uid.is_empty() {
        crate::types::DEFAULT_PERSONA_UID.to_string()
    } else {
        default_uid.to_string()
    }
}

// =========================================================
// 校验辅助
// =========================================================

/// 校验通道标识（trim 后须非空）；返回 trim 后的值。
fn validate_channel(channel: &str) -> RamariaResult<&str> {
    let trimmed = channel.trim();
    if trimmed.is_empty() {
        return Err(RamariaError::validation("通道标识不能为空"));
    }
    Ok(trimmed)
}

/// 校验会话外部标识（trim 后须非空）；返回 trim 后的值。
fn validate_external_ref(external_ref: &str) -> RamariaResult<&str> {
    let trimmed = external_ref.trim();
    if trimmed.is_empty() {
        return Err(RamariaError::validation("会话外部标识不能为空"));
    }
    Ok(trimmed)
}

// =========================================================
// 凭据用例（OS keychain）
// =========================================================

/// 平台凭据的 keychain 服务名（qq / telegram）。
pub(crate) fn credential_service(platform: ChannelPlatform) -> &'static str {
    platform.as_str()
}

/// 写入 keychain 凭据（底层接口；调用方负责令牌校验与日志口径）。
pub(crate) fn write_credential(engine: &Engine, service: &str, token: &str) -> RamariaResult<()> {
    engine.keychain().set_api_key(service, token)
}

/// 读取 keychain 凭据（None = 未配置）。
pub(crate) fn read_credential(engine: &Engine, service: &str) -> RamariaResult<Option<String>> {
    engine.keychain().get_api_key(service)
}

/// 删除 keychain 凭据（幂等：不存在视同成功）。
///
/// 说明:
/// - 凭据管理底层接口；当前由用例测试消费，生产清理入口接入前暂标 `allow(dead_code)`。
#[allow(dead_code)]
pub(crate) fn delete_credential(engine: &Engine, service: &str) -> RamariaResult<()> {
    engine.keychain().delete_api_key(service)
}

/// 保存平台凭据（令牌写入 keychain）。
///
/// 参数:
/// - `platform`: 目标平台（凭据键 = 平台名）。
/// - `token`: 令牌明文（两侧空白容忍；空白为校验错误）。
///
/// 返回:
/// - 写入失败为隐私域错误（keychain 不可用 / 权限不足）。
pub fn save_credential(
    engine: &Engine,
    platform: ChannelPlatform,
    token: &str,
) -> RamariaResult<()> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(RamariaError::validation("平台凭据不能为空"));
    }
    let service = credential_service(platform);
    write_credential(engine, service, trimmed)?;
    // 日志仅记平台名，绝不打印令牌内容或片段
    tracing::info!(platform = %platform.as_str(), "社交通道凭据已保存");
    Ok(())
}

/// 查询平台凭据是否已配置。
pub fn credential_configured(engine: &Engine, platform: ChannelPlatform) -> RamariaResult<bool> {
    Ok(load_credential(engine, platform)?.is_some())
}

/// 读取平台凭据（None = 未配置）。
pub fn load_credential(
    engine: &Engine,
    platform: ChannelPlatform,
) -> RamariaResult<Option<String>> {
    read_credential(engine, credential_service(platform))
}

#[cfg(test)]
mod tests;
