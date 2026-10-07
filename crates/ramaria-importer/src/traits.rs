//! crates/ramaria-importer/src/traits.rs - 导入器抽象层
//!
//! 设计特点:
//! - `ImportSource` trait 定义导入源的统一接口，便于扩展 QQ/微信/Telegram 等格式
//! - `ParsedMessage` 为解析后的中间表示，与存储层的 `Message` 解耦
//! - `ImportMode` 区分快速导入（仅 L0）和深度导入（全管线）
//! - `PersonaSide` / `ImportSide` 为导入源无关的"双人对话双方/导入侧过滤"模型，
//!   供各导入源与通用写入层复用；平台特有的 UID/画像命名规则留在各导入源模块
//! - ParsedMessage 新增 sender 标识字段，支持双画像导入
//! - 群聊支持：解析消息携带群名片 / 群内角色
//! - 解析诊断报告（`ImportReport` / `ImportMemberStat`）位于 `report` 模块

pub use crate::report::{ImportMemberStat, ImportReport};

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{
    CHANNEL_QQ, InboundAttachmentRef, InboundMessage, InboundSender, MemberRole,
};
use std::path::Path;

// =========================================================
// 导入模式
// =========================================================

/// 导入模式枚举。
///
/// 职责:
/// - `Fast`: 仅写入 messages 表（L0），关闭 session 后不触发记忆管线。
/// - `Deep`: 创建 session → 写入 L0 → 关闭 session → 触发 L1→L2→L3 全管线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 快速导入：仅写入 L0 消息，适合快速预览历史对话
    Fast,
    /// 深度导入：走完整记忆管线，生成 L1 摘要、L2 事件和 L3 性格画像
    Deep,
}

impl std::fmt::Display for ImportMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fast => write!(f, "fast"),
            Self::Deep => write!(f, "deep"),
        }
    }
}

// =========================================================
// 双人对话双方与导入侧过滤（导入源无关）
// =========================================================

///
/// 双人对话中的一方（导出者本人 / 对话另一方）。
///
/// 职责:
/// - 标识一次人对人导入中"谁是导出者（我方）"、"谁是对话对象（对方）"。
/// - 供 `ImportSide` 过滤、会话归属和画像归属决策使用。
///
/// 说明:
/// - 该模型对任何"导出者 + 对话对象"形式的聊天平台导入都成立，不绑定具体平台。
/// - 各平台的画像命名规则（如 UID 前缀、kind 映射）由各导入源自行定义，不在此处承载。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaSide {
    /// 我方（导出者本人）
    Me,
    /// 对方（对话另一方）
    Other,
}

///
/// 导入侧过滤选项（`import --side self|other|both`）。
///
/// 语义:
/// - `Me`（self）: 只处理我方消息；跳过侧（对方）消息不入库、对方画像不创建。
/// - `Other`: 只处理对方消息；我方画像不创建。
/// - `Both`: 双方都处理（默认）。
///
/// 用途:
/// - 调用方（CLI `--side` / 桌面导入面板）按选项控制画像创建与消息写入。
/// - 通用写入层（`writer.rs`）按该过滤决定保留哪些消息、会话归属哪一方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSide {
    /// 只处理我方（self）
    Me,
    /// 只处理对方（other）
    Other,
    /// 双方都处理（默认）
    Both,
}

impl ImportSide {
    /// 解析 CLI/前端字符串（`self`/`other`/`both`，大小写不敏感）。
    ///
    /// 返回:
    /// - `Ok(Some(side))`: 合法值。
    /// - `Ok(None)`: 空/未提供 → 默认 `Both`。
    /// - `Err(msg)`: 非法值（业务校验失败提示）。
    pub fn parse_cli(value: Option<&str>) -> Result<ImportSide, String> {
        match value.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("both") => Ok(ImportSide::Both),
            Some("self") | Some("me") => Ok(ImportSide::Me),
            Some("other") => Ok(ImportSide::Other),
            Some(other) => Err(format!(
                "不支持的导入侧: '{other}'（仅支持 self | other | both）"
            )),
        }
    }

    /// 该侧是否需要创建画像（Both 时两侧都创建）。
    pub fn needs_persona(self, side: PersonaSide) -> bool {
        match self {
            ImportSide::Both => true,
            ImportSide::Me => side == PersonaSide::Me,
            ImportSide::Other => side == PersonaSide::Other,
        }
    }
}

// =========================================================
// 解析后消息（中间表示）
// =========================================================

