//! crates/ramaria-desktop/src/commands/import_cmd/result.rs - Ramaria 导入命令返回结构
//!
//! 设计特点:
//! - 分析报告只描述文件内容，不含导入统计
//! - 导入结果含双方 persona 标识与 L1 状态字段，供前端展示摘要与后续引导
//! - 空 session 列表不参与序列化（前端按缺省处理）

use serde::Serialize;

/// 导入操作的完整结果，序列化后返回给前端展示。
///
/// 包含双方 persona 标识与 L1 状态字段，供前端展示导入摘要与后续引导。
#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    /// 是否成功
    pub success: bool,
    /// 导入模式：fast / deep
    pub mode: String,
    /// 解析报告摘要（人类可读文本）
    pub report_summary: String,
    /// 写入的 session 数
    pub sessions_written: usize,
    /// 写入的消息总数
    pub messages_written: usize,
    /// 使用的 persona_uid（导出者；side=other 导入侧过滤时为 null）
    pub persona_uid: Option<String>,
    /// persona 名称（导出者）
    pub persona_name: String,
    /// 对方 persona UID（side=self 导入侧过滤时为 null）
    pub other_persona_uid: Option<String>,
    /// 对方 persona 名称
    pub other_persona_name: String,
    /// 导出者名称（从文件中解析）
    pub self_name: String,
    /// 对话对象名称（群聊为群名称）
    pub chat_name: String,
    /// 对话类型（private / group）
    pub chat_type: String,
    /// 解析成员分布（消息数降序；私聊为双方，群聊为全部成员）
    pub members: Vec<ramaria_importer::ImportMemberStat>,
    /// 对话时间范围（如 "2023-01-01 ~ 2024-06-30"）
    pub time_range: String,
    /// 跳过的消息数（撤回+空+未知类型）
    pub skipped_count: usize,
    /// 写入的 session_id 列表（供前端导航查看导入消息）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
}

/// 文件分析报告（不含导入相关的统计，仅描述文件内容）。
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    /// 文件路径
    pub file_path: String,
    /// 导出者 ID
    pub self_id: String,
    /// 导出者名称
    pub self_name: String,
    /// 导出者 QQ 号
    pub self_uin: Option<String>,
    /// 对话对象名称（chatInfo.name）
    pub chat_name: String,
    /// 对话类型
    pub chat_type: String,
    /// 解析成员分布（消息数降序；私聊为双方，群聊为全部成员）
    pub members: Vec<ramaria_importer::ImportMemberStat>,
    /// 对方名称
    pub other_name: String,
    /// 对方 QQ UID
    pub other_uid: String,
    /// 对方 QQ 号
    pub other_uin: Option<String>,
    /// 时间范围
    pub time_range: String,
    /// 原始消息总数
    pub total_raw: usize,
    /// 成功解析数
    pub total_success: usize,
    /// 降级处理数
    pub total_degraded: usize,
    /// 跳过数
    pub total_skipped: usize,
    /// 切割后的 session 数
    pub session_count: usize,
    /// 切割间隔（分钟）
    pub gap_minutes: u32,
}
