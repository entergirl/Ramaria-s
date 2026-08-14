//! rust/crates/ramaria-memory/src/utt/splitter.rs - utt 话语块切分器
//!
//! 设计特点:
//! - 纯函数模块：输入消息序列 + 配置 → 输出话语块，无 IO、无状态
//! - 三种切分规则（v3.1 §7.1 / §10）:
//!   1. 时间间隙切分：相邻消息间隔 > θ_gap 分钟 → 新块
//!   2. 条数上限切分：块内消息达到 max_msgs_per_block → 新块
//!   3. 块内必须含目标 persona 发言；单边块（只有一边发言）与相邻块合并
//! - 系统/工具消息不进入块（不是对话原文）
//! - 输入约定时间升序；防御性按 created_at 稳定排序
//!
//! 合并规则说明:
//! - "单边"指块内消息全部来自同一发言侧（全为目标发言或全非目标发言）。
//! - 单边块不独立存在，按**时间间隔更短的一侧**并入相邻块（v1.5 D-V15-014）：
//!   比较单边块首条与前块末条的间隔、后块首条与单边块末条的间隔，取短侧；
//!   首块仅后侧、末块仅前侧；等距时并入前块（保持时间顺序，兼容旧行为）。
//!   目的：提问型独白近回复侧 → 并入后块（问答配对同块）；收尾型独白
//!   （如"晚安"）近旧话题侧 → 并入前块末尾（意义连贯），无需语义判断。
//! - 合并循环收敛：每次合并减少一块，最多 n-1 次；单边块并入后若仍单边继续合并。
//! - 合并可突破 θ_gap 与条数上限（合并优先于上限，注释约定）。
//! - 最终仍不含目标 persona 发言的块丢弃（纯用户消息无注入价值）。

use ramaria_core::types::Message;

use super::{UttChunk, UttSplitterConfig, is_chat_message, is_target_speech};

/// 将消息序列切分为话语块。
///
/// 参数:
/// - `messages`: 会话消息（时间升序约定；内部防御性排序）。
/// - `target_persona_uid`: 目标 persona（块内必须含其发言）。None 表示 rama 自身会话。
/// - `config`: 切分配置（θ_gap / 条数上限）。
///
/// 返回:
/// - 话语块列表（时间升序，每块必含目标 persona 发言）。
/// - 无任何目标发言（如全会话只有用户消息）时返回空列表。
///
/// 边界:
/// - 空输入 → 空输出。
/// - 单条消息 → 单个块。
/// - 间隙恰好等于 θ_gap 分钟 → 不切分（严格大于才切）。
pub fn split_messages(
    messages: &[Message],
    target_persona_uid: Option<&str>,
    config: &UttSplitterConfig,
) -> Vec<UttChunk> {
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
                candidates.push(build_chunk(std::mem::take(&mut current)));
            }
        }
        current.push(m);
    }
    if !current.is_empty() {
        candidates.push(build_chunk(current));
    }

    // ---- 单边合并（收敛循环，v1.5 D-V15-014：按时间间隔更短的一侧并入） ----
    let mut chunks = candidates;
    let mut i = 0;
    while i < chunks.len() {
        if !is_single_side(&chunks[i], target_persona_uid) {
            i += 1;
            continue;
        }

        // 单边块：两侧间隔比较，取短侧并入（无需语义判断）
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

    // ---- 丢弃不含目标 persona 发言的块 ----
    chunks
        .into_iter()
        .filter(|c| {
            c.messages
                .iter()
                .any(|m| is_target_speech(m, target_persona_uid))
        })
        .collect()
}

/// 块内是否"只有一边发言"（全为目标发言或全非目标发言）。
fn is_single_side(chunk: &UttChunk, target_persona_uid: Option<&str>) -> bool {
    let mut seen_target = false;
    let mut seen_other = false;
    for m in &chunk.messages {
        if is_target_speech(m, target_persona_uid) {
            seen_target = true;
        } else {
            seen_other = true;
        }
        if seen_target && seen_other {
            return false;
        }
    }
    // 空块按单边处理（防御：调用方不会产生空块）
    seen_target || seen_other
}