/// 解析后的单条消息，是 parser 和 importer 之间的中间表示。
///
/// 职责:
/// - 保存解析后的标准化字段，与存储层 `Message` 解耦。
/// - `created_at` 用于 session 切割和时间排序。
/// - `fingerprint` 用于跨导入批次的重复检测。
/// - : 新增 sender 标识字段（uid/uin/name），支持按发送者分配画像。
///
/// 字段约定:
/// - `role`: "user" 表示导出者本人，"assistant" 表示对方。
/// - `content`: 已经过占位符替换和前缀处理的最终文本。
/// - `sender_uid`: QQ 内部用户标识（如 `u_example_uid`）。
/// - `sender_uin`: QQ 号（如 `123456789`），不存在时为 None。
/// - `sender_name`: 发送者显示昵称/群名片。
/// - `group_nickname`: 群名片；私聊或导出未提供时为 None。
/// - `member_role`: 群内角色；私聊或导出未提供时为 None。
/// - `attachments`: 附件引用列表（导出资源的引用而非内容）；无附件为空。
#[derive(Debug, Clone)]
pub struct ParsedMessage {
    /// 消息角色：user / assistant
    pub role: String,
    /// 处理后的消息正文
    pub content: String,
    /// 消息创建时间（Unix 毫秒）
    pub created_at: i64,
    /// SHA-256 前 16 位 hex，用于去重
    pub fingerprint: String,
    /// 发送者的 QQ 内部 UID
    pub sender_uid: String,
    /// 发送者的 QQ 号（uin），不存在时为 None
    pub sender_uin: Option<String>,
    /// 发送者的显示名称
    pub sender_name: String,
    /// 发送者的群名片（群聊导出提供时）
    pub group_nickname: Option<String>,
    /// 发送者在群内的角色（群聊导出提供时）
    pub member_role: Option<MemberRole>,
    /// 附件引用列表（导出资源的引用而非内容）
    pub attachments: Vec<InboundAttachmentRef>,
}

impl ParsedMessage {
    /// 投影为入站规范模型（QQ 通道）。
    ///
    /// 职责:
    /// - 把 QQ 解析中间态投影为平台无关的入站消息：发送者身份 / 时间 / 正文 /
    ///   附件引用；
    ///
    /// 说明:
    /// - `role` / `fingerprint` 为导入端归属与去重信息，不进规范模型；
    /// - `platform_id` 原样保留（空串表示平台未提供，判空在写入侧执行）；
    /// - 附件引用原样投射（source_ref 已在解析期规范化为导出根相对路径）。
    pub fn to_inbound(&self) -> InboundMessage {
        InboundMessage {
            channel: CHANNEL_QQ.to_string(),
            platform_message_id: None,
            reply_to: None,
            created_at: self.created_at,
            text: self.content.clone(),
            sender: InboundSender {
                platform_id: self.sender_uid.clone(),
                uin: self.sender_uin.clone(),
                display_name: self.sender_name.clone(),
                group_nickname: self.group_nickname.clone(),
                role: self.member_role,
            },
            attachments: self.attachments.clone(),
        }
    }
}

/// 解析后的 session（一组消息）。
///
/// 职责:
/// - 按时间间隔切割后的一组连续消息。
/// - `started_at` / `ended_at` 用于创建历史 session。
pub struct ImportedSession {
    /// 本 session 内的消息列表
    pub messages: Vec<ParsedMessage>,
    /// Session 开始时间（首条消息的 created_at）
    pub started_at: i64,
    /// Session 结束时间（末条消息的 created_at）
    pub ended_at: i64,
}

// =========================================================
// ImportSource trait
// =========================================================

/// 导入源抽象 trait。
///
/// 职责:
/// - 定义不同聊天平台（QQ/微信/Telegram 等）的统一导入接口。
/// - 每个平台实现自己的格式检测、解析和消息转换逻辑。
///
/// 实现要求:
/// - `name` 返回静态名称，用于日志和 UI 展示。
/// - `detect_format` 检测文件是否为当前平台支持的格式。
/// - `parse` 解析文件，返回标准化消息列表和诊断报告。
/// - 不在此 trait 中定义数据库写入逻辑（由 importer 模块负责）。
#[async_trait::async_trait]
pub trait ImportSource: Send + Sync {
    /// 返回导入源名称（如 "QQ"）。
    fn name(&self) -> &'static str;

    /// 检测文件格式是否为当前平台支持的格式。
    ///
    /// 参数:
    /// - `file_path`: 待检测的文件路径。
    ///
    /// 返回:
    /// - `true`: 文件格式匹配，可以使用 `parse` 解析。
    /// - `false`: 格式不匹配，应尝试其他 parser。
    fn detect_format(&self, file_path: &Path) -> RamariaResult<bool>;

