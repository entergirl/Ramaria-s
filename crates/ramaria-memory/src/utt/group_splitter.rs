//! crates/ramaria-memory/src/utt/group_splitter.rs - utt 群聊话语块切分器
//!
//! 设计特点:
//! - 纯函数模块：输入消息序列 + 配置 → 输出话语块，无 IO、无状态
//! - 群聊多发言者口径：无单一目标 persona，按块内发言者（消息 persona_uid）集合判定单边
//! - 切分规则:
//!   1. 时间间隙切分：相邻消息间隔 > θ_gap 分钟 → 新块
//!   2. 条数上限切分：块内消息达到 max_msgs_per_block → 新块
//!   3. 单边块（发言者集合 ≤ 1 类）按时间间隔更短的一侧并入相邻块
//! - 全部块按时间序保留（不丢弃）：群聊无"不含目标发言"的过滤概念
//! - 系统/工具消息不进入块（不是对话原文）
//! - 输入约定时间升序；防御性按 created_at 稳定排序
//!
//! 合并规则说明:
//! - "单边"指块内消息的 persona_uid 去重集合大小 ≤ 1（None 视为一类）：
//!   仅一个发言者的块（如独白）不独立存在。
//! - 单边块按时间间隔更短的一侧并入相邻块：
//!   比较单边块首条与前块末条的间隔、后块首条与单边块末条的间隔，取短侧；
//!   首块仅后侧、末块仅前侧；等距时并入前块（保持时间顺序）。
//! - 合并循环收敛：每次合并减少一块，最多 n-1 次；单边块并入后若仍单边继续合并。
//! - 合并可突破 θ_gap 与条数上限（合并优先于上限，注释约定）。

use std::collections::HashSet;

use ramaria_core::types::Message;

use super::{UttChunk, UttSplitterConfig, is_chat_message};

