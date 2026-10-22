//! crates/ramaria-importer/src/writer/types.rs - 导入写入层的对外类型
//!
//! 设计特点:
//! - 写入结果只做统计与 UUID 交付，不持有存储句柄
//! - 画像派发用枚举表达双画像与多画像两种归属方式
//! - 成员派发信息只承载群名片与角色，不含消息内容

use std::collections::BTreeMap;

use ramaria_core::types::MemberRole;

// =========================================================
// 写入结果
// =========================================================

/// L0 写入结果，供调用方展示统计并触发后续深度处理。
///
/// 字段约定:
/// - `session_ids`: 已创建的历史 session UUID 列表，供调用方触发 L1 摘要等深度处理。
/// - `messages_dropped`: 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息），
///   调用方应将其作为"非静默降级"提示给用户，而不是仅存在于日志中。
pub struct WriteOutcome {
    /// 成功写入的 session 数
    pub sessions_written: usize,
    /// 成功写入的消息数
    pub messages_written: usize,
    /// 创建的 session UUID 列表（供调用方触发 L1 摘要等深度处理）
    pub session_ids: Vec<uuid::Uuid>,
    /// 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息）
    pub messages_dropped: usize,
}

// =========================================================
// 多画像派发
// =========================================================

/// 多画像成员派发信息（群聊：一个平台发送者对应一个 persona）。
///
/// 字段约定:
/// - `persona_uid`: 该发送者消息归属的 persona 标识。
/// - `group_nickname`: 群名片；导出未提供时为 None。
/// - `role`: 群内角色；导出未提供时为 None。
#[derive(Debug)]
pub struct MemberDispatch {
    /// 该发送者对应的 persona 标识
    pub persona_uid: String,
    /// 群名片（无则 None）
    pub group_nickname: Option<String>,
    /// 群内角色（无则 None）
    pub role: Option<MemberRole>,
}

/// 消息画像归属派发策略。
///
/// 语义:
/// - `Dual`: 双画像（私聊），发送者为导出者 → self persona，其余 → other persona。
/// - `Multi`: 多画像（群聊），按发送者平台 UID 查成员映射；未命中（含空 UID）
///   的消息丢弃并计入 `messages_dropped`。
#[derive(Debug)]
pub enum PersonaDispatch<'a> {
    /// 双画像（私聊）：发送者为导出者 → self persona，其余 → other persona
    Dual {
        /// 导出者的平台内部 UID（用于与消息的 sender_uid 比较）
        self_uid: &'a str,
        /// 导出者本人的画像标识（导入侧过滤跳过时为 None）
        self_persona_uid: Option<&'a str>,
        /// 对话对方的画像标识（导入侧过滤跳过时为 None）
        other_persona_uid: Option<&'a str>,
    },
    /// 多画像（群聊）：按发送者平台 UID 查成员表
    Multi {
        /// 导出者的平台内部 UID（用于判定我方消息）
        self_uid: &'a str,
        /// 平台 UID → 成员派发信息
        members: &'a BTreeMap<String, MemberDispatch>,
        /// 会话归属 persona（调用方保证；None 时整段会话丢弃并计入 messages_dropped）
        owner_uid: Option<&'a str>,
    },
}