    /// 解析文件为标准化消息列表。
    ///
    /// 参数:
    /// - `file_path`: 待解析的文件路径。
    /// - `gap_minutes`: session 切割的时间间隔阈值（分钟）。
    ///
    /// 返回:
    /// - `(sessions, report)`: 解析后的 session 列表和诊断报告。
    fn parse(
        &self,
        file_path: &Path,
        gap_minutes: u32,
    ) -> RamariaResult<(Vec<ImportedSession>, ImportReport)>;
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::InboundAttachmentKind;

    /// to_inbound 投影：发送者身份 / 时间 / 正文 / 附件完整映射；空平台 ID 原样保留。
    #[test]
    fn to_inbound_maps_sender_fields_and_keeps_empty_uid() {
        let parsed = ParsedMessage {
            role: "user".to_string(),
            content: "早上好".to_string(),
            created_at: 1_700_000_000_000,
            fingerprint: "fp-x".to_string(),
            sender_uid: "u_self".to_string(),
            sender_uin: Some("10001".to_string()),
            sender_name: "小明".to_string(),
            group_nickname: None,
            member_role: None,
            attachments: vec![InboundAttachmentRef {
                kind: InboundAttachmentKind::Image,
                source_ref: "resources/images/aabbccdd_AABBCCDD.jpg".to_string(),
                md5: Some("aabbccddeeff00112233445566778899".to_string()),
                size: Some(1024),
                width: Some(640),
                height: Some(480),
                sub_type: Some("photo".to_string()),
            }],
        };

        let inbound = parsed.to_inbound();
        assert_eq!(inbound.channel, CHANNEL_QQ);
        assert_eq!(inbound.created_at, parsed.created_at);
        assert_eq!(inbound.text, "早上好");
        assert_eq!(inbound.sender.platform_id, "u_self");
        assert_eq!(inbound.sender.uin.as_deref(), Some("10001"));
        assert_eq!(inbound.sender.display_name, "小明");
        assert!(inbound.sender.group_nickname.is_none());
        assert!(inbound.sender.role.is_none());
        assert!(inbound.platform_message_id.is_none());
        assert!(inbound.reply_to.is_none());
        // 附件引用原样投射（source_ref 为解析期规范化的导出根相对路径）
        assert_eq!(inbound.attachments.len(), 1);
        assert_eq!(inbound.attachments[0].kind, InboundAttachmentKind::Image);
        assert_eq!(
            inbound.attachments[0].source_ref,
            "resources/images/aabbccdd_AABBCCDD.jpg"
        );
        assert_eq!(
            inbound.attachments[0].md5.as_deref(),
            Some("aabbccddeeff00112233445566778899")
        );
        assert_eq!(inbound.attachments[0].size, Some(1024));
        assert_eq!(inbound.attachments[0].width, Some(640));
        assert_eq!(inbound.attachments[0].height, Some(480));
        assert_eq!(inbound.attachments[0].sub_type.as_deref(), Some("photo"));

        // 空 uid / 空名原样保留（判空口径在写入侧执行）
        let mut empty = parsed.clone();
        empty.sender_uid = String::new();
        empty.sender_uin = None;
        empty.sender_name = String::new();
        empty.attachments = Vec::new();
        let inbound = empty.to_inbound();
        assert_eq!(inbound.sender.platform_id, "");
        assert!(inbound.sender.uin.is_none());
        assert_eq!(inbound.sender.display_name, "");
        assert!(inbound.attachments.is_empty(), "无附件时投影为空列表");
    }

    /// 群名片与群内角色原样投射到入站发送者。
    #[test]
    fn to_inbound_maps_group_nickname_and_role() {
        let parsed = ParsedMessage {
            role: "assistant".to_string(),
            content: "[昵称A] 大家好".to_string(),
            created_at: 1_700_000_000_001,
            fingerprint: "fp-group".to_string(),
            sender_uid: "u_a".to_string(),
            sender_uin: Some("10001".to_string()),
            sender_name: "昵称A".to_string(),
            group_nickname: Some("群名片A".to_string()),
            member_role: Some(MemberRole::Owner),
            attachments: Vec::new(),
        };

        let inbound = parsed.to_inbound();
        assert_eq!(inbound.sender.group_nickname.as_deref(), Some("群名片A"));
        assert_eq!(inbound.sender.role, Some(MemberRole::Owner));
        assert!(inbound.attachments.is_empty());
    }
}