/// 将消息序列切分为话语块（群聊多发言者口径）。
///
/// 与 [`split_messages`](super::splitter::split_messages) 的差异:
/// - "单边"判定为块内发言者（消息 persona_uid）集合大小 ≤ 1（多人群聊无单一目标 persona）；
/// - 不做"不含目标发言的块丢弃"（全部块按时间序保留）。
///
/// 参数:
/// - `messages`: 会话消息（时间升序约定；内部防御性排序）。
/// - `config`: 切分配置（θ_gap / 条数上限）。
///
/// 返回:
/// - 话语块列表（时间升序）。
///
/// 边界:
/// - 空输入 → 空输出。
/// - 单条消息 → 单个块。
/// - 间隙恰好等于 θ_gap 分钟 → 不切分（严格大于才切）。
pub fn split_messages_group(messages: &[Message], config: &UttSplitterConfig) -> Vec<UttChunk> {
    // 防御：过滤非对话消息 + 按时间稳定排序（输入约定升序，但导入等场景可能乱序）
    let mut chat: Vec<Message> = messages
        .iter()
        .filter(|m| is_chat_message(m))
        .cloned()
        .collect();
    chat.sort_by_key(|m| m.created_at);

    if chat.is_empty() {
        return Vec::new();
    }

    let gap_ms = (config.theta_gap_minutes as i64) * 60_000;
    let max_count = config.max_msgs_per_block.max(1);

    // ---- 候选切分：间隙 / 条数上限 ----
    let mut candidates: Vec<UttChunk> = Vec::new();
    let mut current: Vec<Message> = Vec::with_capacity(max_count as usize);

    for m in chat {
        if !current.is_empty() {
            let gap = m.created_at - current.last().expect("非空").created_at;
            let over_gap = gap > gap_ms;
            let over_count = current.len() as u32 >= max_count;
            if over_gap || over_count {
                candidates.push(UttChunk::from_messages(std::mem::take(&mut current)));
            }
        }
        current.push(m);
    }
    if !current.is_empty() {
        candidates.push(UttChunk::from_messages(current));
    }

    // ---- 单边合并（收敛循环：按时间间隔更短的一侧并入相邻块） ----
    let mut chunks = candidates;
    let mut i = 0;
    while i < chunks.len() {
        if !has_single_speaker(&chunks[i]) {
            i += 1;
            continue;
        }

        let has_prev = i > 0;
        let has_next = i + 1 < chunks.len();
        if !has_prev && !has_next {
            // 仅剩一块且仍单边：无法合并，保留
            break;
        }

        // 间隔计算（毫秒）：
        // - 前侧间隔 = 单边块首条与前块末条的时间差
        // - 后侧间隔 = 后块首条与单边块末条的时间差
        let gap_prev = has_prev.then(|| {
            chunks[i].messages.first().expect("块非空").created_at
                - chunks[i - 1].messages.last().expect("块非空").created_at
        });
        let gap_next = has_next.then(|| {
            chunks[i + 1].messages.first().expect("块非空").created_at
                - chunks[i].messages.last().expect("块非空").created_at
        });

        // 方向判定：
        // - 两侧都有 → 取短侧（<= 表示等距时并入前块，保持时间顺序）
        // - 末块（仅前侧）→ 并入前块；首块（仅后侧）→ 并入后块
        let merge_into_prev = match (gap_prev, gap_next) {
            (Some(gp), Some(gn)) => gp <= gn,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => unreachable!("has_prev || has_next 已保证至少一侧"),
        };

        if merge_into_prev {
            // 并入前块末尾（收尾型独白：保持意义连贯）
            let cur = chunks.remove(i);
            chunks[i - 1] = merge_back(chunks[i - 1].clone(), cur);
            i -= 1; // 回退：合并后的前块可能仍单边，继续向前检查
        } else {
            // 并入后块开头（提问型独白：问答配对同块）
            let cur = chunks.remove(i);
            chunks[i] = merge_front(chunks[i].clone(), cur);
            // i 保持不变：新合并块可能仍单边，继续按短侧原则检查
        }
    }

    // 群聊口径：全部块保留（不做"不含目标发言"的丢弃）
    chunks
}

/// 块内发言者集合（消息 persona_uid 去重，None 视为一类）大小是否 ≤ 1。
fn has_single_speaker(chunk: &UttChunk) -> bool {
    let mut speakers: HashSet<Option<&str>> = HashSet::new();
    for m in &chunk.messages {
        speakers.insert(m.persona_uid.as_deref());
    }
    // 空块按单边处理（防御：调用方不会产生空块）
    speakers.len() <= 1
}

/// 将 `other` 追加到 `base` 末尾（单边合并用）。
fn merge_back(base: UttChunk, other: UttChunk) -> UttChunk {
    let mut messages = base.messages;
    messages.extend(other.messages);
    UttChunk::from_messages(messages)
}