/// 由消息序列构建 UttChunk（start/end/msg_count/time_span_ms 计算）。
fn build_chunk(messages: Vec<Message>) -> UttChunk {
    let first = messages.first().expect("块非空");
    let last = messages.last().expect("块非空");
    UttChunk {
        start_msg_id: first.id,
        end_msg_id: last.id,
        msg_count: messages.len() as u32,
        time_span_ms: last.created_at - first.created_at,
        messages,
    }
}

/// 将 `other` 追加到 `base` 末尾（单边合并用）。
fn merge_back(base: UttChunk, other: UttChunk) -> UttChunk {
    let mut messages = base.messages;
    messages.extend(other.messages);
    build_chunk(messages)
}

/// 将 `other` 插入到 `base` 开头（首块并入后块用）。
fn merge_front(base: UttChunk, other: UttChunk) -> UttChunk {
    let mut messages = other.messages;
    messages.extend(base.messages);
    build_chunk(messages)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{MessageRole, MessageSource};

    /// 构造测试消息：persona_uid 为 Some(uid) 表示 target 发言，None 表示用户。
    fn msg(created_at: i64, persona_uid: Option<&str>) -> Message {
        let mut m = Message::new(
            uuid::Uuid::new_v4(),
            if persona_uid.is_some() {
                MessageRole::Assistant
            } else {
                MessageRole::User
            },
            format!("msg@{}", created_at),
            MessageSource::Local,
        )
        .with_persona_uid(persona_uid.map(|s| s.to_string()));
        // Message::new 使用 now_ms()，测试需显式覆盖以模拟时间间隙/乱序场景
        m.created_at = created_at;
        m
    }

    /// 连续消息序列（间隔 1 分钟），target 发言穿插。
    fn continuous(target: &str, count: usize) -> Vec<Message> {
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            // 偶数条 target 发言，奇数条用户发言 → 交替
            let uid = if i % 2 == 0 { Some(target) } else { None };
            out.push(msg(1_000_000 + i as i64 * 60_000, uid));
        }
        out
    }

    const TARGET: &str = "char-0001";

    fn cfg(gap_minutes: u32, max_count: u32) -> UttSplitterConfig {
        UttSplitterConfig {
            theta_gap_minutes: gap_minutes,
            max_msgs_per_block: max_count,
        }
    }

    // ---- 基础切分 ----

    #[test]
    fn empty_input_yields_empty() {
        assert!(split_messages(&[], Some(TARGET), &cfg(30, 40)).is_empty());
    }

    #[test]
    fn single_message_yields_single_chunk() {
        let msgs = vec![msg(1000, Some(TARGET))];
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 1);
        assert_eq!(chunks[0].time_span_ms, 0);
    }

    #[test]
    fn no_gap_yields_single_chunk() {
        let msgs = continuous(TARGET, 10);
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 10);
    }

    #[test]
    fn gap_exceeding_theta_splits() {
        // 前 3 条连续，然后跳 60 分钟（> 30），再 3 条
        let mut msgs = continuous(TARGET, 3);
        let t = msgs.last().unwrap().created_at + 60 * 60_000;
        msgs.push(msg(t, Some(TARGET)));
        msgs.push(msg(t + 60_000, None));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 3);
        assert_eq!(chunks[1].msg_count, 2);
    }

    #[test]
    fn gap_equal_to_theta_does_not_split() {
        // 间隙恰好 30 分钟 = θ_gap → 不切分（严格大于才切）
        let mut msgs = continuous(TARGET, 2);
        let t = msgs.last().unwrap().created_at + 30 * 60_000;
        msgs.push(msg(t, Some(TARGET)));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 3);
    }

    #[test]
    fn max_count_splits() {
        let msgs = continuous(TARGET, 9);
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 4));
        // 9 条 / 每块 4 条 → 候选 4+4+1；末块单条 target 单边 → 并入第二块 → 2 块
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 4);
        assert_eq!(chunks[1].msg_count, 5, "单边末块并入相邻块");
    }

    #[test]
    fn gap_and_count_combined() {
        // 上限 4 + 中间大间隙 → 间隙优先切分
        let mut msgs = continuous(TARGET, 4);
        let t = msgs.last().unwrap().created_at + 60 * 60_000;
        msgs.push(msg(t, Some(TARGET)));
        msgs.push(msg(t + 60_000, None));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 4));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 4);
        assert_eq!(chunks[1].msg_count, 2);
    }

    #[test]
    fn out_of_order_input_is_sorted() {
        // 乱序输入（防御）：仍按时间切分
        let msgs = vec![
            msg(3000, Some(TARGET)),
            msg(1000, None),
            msg(2000, Some(TARGET)),
        ];
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 3);
        assert_eq!(chunks[0].start_msg_id, msgs[1].id);
        assert_eq!(chunks[0].end_msg_id, msgs[0].id);
    }

    #[test]
    fn system_and_tool_messages_are_excluded() {
        let mut msgs = continuous(TARGET, 2);
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
        msgs.push(msg(msgs[1].created_at + 60_000, Some(TARGET)));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 3, "系统/工具消息不进入块");
    }

    // ---- 单边合并 ----

    #[test]
    fn single_side_block_merges_into_previous() {
        // 块结构：[双方 3 条] [纯 target 2 条]（2 小时间隙分隔）[双方 2 条]
        // 中间块（纯 target）单边 → 前侧间隔 1 分钟 < 后侧间隔 2 小时 → 并入前一块
        let mut msgs = continuous(TARGET, 3);
        let t = msgs.last().unwrap().created_at + 60_000;
        msgs.push(msg(t, Some(TARGET)));
        msgs.push(msg(t + 60_000, Some(TARGET)));
        // 2 小时间隙（> θ_gap）→ 后续消息开新块，块2 保持纯 target
        let t5 = msgs.last().unwrap().created_at + 2 * 3600 * 1000;
        msgs.push(msg(t5, None));
        msgs.push(msg(t5 + 60_000, Some(TARGET)));

        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 3));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 5, "单边块并入前一块（时间短侧）");
        assert_eq!(chunks[1].msg_count, 2);
    }

    #[test]
    fn single_side_first_block_merges_into_next() {
        // 首块纯 target（用户未发言）→ 并入后一块
        let mut msgs = vec![msg(1000, Some(TARGET)), msg(60_000, Some(TARGET))];
        msgs.push(msg(120_000, None));
        msgs.push(msg(180_000, Some(TARGET)));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 2));
        assert_eq!(chunks.len(), 1, "首块单边并入后块");
        assert_eq!(chunks[0].msg_count, 4);
    }

    #[test]
    fn consecutive_single_side_blocks_converge() {
        // 连续两块纯 target，再跟双方块 → 最终并入一块
        let mut msgs = vec![
            msg(1000, Some(TARGET)),
            msg(60_000, Some(TARGET)),
            msg(120_000, Some(TARGET)),
            msg(180_000, Some(TARGET)),
        ];
        msgs.push(msg(240_000, None));
        msgs.push(msg(300_000, Some(TARGET)));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 2));
        assert_eq!(chunks.len(), 1, "连续单边块收敛为一块");
        assert_eq!(chunks[0].msg_count, 6);
    }

    #[test]
    fn all_target_speech_keeps_single_block() {
        // 全会话只有 target 发言：合并后仍单边，保留单块
        let msgs: Vec<Message> = (0..6)
            .map(|i| msg(1_000_000 + i as i64 * 60_000, Some(TARGET)))
            .collect();
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 2));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 6);
    }

    #[test]
    fn all_user_messages_yield_empty() {
        // 全会话只有用户消息：无目标发言，全部丢弃
        let msgs: Vec<Message> = (0..6).map(|i| msg(1000 + i * 60_000, None)).collect();
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 2));
        assert!(chunks.is_empty());
    }

    #[test]
    fn merge_can_exceed_max_count() {
        // 单边合并优先于条数上限：合并后块可超过 max_msgs_per_block
        let msgs: Vec<Message> = (0..8)
            .map(|i| msg(1000 + i as i64 * 60_000, Some(TARGET)))
            .collect();
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 3));
        // 8 条全 target → 候选 3+3+2 全单边 → 合并收敛为一块（8 > 上限 3）
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 8, "合并突破上限但保留完整消息");
    }

    // ---- 目标 persona 判定边界 ----

    #[test]
    fn other_persona_speech_counts_as_other_side() {
        // 其他 persona 的 assistant 消息不是目标发言（按 uid 精确匹配）
        let msgs = vec![
            msg(1000, Some(TARGET)),
            msg(60_000, Some("char-9999")),
            msg(120_000, Some(TARGET)),
        ];
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 2));
        // 上限 2：块1 [target, 其他]（双方 → 不合并）、块2 [target]（单边 → 并入前块）
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 3);
    }

    #[test]
    fn none_target_session_uses_no_persona_uid() {
        // rama 自身会话（target=None）：无 persona_uid 的 assistant 消息算目标发言
        // 注意：splitter 测试的 msg helper 将 None → User 角色；
        // 这里显式构造 Assistant + 无 uid（rama 自身发言的表示）
        fn asst_no_uid(t: i64) -> Message {
            let mut m = Message::new(
                uuid::Uuid::new_v4(),
                MessageRole::Assistant,
                format!("rama@{t}"),
                MessageSource::Local,
            );
            m.created_at = t;
            m
        }
        let msgs = vec![
            asst_no_uid(1000),
            msg(60_000, Some("char-0001")), // 其他 persona：非目标
            asst_no_uid(120_000),
        ];
        let chunks = split_messages(&msgs, None, &cfg(30, 40));
        assert_eq!(chunks.len(), 1, "含无 uid 发言 → 保留");
        assert_eq!(chunks[0].msg_count, 3);
    }

    #[test]
    fn none_target_all_foreign_speech_yields_empty() {
        let msgs = vec![msg(1000, Some("char-0001")), msg(60_000, Some("char-0002"))];
        let chunks = split_messages(&msgs, None, &cfg(30, 40));
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_side_user_block_at_end_merges_back() {
        // 末尾纯用户块并入前一块
        let mut msgs = continuous(TARGET, 3);
        msgs.push(msg(msgs.last().unwrap().created_at + 60_000, None));
        msgs.push(msg(msgs.last().unwrap().created_at + 60_000, None));
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 3));
        // 末块（纯用户）单边 → 并入前块 → 一块 5 条
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].msg_count, 5);
    }

    // ---- v1.5 D-V15-014：单边合并方向 = 时间间隔更短的一侧 ----

    /// 隔夜提问独白并入回复侧（后块）：
    /// 用户深夜提问（纯用户块）与次日回复间隔近、与旧话题块间隔远 → 并入回复块。
    #[test]
    fn overnight_question_merges_into_reply_side() {
        // 块1：白天双方对话（3 条，间隔 1 分钟）
        let mut msgs = continuous(TARGET, 3);
        // 深夜：用户独白"在吗？"（纯用户块），与块1 末条间隔 8 小时
        let night = msgs.last().unwrap().created_at + 8 * 3600 * 1000;
        msgs.push(msg(night, None));
        // 次日清晨：回复（双方块），与提问间隔仅 10 分钟
        let morning = night + 10 * 60_000;
        msgs.push(msg(morning, None));
        msgs.push(msg(morning + 60_000, Some(TARGET)));

        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        // 候选：[双方块] [纯用户提问] [双方回复]；提问块近回复侧 → 并入后块
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 3, "旧话题块独立保留");
        assert_eq!(chunks[1].msg_count, 3, "提问并入回复块（问答同块）");
        assert_eq!(
            chunks[1].messages[0].created_at, night,
            "提问消息应位于回复块开头"
        );
    }

    /// 收尾型独白并入旧话题侧（前块）：
    /// "晚安"独白紧邻旧话题块、远离下次对话 → 并入前块末尾（意义连贯）。
    #[test]
    fn closing_monologue_merges_into_old_topic_side() {
        // 块1：双方对话（3 条）
        let mut msgs = continuous(TARGET, 3);
        // target 收尾独白："晚安"（纯 target），紧接块1 末条
        let close = msgs.last().unwrap().created_at + 60_000;
        msgs.push(msg(close, Some(TARGET)));
        // 下次对话（双方），与收尾独白间隔 10 小时
        let next = close + 10 * 3600 * 1000;
        msgs.push(msg(next, None));
        msgs.push(msg(next + 60_000, Some(TARGET)));

        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        // 候选：[双方块] [纯 target 收尾] [下次对话]；收尾近旧话题侧 → 并入前块
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 4, "收尾独白并入前块末尾");
        assert_eq!(chunks[1].msg_count, 2, "下次对话独立成块");
        assert_eq!(
            chunks[0].messages.last().unwrap().created_at,
            close,
            "收尾消息应位于前块末尾"
        );
    }

    /// 条数上限切出的单边块按时间短侧回切相邻块：
    /// 上限切分使纯 target 块独立，其与相邻块的间隔决定回切方向。
    #[test]
    fn single_side_split_by_count_merges_to_short_side() {
        // 连续 6 条交替消息（上限 2）：候选 [t,u] [t,u] [t,u] 均双方 → 无单边
        // 构造：4 条 target 连续（上限 2 切出两块纯 target），随后紧跟双方块
        let base = 1_000_000i64;
        let mut msgs: Vec<Message> = (0..4)
            .map(|i| msg(base + i as i64 * 60_000, Some(TARGET)))
            .collect();
        // 双方块：紧接纯 target 块（间隔 1 分钟）
        msgs.push(msg(base + 4 * 60_000, None));
        msgs.push(msg(base + 5 * 60_000, Some(TARGET)));

        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 2));
        // 候选：[t,t] [t,t] [u,t]；块1 首块并入后块 → [t,t,t,t] [u,t]
        // 合并块仍纯 target 单边 → 唯一相邻侧为后块 → 并入 → 一块 6 条
        assert_eq!(chunks.len(), 1, "连续单边块收敛为一块");
        assert_eq!(chunks[0].msg_count, 6);
    }

    /// 等距时并入前块（保持时间顺序，兼容旧行为）。
    ///
    /// 构造：三块之间间隙相同（均 > θ_gap），中间为纯 target 单边块，
    /// 前侧间隔 == 后侧间隔 → 按 `<=` 判定并入前块。
    #[test]
    fn equal_gap_merges_into_previous() {
        let gap = 600_001i64; // θ_gap=10 分钟（600_000ms），严格大于才切
        // 块1：双方对话
        let mut msgs = vec![msg(1_000, None), msg(2_000, Some(TARGET))];
        // 块2：纯 target 单边块（与块1 末条间隔 = gap）
        msgs.push(msg(2_000 + gap, Some(TARGET)));
        msgs.push(msg(2_000 + gap + 60_000, Some(TARGET)));
        // 块3：双方块（与块2 末条间隔 = gap，等距）
        msgs.push(msg(2_000 + gap + 60_000 + gap, None));
        msgs.push(msg(2_000 + gap + 60_000 + gap + 60_000, Some(TARGET)));

        let chunks = split_messages(&msgs, Some(TARGET), &cfg(10, 40));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].msg_count, 4, "等距时并入前块");
        assert_eq!(chunks[1].msg_count, 2);
        // 单边块消息位于合并后的前块末尾
        assert_eq!(chunks[0].messages[3].created_at, 2_000 + gap + 60_000);
    }

    #[test]
    fn boundary_gap_negative_clock_does_not_split() {
        // 防御：消息时间倒挂（乱序防御已排序，此测试验证排序后不产生负 gap 崩溃）
        let mut msgs = vec![
            msg(5000, Some(TARGET)),
            msg(1000, None),
            msg(2000, Some(TARGET)),
        ];
        msgs.sort_by_key(|m| m.created_at);
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].time_span_ms, 4000);
    }

    #[test]
    fn chunk_metadata_correct() {
        let msgs = vec![
            msg(1000, Some(TARGET)),
            msg(60_000, None),
            msg(120_000, Some(TARGET)),
        ];
        let chunks = split_messages(&msgs, Some(TARGET), &cfg(30, 40));
        assert_eq!(chunks.len(), 1);
        let c = &chunks[0];
        assert_eq!(c.start_msg_id, msgs[0].id);
        assert_eq!(c.end_msg_id, msgs[2].id);
        assert_eq!(c.msg_count, 3);
        assert_eq!(c.time_span_ms, 119_000);
    }
}
