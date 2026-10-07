//! crates/ramaria-core/src/types/inbound.rs - Ramaria 入站消息规范模型
//!
//! 设计特点:
//! - 外部平台消息到内部模型的中间层：导入器与未来社交通道共同产出
//! - 零 I/O 纯类型，平台差异按字段可选性承载（QQ 号 / 群名片等）
//! - 字段设计参考 ChatLab 标准格式并按 Ramaria 需要裁剪

use serde::{Deserialize, Serialize};

use super::MemberRole;

/// 入站消息（外部平台消息的规范态）。
///
/// 职责:
/// - 承载外部平台单条消息的平台无关字段，作为导入器与后续社交通道
///   写入内部存储的统一输入形态。
///
/// 字段约定:
/// - `channel`: 来源通道（QQ 为 `qq`，见 `CHANNEL_QQ`）。
/// - `platform_message_id`: 平台消息 ID；平台不提供时为 None。
/// - `reply_to`: 被回复的平台消息 ID；非回复消息为 None。
/// - `created_at`: 消息时间（Unix 毫秒）。
/// - `text`: 规范正文（含占位符，不含导入端人名前缀）。
/// - `sender`: 发送者身份。
/// - `attachments`: 附件引用列表；无附件为空。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundMessage {
    pub channel: String,
    pub platform_message_id: Option<String>,
    pub reply_to: Option<String>,
    pub created_at: i64,
    pub text: String,
    pub sender: InboundSender,
    pub attachments: Vec<InboundAttachmentRef>,
}

/// 入站消息发送者身份。
///
/// 字段约定:
/// - `platform_id`: 平台内唯一 ID（QQ 为内部 UID）；空串表示平台未提供。
/// - `uin`: 平台账号级 ID（QQ 号）；缺失或非 QQ 平台为 None。
/// - `display_name`: 发送时显示名（可空串）。
/// - `group_nickname`: 群名片；私聊或未提供为 None。
/// - `role`: 群内角色；未知为 None。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundSender {
    pub platform_id: String,
    pub uin: Option<String>,
    pub display_name: String,
    pub group_nickname: Option<String>,
    pub role: Option<MemberRole>,
}

/// 入站附件类型。
///
/// 与存储侧 `message_attachments.kind` 取值一致（image / audio / video / file）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboundAttachmentKind {
    Image,
    Audio,
    Video,
    File,
}

impl InboundAttachmentKind {
    /// 返回存储口径的小写字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::File => "file",
        }
    }

    /// 解析存储口径字符串；非法值返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "image" => Some(Self::Image),
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// 入站消息附件引用（本地文件或远端 URL 的引用形态）。
///
/// 字段约定:
/// - `kind`: 附件类型。
/// - `source_ref`: 导出内相对路径或 URL（引用而非内容）。
/// - `md5`: 内容指纹；平台未提供时为 None。
/// - `size` / `width` / `height`: 可选的尺寸信息。
/// - `sub_type`: 平台细分类型（如 photo / sticker）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundAttachmentRef {
    pub kind: InboundAttachmentKind,
    pub source_ref: String,
    pub md5: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sub_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造全字段入站消息，断言字段完整性。
    #[test]
    fn inbound_message_carries_all_fields() {
        let message = InboundMessage {
            channel: "qq".to_string(),
            platform_message_id: Some("m_1".to_string()),
            reply_to: Some("m_0".to_string()),
            created_at: 1_700_000_000_000,
            text: "早上好".to_string(),
            sender: InboundSender {
                platform_id: "u_self".to_string(),
                uin: Some("10001".to_string()),
                display_name: "小明".to_string(),
                group_nickname: Some("群名片".to_string()),
                role: Some(MemberRole::Admin),
            },
            attachments: vec![InboundAttachmentRef {
                kind: InboundAttachmentKind::Image,
                source_ref: "images/a.png".to_string(),
                md5: Some("d41d8cd98f00b204".to_string()),
                size: Some(1024),
                width: Some(640),
                height: Some(480),
                sub_type: Some("photo".to_string()),
            }],
        };

        assert_eq!(message.channel, "qq");
        assert_eq!(message.platform_message_id.as_deref(), Some("m_1"));
        assert_eq!(message.reply_to.as_deref(), Some("m_0"));
        assert_eq!(message.created_at, 1_700_000_000_000);
        assert_eq!(message.text, "早上好");
        assert_eq!(message.sender.platform_id, "u_self");
        assert_eq!(message.sender.uin.as_deref(), Some("10001"));
        assert_eq!(message.sender.display_name, "小明");
        assert_eq!(message.sender.group_nickname.as_deref(), Some("群名片"));
        assert_eq!(message.sender.role, Some(MemberRole::Admin));
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].kind, InboundAttachmentKind::Image);
        assert_eq!(message.attachments[0].source_ref, "images/a.png");
        assert_eq!(message.attachments[0].width, Some(640));
    }

    /// 附件类型 as_str / parse 往返；非法值返回 None。
    #[test]
    fn attachment_kind_roundtrips_and_rejects_invalid() {
        for kind in [
            InboundAttachmentKind::Image,
            InboundAttachmentKind::Audio,
            InboundAttachmentKind::Video,
            InboundAttachmentKind::File,
        ] {
            assert_eq!(InboundAttachmentKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(InboundAttachmentKind::parse("unknown"), None);
        assert_eq!(InboundAttachmentKind::parse(""), None);
    }

    /// serde JSON 往返一个样例。
    #[test]
    fn inbound_message_serde_roundtrip() {
        let message = InboundMessage {
            channel: "qq".to_string(),
            platform_message_id: None,
            reply_to: None,
            created_at: 1,
            text: "你好".to_string(),
            sender: InboundSender {
                platform_id: String::new(),
                uin: None,
                display_name: String::new(),
                group_nickname: None,
                role: None,
            },
            attachments: Vec::new(),
        };

        let json = serde_json::to_string(&message).expect("序列化应成功");
        assert!(json.contains("\"channel\":\"qq\""), "channel 应序列化透出");
        let back: InboundMessage = serde_json::from_str(&json).expect("反序列化应成功");
        assert_eq!(message, back);
    }
}
