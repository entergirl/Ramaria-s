//! crates/ramaria-service/src/types/channel.rs - Ramaria 社交通道用例数据结构
//!
//! 设计特点:
//! - 请求 / 结果面向通道运行时（协议壳）：入站写入、回复生成、图片现场理解、
//!   撤回通知四类
//! - 入站消息复用 core 规范模型（`InboundMessage`）；通道层只做协议翻译与组装
//! - 幂等结果显式携带 `written` 标记：重复事件不产生二次写入与响应
//! - 撤回结果三分：已标记（含是否触发回应）/ 未定位忽略 / 重复通知幂等
//! - 理解结果按单张口径（统计语义与批量理解一致）

use ramaria_core::types::InboundMessage;
use uuid::Uuid;

// =========================================================
// 入站写入用例
// =========================================================

/// 入站写入请求（社交通道消息落库）。
///
/// 字段约定:
/// - `channel`: 通道标识（`qq` / `telegram`）。
/// - `external_ref`: 会话外部标识（`c2c:{uin}` / `group:{群号}`）。
/// - `persona_uid`: 本机侧人格（绑定生效人格）。
/// - `self_id`: 本机账号标识（None = 平台未提供）；用于发言归属判定。
/// - `message`: 规范入站消息（正文与附件引用已组装完成）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelIngestRequest {
    pub channel: String,
    pub external_ref: String,
    pub persona_uid: String,
    pub self_id: Option<String>,
    pub message: InboundMessage,
}

/// 入站写入产出的图片附件定位。
///
/// 字段约定:
/// - `attachment_id`: 落库后的附件行 id（理解回填用）。
/// - `source_ref`: 平台引用（下载地址）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelIngestedImage {
    pub attachment_id: i64,
    pub source_ref: String,
}

/// 入站写入结果。
///
/// 字段约定:
/// - `session_id`: 消息落库的目标会话（重复事件时为已存在消息的会话）。
/// - `message_id`: 落库消息 id（重复事件时为已存在消息 id；供回复编排回引）。
/// - `written`: 是否实际写入（false = 平台消息 ID 幂等命中，未产生写入）。
/// - `images`: 本次落库的图片附件定位（仅 `written=true` 时非空）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelIngestOutcome {
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub written: bool,
    pub images: Vec<ChannelIngestedImage>,
}

// =========================================================
// 回复生成用例
// =========================================================

/// 社交通道回复生成请求（入站消息已落库；只写助手回复行）。
///
/// 字段约定:
/// - `session_id`: 目标会话（入站写入返回）。
/// - `persona_uid`: 回复方人格（绑定生效人格；须与会话归属一致）。
/// - `message`: 本轮用户消息（已组装正文，用于检索与 Prompt；不重复落库）。
/// - `trigger_message_id`: 触发本轮回复的入站消息 id（None = 无回引；
///   用于发送前的撤回检查）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelReplyRequest {
    pub session_id: Uuid,
    pub persona_uid: String,
    pub message: String,
    pub trigger_message_id: Option<Uuid>,
}

// =========================================================
// 撤回通知用例
// =========================================================

/// 消息撤回通知（通道层解析产物）。
///
/// 字段约定:
/// - `channel`: 通道标识（`qq` / `telegram`）。
/// - `external_ref`: 会话外部标识（`c2c:{uin}` / `group:{群号}`）。
/// - `platform_message_id`: 被撤回消息的平台侧消息 ID。
/// - `operator_id`: 执行撤回的账号（平台未提供时 None）。
/// - `self_id`: 本机账号标识（None = 平台未提供）。
/// - `occurred_at`: 撤回发生时刻（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecallNoticeRequest {
    pub channel: String,
    pub external_ref: String,
    pub platform_message_id: String,
    pub operator_id: Option<String>,
    pub self_id: Option<String>,
    pub occurred_at: i64,
}

/// 撤回通知处理结果。
///
/// 变体:
/// - `Marked`: 已标记撤回；`triggered=true` 表示已生成主动回应，
///   false 时 `reason` 给出未触发原因；
/// - `Ignored`: 会话或消息未定位（`reason` 给出原因标识）；
/// - `AlreadyMarked`: 重复通知（幂等重入，未产生任何副作用）。
#[derive(Debug, Clone, PartialEq)]
pub enum RecallOutcome {
    Marked {
        triggered: bool,
        reason: Option<String>,
    },
    Ignored {
        reason: String,
    },
    AlreadyMarked,
}

// =========================================================
// 单张图片现场理解结果
// =========================================================

/// 单张已下载图片的现场理解结果（统计口径）。
///
/// 变体:
/// - `Done`: 调用成功并回填。
/// - `Reused`: 命中已有描述（md5 复用）直接回填。
/// - `Failed`: 调用失败（重试后仍失败 / 空描述）。
/// - `Skipped`: 门禁未通过 / 空或超限字节跳过（含隐私待确认，行保持待处理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelImageOutcome {
    Done,
    Reused,
    Failed,
    Skipped,
}
