//! crates/ramaria-channel/src/config.rs - Ramaria 通道运行期装配参数模块
//!
//! 设计特点:
//! - 由 core `[channel]` 配置 + 宿主取得的凭据组装，通道不直连 keychain
//! - 降级不阻塞：总开关 / 平台开关任一关闭或地址 / 凭据缺失时该平台为 None
//! - token 两侧空白归一为空，避免尾部空格导致鉴权失败
//! - 只描述数据，不负责读取配置或凭据（读取在 core 配置层与宿主）

use ramaria_core::config::ChannelConfig;

// =========================================================
// 常量
// =========================================================

/// 默认本机侧人格兜底标识（配置为空且绑定未覆盖时使用）。
pub const FALLBACK_DEFAULT_PERSONA_UID: &str = "rama-0001";

// =========================================================
// 运行期装配参数
// =========================================================

/// 通道运行期装配参数（宿主启动通道时构造）。
///
/// 职责:
/// - 聚合运行时所需的平台连接参数与开关：默认人格、两平台连接配置、社交主动投递总开关。
/// - 由 `ChannelConfig` 与宿主经 service 读取的凭据组装；通道层不接触 keychain。
///
/// 字段约定:
/// - `default_persona_uid`: 通道级默认人格（绑定未覆盖时生效）；配置为空时回落
///   [`FALLBACK_DEFAULT_PERSONA_UID`]。
/// - `qq` / `telegram`: 平台连接配置；`None` = 未启用或参数缺失（该平台不启动）。
/// - `proactive_delivery_enabled`: 社交主动投递总开关（还须绑定级开关，任一关闭即不投递）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRuntimeConfig {
    pub default_persona_uid: String,
    pub qq: Option<QqRuntimeConfig>,
    pub telegram: Option<TelegramRuntimeConfig>,
    pub proactive_delivery_enabled: bool,
}

/// QQ（OneBot v11 正向 WS）连接配置。
///
/// 字段约定:
/// - `ws_url`: 正向 WebSocket 服务地址（协议端监听，Ramaria 作客户端）。
/// - `access_token`: 可选访问令牌；None = 协议端未开启鉴权。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QqRuntimeConfig {
    pub ws_url: String,
    pub access_token: Option<String>,
}

/// Telegram Bot API 连接配置。
///
/// 字段约定:
/// - `bot_token`: Bot 令牌（仅随运行时内存持有，不落日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramRuntimeConfig {
    pub bot_token: String,
}

impl ChannelRuntimeConfig {
    /// 由 core 配置 + 凭据组装运行期参数。
    ///
    /// 组装规则:
    /// - 总开关关闭时两平台均为 None（无配置读取副作用）。
    /// - QQ：平台开关开启且地址（trim 后）非空才装配；地址为空记 WARN 并跳过；
    ///   令牌空白（trim 后为空）归一为 None。
    /// - Telegram：平台开关开启且令牌（trim 后）非空才装配；令牌缺失记 WARN 并跳过。
    /// - 默认人格：配置值 trim 后为空时回落 [`FALLBACK_DEFAULT_PERSONA_UID`]。
    ///
    /// 参数:
    /// - `core`: core `[channel]` 配置组快照。
    /// - `qq_access_token`: 宿主取得的 QQ 访问令牌；None = 未配置。
    /// - `telegram_bot_token`: 宿主取得的 Telegram 令牌；None = 未配置。
    ///
    /// 返回:
    /// - 组装后的运行期参数（缺失平台为 None，降级不阻塞）。
    pub fn from_config(
        core: &ChannelConfig,
        qq_access_token: Option<String>,
        telegram_bot_token: Option<String>,
    ) -> Self {
        let default_persona_uid = {
            let trimmed = core.default_persona_uid.trim();
            if trimmed.is_empty() {
                FALLBACK_DEFAULT_PERSONA_UID.to_string()
            } else {
                trimmed.to_string()
            }
        };

        let qq = if core.enabled && core.qq_enabled {
            let ws_url = core.qq_ws_url.trim();
            if ws_url.is_empty() {
                tracing::warn!("QQ 通道已启用但 WebSocket 地址为空，跳过装配");
                None
            } else {
                Some(QqRuntimeConfig {
                    ws_url: ws_url.to_string(),
                    access_token: normalize_token(qq_access_token),
                })
            }
        } else {
            None
        };

        let telegram = if core.enabled && core.telegram_enabled {
            match normalize_token(telegram_bot_token) {
                Some(bot_token) => Some(TelegramRuntimeConfig { bot_token }),
                None => {
                    tracing::warn!("Telegram 通道已启用但 Bot 令牌缺失，跳过装配");
                    None
                }
            }
        } else {
            None
        };

        Self {
            default_persona_uid,
            qq,
            telegram,
            proactive_delivery_enabled: core.proactive_delivery_enabled,
        }
    }

    /// 两平台均未装配（宿主可跳过运行时启动）。
    pub fn is_empty(&self) -> bool {
        self.qq.is_none() && self.telegram.is_none()
    }
}

/// 令牌空白归一：trim 后为空视为未配置（None）。
fn normalize_token(token: Option<String>) -> Option<String> {
    token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;
