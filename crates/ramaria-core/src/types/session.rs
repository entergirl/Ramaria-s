//! crates/ramaria-core/src/types/session.rs - Ramaria 会话数据类型模块
//!
//! 设计特点:
//! - 定义对话会话生命周期与来源通道标识
//! - CHANNEL_LOCAL 标识桌面/CLI 本地会话通道
//! - Session 承载 L0 消息归属并界定 L1 摘要边界
//! - 记录 channel / external_ref 支撑外部入口续写定位
//! - 所有类型支持 serde，时间统一使用 Unix 毫秒

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{new_id, now_ms};

// =========================================================
// 会话与原始消息（TEXT 主键表 — 使用 UUID）
// =========================================================

/// 本地会话通道标识（桌面 / CLI 产生的会话）。
///
/// 说明:
/// - 与 `sessions.channel` 列的默认值一致；存量行升级后即为该通道。
/// - 外部入口通道（如 `mcp`）在入口层各自声明，不使用本常量。
pub const CHANNEL_LOCAL: &str = "local";

/// QQ 导入通道标识（QQ 聊天记录导入与后续 QQ 连接共用）。
pub const CHANNEL_QQ: &str = "qq";

/// 对话会话。
///
/// 职责:
/// - 表示一次连续对话生命周期。
/// - 承载 L0 消息归属关系。
/// - 为 session 结束后的 L1 摘要生成提供边界。
/// - 记录会话来源通道与外部对话标识，支撑来源标注与外部入口的续写定位。
///
/// 状态:
/// - `ended_at = None`: 会话仍在进行中。
/// - `ended_at = Some(...)`: 会话已关闭，可触发 L1 摘要。
/// - `persona_uid = Some(...)`: 创建此 session 时使用的对话人格。
/// - `persona_uid = None`: 存量 session 或未指定人格。
///
/// 通道约定:
/// - `channel`: 会话来源通道（`local` / `mcp` / 后续社交通道等，开放集合）。
/// - `external_ref`: 外部对话标识（客户端 conversation id 或客户端身份名）；
///   本地会话为 None。同一 `(channel, external_ref)` 至多一个活跃会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    /// Session 开始时间（Unix 毫秒）
    pub started_at: i64,
    /// Session 结束时间，None 表示未关闭
    pub ended_at: Option<i64>,
    /// 创建此 session 时绑定的对话人格 UID（可空兼容存量数据）
    pub persona_uid: Option<String>,
    /// 会话来源通道（存量行升级后为 `local`）
    pub channel: String,
    /// 外部对话标识；本地会话为 None
    pub external_ref: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// 创建一个新的活跃 Session（本地通道）。
    ///
    /// 返回:
    /// - 带新 UUID、当前开始时间、未关闭状态、无 persona_uid 的本地 Session。
    pub fn new() -> Self {
        Self {
            id: new_id(),
            started_at: now_ms(),
            ended_at: None,
            persona_uid: None,
            channel: CHANNEL_LOCAL.to_string(),
            external_ref: None,
        }
    }

    /// 创建一个绑定人格的活跃 Session（本地通道）。
    ///
    /// 参数:
    /// - `persona_uid`: 对话人格标识（None 表示 rama 自身）。
    ///
    /// 返回:
    /// - 带新 UUID、当前开始时间、绑定 persona_uid 的本地 Session。
    pub fn with_persona(persona_uid: Option<String>) -> Self {
        Self {
            id: new_id(),
            started_at: now_ms(),
            ended_at: None,
            persona_uid,
            channel: CHANNEL_LOCAL.to_string(),
            external_ref: None,
        }
    }

    /// 创建一个指定来源通道的活跃 Session。
    ///
    /// 用法:
    /// - 外部入口（MCP / 未来社交通道）创建会话时使用，
    ///   使会话在桌面按来源可区分、外部入口可按 `external_ref` 续写。
    ///
    /// 参数:
    /// - `persona_uid`: 对话人格标识。
    /// - `channel`: 来源通道（如 `mcp`）。
    /// - `external_ref`: 外部对话标识（客户端 conversation id / 客户端名）。
    ///
    /// 返回:
    /// - 带新 UUID、当前开始时间、指定通道信息的活跃 Session。
    pub fn new_in_channel(
        persona_uid: Option<String>,
        channel: impl Into<String>,
        external_ref: Option<String>,
    ) -> Self {
        Self {
            id: new_id(),
            started_at: now_ms(),
            ended_at: None,
            persona_uid,
            channel: channel.into(),
            external_ref,
        }
    }

    /// 关闭当前 Session，记录结束时间。
    ///
    /// 说明:
    /// - 如果 Session 已关闭，此方法保持原结束时间不变。
    /// - 幂等设计避免重复关闭导致时间漂移。
    pub fn close(&mut self) {
        if self.ended_at.is_none() {
            self.ended_at = Some(now_ms());
        }
    }

    /// 判断 Session 是否仍在进行中。
    ///
    /// 返回:
    /// - `true`: Session 处于活跃状态（`ended_at` 为 None）。
    /// - `false`: Session 已关闭（`ended_at` 有值）。
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// 会话成员角色（外部平台成员在会话内的身份）。
///
/// 职责:
/// - 表示外部平台（如 QQ 群）成员的会话内角色；
///   未知角色为 `None`（由使用方以 Option 表达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberRole {
    Owner,
    Admin,
    Member,
}

impl MemberRole {
    /// 返回存储口径的小写字符串（owner / admin / member）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    /// 解析存储口径字符串；非法值返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

/// 会话成员行（外部平台发言者在会话内的身份记录）。
///
/// 职责:
/// - 记录某外部平台发言者在某会话内的最近显示名、群名片、角色与首末见时间；
/// - 供成员映射、成员列表展示与跨会话身份归一使用。
///
/// 字段约定:
/// - `platform_ref`: 外部平台 ID；(session_id, platform_ref) 唯一。
/// - `group_nickname` / `role`: 平台未提供时为 None。
/// - `first_seen_at` / `last_seen_at`: 该成员在本会话内的最早 / 最晚消息时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMember {
    pub session_id: Uuid,
    pub platform_ref: String,
    pub name: String,
    pub group_nickname: Option<String>,
    pub role: Option<MemberRole>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

impl SessionMember {
    /// 创建成员行（群名片与角色初始为 None，由写入方按需填充）。
    pub fn new(
        session_id: Uuid,
        platform_ref: impl Into<String>,
        name: impl Into<String>,
        first_seen_at: i64,
        last_seen_at: i64,
    ) -> Self {
        Self {
            session_id,
            platform_ref: platform_ref.into(),
            name: name.into(),
            group_nickname: None,
            role: None,
            first_seen_at,
            last_seen_at,
        }
    }
}
