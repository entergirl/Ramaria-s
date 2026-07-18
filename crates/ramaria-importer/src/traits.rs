//! rust/crates/ramaria-importer/src/traits.rs - 导入器抽象层
//!
//! 设计特点:
//! - `ImportSource` trait 定义导入源的统一接口，便于扩展 QQ/微信/Telegram 等格式
//! - `ParsedMessage` 为解析后的中间表示，与存储层的 `Message` 解耦
//! - `ImportReport` 提供完整的诊断信息：成功/降级/跳过 三类统计
//! - `ImportMode` 区分快速导入（仅 L0）和深度导入（全管线）
//! - ParsedMessage 新增 sender 标识字段，支持双画像导入
//! - ImportReport 新增双方 QQ 号及对方标识字段，支持 UID 生成策略

use ramaria_core::error::RamariaResult;
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
/// - `sender_uid`: QQ 内部用户标识（如 `u_RSOI7gG2LaRiP64W8ayLDA`）。
/// - `sender_uin`: QQ 号（如 `123456789`），不存在时为 None。
/// - `sender_name`: 发送者显示昵称/群名片。
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
// 解析诊断报告
// =========================================================

/// 解析诊断报告，包含成功/降级/跳过三类统计。
///
/// 职责:
/// - 提供完整的文件解析结果概览，供 CLI 和前端展示。
/// - 记录文件信息（含双画像标识）、时间跨度和 session 切割结果。
/// - 覆盖 qce v6.x 全部 10 种消息类型（见 import-qq-schema.md §8）。
/// - : 新增导出者 QQ 号和对方标识，支持双画像导入。
#[derive(Debug, Clone)]
pub struct ImportReport {
    // -- 文件信息 --
    /// 解析的文件路径
    pub file_path: String,
    /// 导出者标识（QQ UID）
    pub self_id: String,
    /// 导出者名称
    pub self_name: String,
    /// 导出者 QQ 号（chatInfo.selfUin），不存在时为 None
    pub self_uin: Option<String>,
    /// 对话对象名称（chatInfo.name）
    pub chat_name: String,
    /// 对话类型（private / group）
    pub chat_type: String,
    /// 对话对方 QQ UID（从 chatInfo.peerUid 直接提取）
    pub other_uid: String,
    /// 对话对方 QQ 号（从 chatInfo.peerUin 直接提取），不存在时为 None
    pub other_uin: Option<String>,
    /// 对话对方名称（chatInfo.name）
    pub other_name: String,

    // -- 时间范围 --
    /// 最早消息日期（YYYY-MM-DD）
    pub time_start: String,
    /// 最晚消息日期（YYYY-MM-DD）
    pub time_end: String,

    // -- 原始统计 --
    /// 文件中的原始消息总数
    pub total_raw: usize,
    /// 文件内去重移除数
    pub dedup_removed: usize,

    // -- 成功解析 --
    /// 纯文本消息数（含表情、emoji）
    pub success_text: usize,
    /// 含图片消息数
    pub success_image: usize,
    /// 回复消息数
    pub success_reply: usize,
    /// 对方发言消息数（仅 text 和 reply 类型）
    pub success_other_sender: usize,

    // -- 降级处理（非文本消息→文本占位符） --
    /// 无 reply 元素的回复消息（降级提取正文）
    pub degraded_reply_fallback: usize,
    /// 合并转发消息 → [转发消息]
    pub degraded_forward: usize,
    /// 卡片消息 → [卡片消息]
    pub degraded_card: usize,
    /// 语音消息 → [语音]
    pub degraded_audio: usize,
    /// 视频消息 → [视频]
    pub degraded_video: usize,
    /// 文件消息 → [文件: filename]
    pub degraded_file: usize,
    /// 红包/转账消息 → [红包/转账]
    pub degraded_red_envelope: usize,
    /// qce 未解析的消息类型降级（如 type_19 通话记录）
    pub degraded_qce_unsupported: usize,

    // -- 完全跳过 --
    /// 撤回消息
    pub skipped_recalled: usize,
    /// 系统消息（system == true）
    pub skipped_system: usize,
    /// content.text 为空且无法从 elements 提取有效文本的消息
    pub skipped_empty: usize,
    /// 未知 type 的消息
    pub skipped_unknown: usize,
    /// 遇到的未知 type 值列表
    pub unknown_types: Vec<String>,

    // -- 重复预检 --
    /// 是否启用了重复导入预检
    pub duplicate_check_enabled: bool,
    /// 发现已导入的重复消息数
    pub duplicates_found: usize,

    // -- Session 切割 --
    /// 切割后的 session 数量
    pub session_count: usize,
    /// 切割时间间隔（分钟）
    pub gap_minutes: u32,

