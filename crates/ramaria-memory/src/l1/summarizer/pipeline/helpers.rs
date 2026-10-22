//! crates/ramaria-memory/src/l1/summarizer/pipeline/helpers.rs - L1 摘要内部辅助
//!
//! 设计特点:
//! - 目标集合：绑定人格 ∪ 块内发言人，条数降序截断，绑定人格恒在首位
//! - 多画像分发：按块内参与者复制 L1 行，仅 id 与 persona_uid 不同
//! - 附件描述：切块前一次加载，查询失败降级为空映射

use std::collections::{BTreeSet, HashMap};

use ramaria_core::MemoryL1;
use ramaria_core::types::MessageRole;
use tracing::warn;
use uuid::Uuid;

use crate::utt::UttChunk;

use super::super::L1Summarizer;

/// 单块投影目标数上限（绑定人格 + 块内发言者合计）。
///
/// 说明:
/// - 控制群聊逐目标调用成本（调用次数 = 块数 × 目标数）；
/// - 不做配置键，按实测定值，超限按「消息条数降序 → 首次出现升序」截断。
const MAX_TARGETS_PER_CHUNK: usize = 8;

impl<'a> L1Summarizer<'a> {
    // =========================================================
    // 目标相对投影（内部辅助）
    // =========================================================

    /// 计算块级目标集合（投影模式）。
    ///
    /// 规则:
    /// - 集合 = 绑定人格（`config.persona_uid`，若配置）∪ 块内消息发言人
    ///   `persona_uid`（Some 且非空白，去重）；
    /// - 截断上限 [`MAX_TARGETS_PER_CHUNK`]：绑定人格恒保留在首位；
    ///   块内发言者按「消息条数降序 → 首次出现升序」取回剩余名额；
    /// - 绑定人格为 None 时目标集合全由块内发言者构成（仍受上限约束）；
    /// - 空块（无消息）返回空集合（调用方按非投影单目标兜底生成一次）。
    ///
    /// 返回:
    /// - 去重后的目标人格列表（绑定人格在首位；可能为空）。
    pub(super) fn projection_targets(&self, chunk: &UttChunk) -> Vec<String> {
        // 块内发言者统计：(uid, 消息条数)，保持首次出现顺序
        let mut speakers: Vec<(String, usize)> = Vec::new();
        for msg in &chunk.messages {
            let Some(uid) = msg
                .persona_uid
                .as_deref()
                .map(str::trim)
                .filter(|uid| !uid.is_empty())
            else {
                continue;
            };
            match speakers.iter_mut().find(|(known, _)| known == uid) {
                Some((_, count)) => *count += 1,
                None => speakers.push((uid.to_string(), 1)),
            }
        }
        // 条数降序（稳定排序：同条数保持首次出现顺序）
        speakers.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

        let mut targets: Vec<String> = Vec::new();
        if let Some(bound) = self
            .config
            .persona_uid
            .as_deref()
            .map(str::trim)
            .filter(|uid| !uid.is_empty())
        {
            targets.push(bound.to_string());
        }
        for (uid, _) in speakers {
            if targets.len() >= MAX_TARGETS_PER_CHUNK {
                break;
            }
            if targets.iter().any(|known| known == &uid) {
                continue;
            }
            targets.push(uid);
        }
        targets
    }

    // =========================================================
    // 多画像分发（内部辅助）
    // =========================================================

    /// 按块内参与者复制 L1 行（多画像分发）。
    ///
    /// 规则:
    /// - `fanout_others=false` → 原行（1 行）；
    /// - 参与者 = 块内 Assistant 角色且 persona_uid 非空的消息发送者集合
    ///   （去重、稳定序）；`fanout_include_self=true` 时参与者额外纳入
    ///   User 角色且 persona_uid 非空的发言者（含"我"的两态口径）；
    /// - 参与者为空（无参与发言）→ 原行保留（persona_uid 为 config 值，导入场景即 NULL）；
    /// - 参与者非空 → 每参与者复制一行（新 id、各自 persona_uid），原行不落库。
    ///
    /// 参数:
    /// - `l1`: 块/段生成结果（复制模板）。
    /// - `chunk`: 生成该 L1 的话语块（读取发言者）。
    ///
    /// 返回:
    /// - 待落库的 L1 行列表（各行摘要内容一致，仅 id 与 persona_uid 不同）。
    pub(super) fn fanout_rows(&self, l1: &MemoryL1, chunk: &UttChunk) -> Vec<MemoryL1> {
        if !self.config.fanout_others {
            return vec![l1.clone()];
        }

        let participants: BTreeSet<&str> = chunk
            .messages
            .iter()
            .filter(|m| self.config.fanout_include_self || m.role == MessageRole::Assistant)
            .filter_map(|m| m.persona_uid.as_deref())
            .collect();

        if participants.is_empty() {
            // 无他人参与（纯 self / 无他人发言者）→ 原行保留
            return vec![l1.clone()];
        }

        participants
            .into_iter()
            .map(|uid| {
                let mut row = l1.clone();
                row.id = ramaria_core::types::new_id();
                row.persona_uid = Some(uid.to_string());
                row
            })
            .collect()
    }

    // =========================================================
    // 附件描述加载（内部辅助）
    // =========================================================

    /// 加载会话消息的附件描述映射（`[图片#{hash}]` → 描述）。
    ///
    /// 说明:
    /// - 切块前一次加载全部会话消息的附件行，供对话文本与上文注入渲染；
    /// - 查询失败记 warn 并返回空映射（对话文本保留占位符原文，L1 不阻塞）。
    pub(super) async fn load_attachment_descriptions(
        &self,
        messages: &[ramaria_core::types::Message],
    ) -> HashMap<String, String> {
        let message_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();
        if message_ids.is_empty() {
            return HashMap::new();
        }
        match self
            .storage
            .list_attachments_by_messages(&message_ids)
            .await
        {
            Ok(rows) => ramaria_core::types::build_render_map(&rows),
            Err(e) => {
                warn!(error = %e, "L1 摘要附件查询失败，对话文本保留占位符原文");
                HashMap::new()
            }
        }
    }
}
