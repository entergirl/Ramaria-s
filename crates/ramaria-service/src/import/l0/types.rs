//! crates/ramaria-service/src/import/l0/types.rs - Ramaria 导入请求与结果类型
//!
//! 设计特点:
//! - 请求与结果只承载数据，不含 I/O 与校验逻辑
//! - 结果覆盖宿主展示所需的统计、双画像标识与 session 列表

use std::path::PathBuf;

use ramaria_importer::ImportSide;
use uuid::Uuid;

// =========================================================
// 请求与结果类型
// =========================================================

/// 导入模式。
///
/// 状态:
/// - `Fast`: 仅写入 L0（会话 + 消息），不触发后续记忆加工；
/// - `Deep`: L0 写入后由宿主继续调用 L1 批量生成与深度处理触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 快速导入：仅写入 L0 消息
    Fast,
    /// 深度导入：L0 之后继续 L1 摘要与 L2/L3 级联
    Deep,
}

impl ImportMode {
    /// 返回模式的稳定字符串标识（`fast` / `deep`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Deep => "deep",
        }
    }
}

/// L0 导入请求。
///
/// 字段约定:
/// - `persona_name` / `other_persona_name`: 画像显示名覆盖（None 时用文件内解析名）；
/// - `self_persona_uid` / `other_persona_uid`: 画像 UID 显式指定（None 时按平台策略生成）；
/// - `side`: 导入侧过滤（仅我方 / 仅对方 / 双方）。
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// 聊天记录文件路径（.json）
    pub file_path: PathBuf,
    /// 导入模式
    pub mode: ImportMode,
    /// session 切割时间间隔（分钟）
    pub gap_minutes: u32,
    /// 导入侧过滤
    pub side: ImportSide,
    /// 导出者画像显示名（None → 文件内解析名）
    pub persona_name: Option<String>,
    /// 导出者画像 UID（None → 按平台策略生成）
    pub self_persona_uid: Option<String>,
    /// 对方画像显示名（None → 文件内解析名）
    pub other_persona_name: Option<String>,
    /// 对方画像 UID（None → 按平台策略生成）
    pub other_persona_uid: Option<String>,
}

/// L0 写入结果（供宿主展示并继续触发 L1 / 深度处理）。
#[derive(Debug, Clone)]
pub struct ImportL0Outcome {
    /// 解析报告摘要（人类可读文本）
    pub report_summary: String,
    /// 写入的 session 数
    pub sessions_written: usize,
    /// 写入的消息总数
    pub messages_written: usize,
    /// 因画像缺失被丢弃的消息数
    pub messages_dropped: usize,
    /// 导出者画像 UID（导入侧过滤跳过时为 None）
    pub persona_uid: Option<String>,
    /// 导出者画像名称（库内实际注册名；画像未创建或回读失败时为请求覆盖名 / 文件解析名）
    pub persona_name: String,
    /// 对方画像 UID（导入侧过滤跳过时为 None）
    pub other_persona_uid: Option<String>,
    /// 对方画像名称（库内实际注册名；画像未创建或回读失败时为请求覆盖名 / 文件解析名）
    pub other_persona_name: String,
    /// 导出者名称（从文件中解析，供预览对照；与 `persona_name` 可能不同）
    pub self_name: String,
    /// 对话对象名称
    pub chat_name: String,
    /// 消息时间范围（`起 ~ 止`；未知时为"未知"）
    pub time_range: String,
    /// 跳过的消息数（撤回 + 空 + 未知类型）
    pub skipped_count: usize,
    /// 写入的 session UUID 列表（供继续触发 L1 与深度处理）
    pub session_ids: Vec<Uuid>,
    /// 本次导入模式
    pub mode: ImportMode,
    /// 对话类型（private / group / unknown）
    pub chat_type: String,
    /// 解析成员分布（消息数降序；私聊为双方），供宿主展示
    pub members: Vec<ramaria_importer::ImportMemberStat>,
}
