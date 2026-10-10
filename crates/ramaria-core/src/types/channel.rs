//! crates/ramaria-core/src/types/channel.rs - Ramaria 社交通道绑定类型模块
//!
//! 设计特点:
//! - 通道平台枚举：凭据（keychain 键）与设置项映射共用的平台身份
//! - 通道绑定行：外部会话 ↔ 本机侧人格 + 开关；时间统一 Unix 毫秒
//! - 只描述数据，不负责绑定解析（解析与回退在服务层用例）

use serde::{Deserialize, Serialize};

// =========================================================
// 通道平台
// =========================================================

/// 通道平台（凭据与配置的键）。
///
/// 职责:
/// - 标识社交平台身份：keychain 凭据键（qq / telegram）与配置项映射共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelPlatform {
    Qq,
    Telegram,
}

impl ChannelPlatform {
    /// 返回存储与凭据口径的小写字符串（qq / telegram）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Qq => "qq",
            Self::Telegram => "telegram",
        }
    }

    /// 解析存储口径字符串；非法值返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "qq" => Some(Self::Qq),
            "telegram" => Some(Self::Telegram),
            _ => None,
        }
    }
}

// =========================================================
// 通道绑定
// =========================================================

/// 通道绑定行（外部会话 ↔ 本机侧人格 + 开关）。
///
/// 职责:
/// - 记录外部会话（私聊 `c2c:{uin}` / 群聊 `group:{群号}`）与本机侧人格、
///   入站处理开关与主动投递开关的绑定关系。
///
/// 字段约定:
/// - `channel` / `external_ref`: 外部会话标识；`(channel, external_ref)` 唯一。
/// - `persona_uid`: 本机侧人格；None = 未覆盖 → 生效时回退通道级默认。
/// - `enabled`: 是否启用该绑定的入站处理。
/// - `allow_proactive`: 是否允许对该会话主动投递（还须通道级总开关）。
/// - `created_at` / `updated_at`: Unix 毫秒；更新保持 created_at、刷新 updated_at。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelBinding {
    pub id: i64,
    pub channel: String,
    pub external_ref: String,
    pub persona_uid: Option<String>,
    pub enabled: bool,
    pub allow_proactive: bool,
    pub created_at: i64,
    pub updated_at: i64,
}