/// 将 `other` 插入到 `base` 开头（首块并入后块用）。
fn merge_front(base: UttChunk, other: UttChunk) -> UttChunk {
    let mut messages = other.messages;
    messages.extend(base.messages);
    UttChunk::from_messages(messages)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{MessageRole, MessageSource};

    /// 构造群聊测试消息：`user-*` UID → User 角色（self），其余（含 None）→ Assistant 角色。
    fn msg(created_at: i64, persona_uid: Option<&str>) -> Message {
        let role = match persona_uid {
            Some(uid) if uid.starts_with("user-") => MessageRole::User,
            _ => MessageRole::Assistant,
        };
        let mut m = Message::new(
            uuid::Uuid::new_v4(),
            role,
            format!("msg@{}", created_at),
            MessageSource::Local,
        )
        .with_persona_uid(persona_uid.map(|s| s.to_string()));
        // Message::new 使用 now_ms()，测试需显式覆盖以模拟时间间隙/乱序场景
        m.created_at = created_at;
        m
    }

    const SELF: &str = "user-0001";
    const A: &str = "char-0002";
    const B: &str = "char-0003";

    fn cfg(gap_minutes: u32, max_count: u32) -> UttSplitterConfig {
        UttSplitterConfig {
            theta_gap_minutes: gap_minutes,
            max_msgs_per_block: max_count,
        }
    }

    // ---- 基础切分 ----

    #[test]
    fn empty_input_yields_empty() {
        assert!(split_messages_group(&[], &cfg(30, 40)).is_empty());
    }

    #[test]
    fn single_message_yields_single_chunk() {
        let msgs = vec![msg(1000, Some(A))];
        let chunks = split_messages_group(&msgs, &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 1);
        assert_eq!(chunks[0].time_span_ms, 0);
    }

    #[test]
    fn gap_exceeding_theta_splits_multi_speaker_blocks() {
        // 两块均含多个发言者（非单边）→ 按间隙切分后各自保留
        let mut msgs = vec![
            msg(0, Some(SELF)),
            msg(1000, Some(A)),
            msg(2000, Some(SELF)),
            msg(3000, Some(B)),
        ];
        let t2 = 31 * 60_000;
        msgs.push(msg(t2, Some(SELF)));
        msgs.push(msg(t2 + 1000, Some(A)));
        let chunks = split_messages_group(&msgs, &cfg(30, 40));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 4);
        assert_eq!(chunks[1].msg_count, 2);
    }

    #[test]
    fn gap_equal_to_theta_does_not_split() {
        // 间隙恰好 30 分钟 = θ_gap → 不切分（严格大于才切）
        let msgs = vec![
            msg(0, Some(SELF)),
            msg(1000, Some(A)),
            msg(1000 + 30 * 60_000, Some(A)),
            msg(2000 + 30 * 60_000, Some(SELF)),
        ];
        let chunks = split_messages_group(&msgs, &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 4);
    }

    #[test]
    fn out_of_order_input_is_sorted() {
        // 乱序输入（防御）：仍按时间切分
        let msgs = vec![
            msg(3000, Some(A)),
            msg(1000, Some(SELF)),
            msg(2000, Some(A)),
        ];
        let chunks = split_messages_group(&msgs, &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 3);
        assert_eq!(chunks[0].start_msg_id, msgs[1].id);
        assert_eq!(chunks[0].end_msg_id, msgs[0].id);
    }

    #[test]
    fn system_and_tool_messages_are_excluded() {
        let mut msgs = vec![msg(0, Some(SELF)), msg(60_000, Some(A))];
        msgs.push(Message::new(
            uuid::Uuid::new_v4(),
            MessageRole::System,
            "system".to_string(),
            MessageSource::Local,
        ));
        msgs.push(Message::new(
            uuid::Uuid::new_v4(),
            MessageRole::Tool,
            "tool".to_string(),
            MessageSource::Local,
        ));
        msgs.push(msg(120_000, Some(A)));
        let chunks = split_messages_group(&msgs, &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 3, "系统/工具消息不进入块");
    }

    // ---- 单边合并 ----

    #[test]
    fn single_speaker_block_merges_into_short_side() {
        // 纯 B 独白块两侧：前侧 1 分钟 < 后侧 2 小时 → 并入前块
        let mut msgs = vec![
            msg(0, Some(SELF)),
            msg(60_000, Some(A)),
            msg(120_000, Some(SELF)),
        ];
        msgs.push(msg(180_000, Some(B)));
        msgs.push(msg(240_000, Some(B)));
        let t = 240_000 + 2 * 3600 * 1000;
        msgs.push(msg(t, Some(SELF)));
        msgs.push(msg(t + 60_000, Some(A)));

        let chunks = split_messages_group(&msgs, &cfg(30, 3));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 5, "单边块并入时间短侧（前块）");
        assert_eq!(chunks[1].msg_count, 2);
    }

    #[test]
    fn single_speaker_first_block_merges_into_next() {
        // 首块纯 A（单边）→ 并入后块
        let msgs = vec![
            msg(0, Some(A)),
            msg(60_000, Some(A)),
            msg(120_000, Some(SELF)),
            msg(180_000, Some(B)),
        ];
        let chunks = split_messages_group(&msgs, &cfg(30, 2));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 4);
        assert_eq!(
            chunks[0].messages[0].persona_uid.as_deref(),
            Some(A),
            "首块消息位于合并块开头"
        );
    }

    #[test]
    fn equal_gap_merges_into_previous() {
        // 等距时并入前块（保持时间顺序）
        let gap = 600_001i64; // θ_gap=10 分钟（600_000ms），严格大于才切
        let mut msgs = vec![msg(1_000, Some(SELF)), msg(2_000, Some(A))];
        // 单边块（纯 B），与块1 末条间隔 = gap
        msgs.push(msg(2_000 + gap, Some(B)));
        msgs.push(msg(2_000 + gap + 60_000, Some(B)));
        // 后块（多发言者），与单边块末条间隔 = gap（等距）
        msgs.push(msg(2_000 + gap + 60_000 + gap, Some(SELF)));
        msgs.push(msg(2_000 + gap + 60_000 + gap + 60_000, Some(A)));

        let chunks = split_messages_group(&msgs, &cfg(10, 40));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 4, "等距时并入前块");
        assert_eq!(chunks[1].msg_count, 2);
        // 单边块消息位于合并后的前块末尾
        assert_eq!(chunks[0].messages[3].created_at, 2_000 + gap + 60_000);
    }

    // ---- 全保留（无目标发言过滤） ----

    #[test]
    fn self_only_blocks_are_kept_and_converge() {
        // 全部为 self 发言：单边块收敛为一块（不丢弃）
        let msgs: Vec<Message> = (0..6)
            .map(|i| msg(1_000_000 + i as i64 * 60_000, Some(SELF)))
            .collect();
        let chunks = split_messages_group(&msgs, &cfg(30, 2));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 6);
    }

    #[test]
    fn foreign_only_blocks_are_kept() {
        // 全部为同一他人发言（群聊单边）：收敛为一块，不丢弃
        let msgs: Vec<Message> = (0..4)
            .map(|i| msg(1_000_000 + i as i64 * 60_000, Some(A)))
            .collect();
        let chunks = split_messages_group(&msgs, &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 4);
    }

    #[test]
    fn max_count_splits_and_single_side_converges() {
        // 4 条 A 连续（上限 2 切出两块纯 A），随后多发言者块 → 收敛为一块
        let base = 1_000_000i64;
        let mut msgs: Vec<Message> = (0..4)
            .map(|i| msg(base + i as i64 * 60_000, Some(A)))
            .collect();
        msgs.push(msg(base + 4 * 60_000, Some(SELF)));
        msgs.push(msg(base + 5 * 60_000, Some(B)));

        let chunks = split_messages_group(&msgs, &cfg(30, 2));
        assert_eq!(chunks.len(), 1, "连续单边块收敛为一块");
        assert_eq!(chunks[0].msg_count, 6);
    }

    #[test]
    fn single_speaker_last_block_merges_back() {
        // 末尾纯 A 块（单边）仅有前侧 → 并入前块
        let mut msgs = vec![msg(0, Some(SELF)), msg(60_000, Some(B))];
        msgs.push(msg(120_000, Some(A)));
        msgs.push(msg(180_000, Some(A)));

        let chunks = split_messages_group(&msgs, &cfg(30, 2));
        assert_eq!(chunks.len(), 1, "末块单边并入前块");
        assert_eq!(chunks[0].msg_count, 4);
        assert_eq!(
            chunks[0].messages.last().unwrap().persona_uid.as_deref(),
            Some(A),
            "末块消息位于合并块末尾"
        );
    }
}
