//! crates/ramaria-service/src/types/sticker.rs - Ramaria 表情资产与本地发图用例数据结构
//!
//! 设计特点:
//! - 表情主题解析结果（命中资产 + 原始字节）与本地发图请求 / 结果三类
//! - 本地发图结果携带落库消息 id / 附件行 id / 占位符与最终正文，入口层按需回执

use uuid::Uuid;

// =========================================================
// 表情主题解析
// =========================================================

/// 表情主题解析结果（命中资产的主题名与原始字节）。
///
/// 字段约定:
/// - `id`: 资产行 id；
/// - `file_name`: 资产原始文件名（用户视角展示用）；
/// - `bytes`: 资产文件字节（出站发送载荷）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerPayload {
    pub id: i64,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

// =========================================================
// 本地发图
// =========================================================

/// 本地会话发图请求（消息与附件行一并落库）。
///
/// 字段约定:
/// - `session_id`: 目标会话（仅本地会话，社交通道会话拒绝）；
/// - `file_name`: 原始文件名（附件 source_ref 记账用）；
/// - `bytes`: 图片字节；
/// - `caption`: 可选文字说明（空 / None = 仅图片占位符）。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalImageSendRequest {
    pub session_id: Uuid,
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub caption: Option<String>,
}

/// 本地发图结果。
///
/// 字段约定:
/// - `message_id`: 落库的用户消息 id；
/// - `attachment_id`: 落库的附件行 id；
/// - `md5`: 图片字节 md5（完整小写 hex；缓存落盘命名与理解链路共用）；
/// - `placeholder`: 正文中的图片占位符（`[图片#{hash}]`）；
/// - `content`: 最终落库正文（caption 在前、占位符在后）。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalImageSendOutcome {
    pub message_id: Uuid,
    pub attachment_id: i64,
    pub md5: String,
    pub placeholder: String,
    pub content: String,
}
