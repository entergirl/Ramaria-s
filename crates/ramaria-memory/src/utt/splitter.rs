//! crates/ramaria-memory/src/utt/splitter.rs - utt 话语块切分器
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
//! - 单边块不独立存在，按**时间间隔更短的一侧**并入相邻块（决策记录见 docs/dev-1.5/v1.5-decisions.md；算法见 v3.1 §7.1）：
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

    // ---- 单边合并（收敛循环：按时间间隔更短的一侧并入相邻块） ----
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
mod tests;
