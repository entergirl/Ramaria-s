//! crates/ramaria-importer/src/report.rs - Ramaria 导入解析诊断报告
//!
//! 设计特点:
//! - 提供完整的解析诊断信息：成功/降级/跳过 三类统计
//! - 摘要双形态：`summary` 保留原值供程序化消费，`summary_masked` 掩码昵称/账号供终端输出
//! - 记录文件信息（含双画像标识）、时间跨度和 session 切割结果
//! - 成员分布按发送者聚合（消息数降序），摘要展示 Top 5

use ramaria_core::privacy::mask_id;

// =========================================================
// 解析诊断报告
// =========================================================

/// 导入成员统计（按发送者聚合的解析结果）。
///
/// 字段约定:
/// - `uid`: 发送者平台内部标识（聚合键；空 UID 的消息不参与统计）。
/// - `uin`: 平台账号级 ID；取该发送者首个非空值，缺失为 None。
/// - `name`: 该发送者最后一条非空显示名（消息序即时间序）。
/// - `message_count`: 该发送者的成功解析消息条数。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImportMemberStat {
    pub uid: String,
    pub uin: Option<String>,
    pub name: String,
    pub message_count: usize,
}

/// 解析诊断报告，包含成功/降级/跳过三类统计。
///
/// 职责:
/// - 提供完整的文件解析结果概览，供 CLI 和前端展示。
/// - 记录文件信息（含双画像标识）、时间跨度和 session 切割结果。
/// - 覆盖 qce v6.x 全部 10 种消息类型（见 import-qq-schema.md §8）。
/// - 记录导出者与对话对象标识，支撑导入侧的 UID 生成策略。
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
    /// 因缺少 chatInfo 元信息（messages 先于 chatInfo 出现）而丢弃的消息数
    pub skipped_missing_meta: usize,
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

    // -- 成员分布 --
    /// 按发送者聚合的成员统计（消息数降序；空 UID 的消息不参与）
    pub members: Vec<ImportMemberStat>,

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

    /// 完全跳过的消息总数（含 system 消息跳过与缺失 chatInfo 元信息的丢弃）。
    pub fn total_skipped(&self) -> usize {
        self.skipped_recalled
            + self.skipped_system
            + self.skipped_empty
            + self.skipped_unknown
            + self.skipped_missing_meta
    }

    /// 生成人类可读的摘要文本（原值形态）。
    ///
    /// 用途:
    /// - 供 `--json` 信封与程序化消费：保留导出者/对方昵称与账号标识原值。
    /// - CLI 终端默认走 `summary_masked`，不直接使用本方法输出。
    pub fn summary(&self) -> String {
        self.render_summary(false)
    }

    /// 生成掩码版人类可读摘要（CLI 终端输出用）。
    ///
    /// 说明:
    /// - 双方姓名 / UID / QQ 号经 `mask_id` 脱敏后渲染，避免终端输出泄露标识；
    /// - 文件路径、时间范围与统计数字保持原样。
    pub fn summary_masked(&self) -> String {
        self.render_summary(true)
    }

    /// 摘要渲染实现；`masked=true` 时把双方姓名/UID/QQ 号替换为掩码。
    fn render_summary(&self, masked: bool) -> String {
        let id_of = |raw: &str| {
            if masked {
                mask_id(raw)
            } else {
                raw.to_string()
            }
        };
        let mut s = String::new();
        s.push_str(&format!("文件: {}\n", self.file_path));
        s.push_str(&format!(
            "导出者: {}（UID={}",
            id_of(&self.self_name),
            id_of(&self.self_id)
        ));
        if let Some(ref uin) = self.self_uin {
            s.push_str(&format!(", QQ号={}", id_of(uin)));
        }
        s.push_str(")\n");
        if !self.other_uid.is_empty() {
            s.push_str(&format!(
                "对话对象: {}（UID={}",
                id_of(&self.other_name),
                id_of(&self.other_uid)
            ));
            if let Some(ref uin) = self.other_uin {
                s.push_str(&format!(", QQ号={}", id_of(uin)));
            }
            s.push_str(")\n");
        } else {
            s.push_str(&format!(
                "对话对象: {}（{}）\n",
                id_of(&self.chat_name),
                self.chat_type
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
        // 成员分布：消息数降序展示 Top 5；仅展示显示名与条数（uid / uin 不进摘要文本）
        if !self.members.is_empty() {
            s.push_str(&format!(
                "成员分布: {} 人（消息数降序）:\n",
                self.members.len()
            ));
            const TOP_N: usize = 5;
            for member in self.members.iter().take(TOP_N) {
                s.push_str(&format!(
                    "  - {} {} 条\n",
                    id_of(&member.name),
                    member.message_count
                ));
            }
            if self.members.len() > TOP_N {
                s.push_str(&format!("  ... 等共 {} 人\n", self.members.len()));
            }
        }
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
            "❌ 跳过: {} 条（撤回 {}，系统 {}，空内容 {}，未知type {}，缺元信息 {}）\n",
            self.total_skipped(),
            self.skipped_recalled,
            self.skipped_system,
            self.skipped_empty,
            self.skipped_unknown,
            self.skipped_missing_meta,
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
            skipped_missing_meta: 0,
            unknown_types: Vec::new(),
            duplicate_check_enabled: false,
            duplicates_found: 0,
            session_count: 0,
            gap_minutes: 10,
            members: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 掩码版摘要不泄露昵称与账号标识；原值版保留既有输出。
    #[test]
    fn summary_masked_hides_identifiers() {
        let report = ImportReport {
            file_path: "x.json".into(),
            self_id: "u_self_001".into(),
            self_name: "导出者昵称A".into(),
            self_uin: Some("123456789".into()),
            chat_name: "对方昵称B".into(),
            chat_type: "private".into(),
            other_uid: "u_peer_001".into(),
            other_uin: Some("987654321".into()),
            other_name: "对方昵称B".into(),
            ..Default::default()
        };

        let masked = report.summary_masked();
        for raw in [
            "123456789",
            "987654321",
            "导出者昵称A",
            "对方昵称B",
            "u_self_001",
        ] {
            assert!(
                !masked.contains(raw),
                "掩码摘要不应包含原值 {raw}: {masked}"
            );
        }
        assert!(
            masked.contains("12…89"),
            "掩码摘要应含 mask_id 后的 QQ 号: {masked}"
        );

        let plain = report.summary();
        for raw in [
            "123456789",
            "987654321",
            "导出者昵称A",
            "对方昵称B",
            "u_self_001",
        ] {
            assert!(plain.contains(raw), "原值摘要应保留 {raw}: {plain}");
        }
    }

    /// 摘要渲染成员分布：消息数降序、Top 5 截断与总人数行；掩码版不泄露昵称。
    #[test]
    fn summary_renders_member_distribution_top5() {
        let members = (0..6)
            .map(|i| ImportMemberStat {
                uid: format!("u_{i}"),
                uin: None,
                name: format!("成员昵称{i}"),
                message_count: 100 - i,
            })
            .collect();
        let report = ImportReport {
            members,
            ..Default::default()
        };

        let plain = report.summary();
        assert!(plain.contains("成员分布: 6 人（消息数降序）:"), "{plain}");
        assert!(plain.contains("  - 成员昵称0 100 条"), "{plain}");
        assert!(
            plain.contains("  - 成员昵称4 96 条"),
            "Top 5 应展示: {plain}"
        );
        assert!(!plain.contains("成员昵称5"), "第 6 名不应单独成行: {plain}");
        assert!(plain.contains("  ... 等共 6 人"), "{plain}");

        // 掩码版：昵称经 mask_id 替换（5 字符昵称 → 首 2 + 尾 2），不出现原值
        let masked = report.summary_masked();
        assert!(masked.contains("成员分布: 6 人"), "{masked}");
        assert!(!masked.contains("成员昵称0"), "{masked}");
        assert!(masked.contains("成员…称0"), "{masked}");

        // 空成员分布不渲染该段
        let empty = ImportReport::default();
        assert!(!empty.summary().contains("成员分布"), "无成员时不渲染");
    }
}