    // -- 警告信息 --
    /// 非致命警告列表
    pub warnings: Vec<String>,
}

impl ImportReport {
    /// 成功解析的消息总数（纯文本 + 图片 + 回复）。
    pub fn total_success(&self) -> usize {
        self.success_text + self.success_image + self.success_reply
    }

    /// 降级处理的消息总数（含 3 种降级类型）。
    pub fn total_degraded(&self) -> usize {
        self.degraded_reply_fallback
            + self.degraded_forward
            + self.degraded_card
            + self.degraded_audio
            + self.degraded_video
            + self.degraded_file
            + self.degraded_red_envelope
            + self.degraded_qce_unsupported
    }

    /// 完全跳过的消息总数（含 system 消息跳过）。
    pub fn total_skipped(&self) -> usize {
        self.skipped_recalled + self.skipped_system + self.skipped_empty + self.skipped_unknown
    }

    /// 生成人类可读的摘要文本。
    ///
    /// 新增导出者 QQ 号和对方标识信息。
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("文件: {}\n", self.file_path));
        s.push_str(&format!("导出者: {}（UID={}", self.self_name, self.self_id));
        if let Some(ref uin) = self.self_uin {
            s.push_str(&format!(", QQ号={}", uin));
        }
        s.push_str(")\n");
        if !self.other_uid.is_empty() {
            s.push_str(&format!(
                "对话对象: {}（UID={}",
                self.other_name, self.other_uid
            ));
            if let Some(ref uin) = self.other_uin {
                s.push_str(&format!(", QQ号={}", uin));
            }
            s.push_str(")\n");
        } else {
            s.push_str(&format!(
                "对话对象: {}（{}）\n",
                self.chat_name, self.chat_type
            ));
        }
        s.push_str(&format!(
            "时间范围: {} ~ {}\n",
            self.time_start, self.time_end
        ));
        s.push_str(&format!(
            "原始消息: {} 条（文件内去重 {} 条）\n",
            self.total_raw, self.dedup_removed
        ));
        s.push_str(&format!(
            "切割 session: {} 个（间隔 {} 分钟）\n",
            self.session_count, self.gap_minutes
        ));
        if self.duplicate_check_enabled {
            s.push_str(&format!(
                "重复预检: 发现 {} 条已导入消息\n",
                self.duplicates_found
            ));
        }
        s.push_str(&format!(
            "✅ 成功: {} 条（纯文本 {}，含图片 {}，回复 {}）\n",
            self.total_success(),
            self.success_text,
            self.success_image,
            self.success_reply,
        ));
        s.push_str(&format!(
            "⚠️  降级: {} 条（回复降级 {}，转发 {}，卡片 {}，语音 {}，视频 {}，文件 {}，红包/转账 {}，qce未解析 {}）\n",
            self.total_degraded(),
            self.degraded_reply_fallback,
            self.degraded_forward,
            self.degraded_card,
            self.degraded_audio,
            self.degraded_video,
            self.degraded_file,
            self.degraded_red_envelope,
            self.degraded_qce_unsupported,
        ));
        s.push_str(&format!(
            "❌ 跳过: {} 条（撤回 {}，系统 {}，空内容 {}，未知type {}）\n",
            self.total_skipped(),
            self.skipped_recalled,
            self.skipped_system,
            self.skipped_empty,
            self.skipped_unknown,
        ));
        if !self.unknown_types.is_empty() {
            s.push_str(&format!("  未知type: {:?}\n", self.unknown_types));
        }
        s
    }
}

impl Default for ImportReport {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            self_id: String::new(),
            self_name: String::new(),
            self_uin: None,
            chat_name: String::new(),
            chat_type: String::new(),
            other_uid: String::new(),
            other_uin: None,
            other_name: String::new(),
            time_start: String::new(),
            time_end: String::new(),
            total_raw: 0,
            dedup_removed: 0,
            success_text: 0,
            success_image: 0,
            success_reply: 0,
            success_other_sender: 0,
            degraded_reply_fallback: 0,
            degraded_forward: 0,
            degraded_card: 0,
            degraded_audio: 0,
            degraded_video: 0,
            degraded_file: 0,
            degraded_red_envelope: 0,
            degraded_qce_unsupported: 0,
            skipped_recalled: 0,
            skipped_system: 0,
            skipped_empty: 0,
            skipped_unknown: 0,
            unknown_types: Vec::new(),
            duplicate_check_enabled: false,
            duplicates_found: 0,
            session_count: 0,
            gap_minutes: 10,
            warnings: Vec::new(),
        }
    }
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
