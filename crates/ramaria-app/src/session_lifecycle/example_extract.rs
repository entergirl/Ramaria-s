//! crates/ramaria-app/src/session_lifecycle/example_extract.rs - 回复对抽取（examples 写侧）
//!
//! 设计特点:
//! - 纯函数模块：消息序列 → 回复对列表，无 IO、无状态、不调用 LLM
//! - 抽取范围（决策见 docs/dev-1.4/v1.4-decisions.md）: 仅"对方消息 → persona 回复"相邻对
//! - 过滤规则: 图片消息 / 回复过短（< 5 字符）/ 系统消息 / 批内重复对
//! - 每条回复对附带前文 context（最多 3 条）与话题 tags（CJK bigram 关键词），
//!   供注入时的话题匹配评分（example_selector）使用
//!
//! 配对规则:
//! - 用户消息后紧邻的第一条目标 persona 回复组成一对
//! - 连续多条用户消息 → 只与最后一条配对（覆盖前序）
//! - 系统/工具消息或非目标 assistant 消息中断配对（不是对用户的回复）

use ramaria_core::types::{Message, MessageRole};
use ramaria_memory::prompt::example_selector::extract_keywords;
use uuid::Uuid;

/// 图片消息占位符（导入器统一替换格式，见 importer/qq/parser.rs）。
const IMAGE_PLACEHOLDER: &str = "[图片]";

/// 抽取出的回复对（未入库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPair {
    /// 对方消息内容
    pub partner: String,
    /// persona 回复内容
    pub reply: String,
    /// 来源会话
    pub session_id: Uuid,
    /// 前文（partner 前最多 3 条对话消息，`角色: 内容` 行）
    pub context: Option<String>,
    /// 话题标签（逗号分隔，由 partner+reply 关键词提取）
    pub tags: String,
}

/// 判断消息是否为图片消息。
///
/// 说明:
/// - 导入器将图片统一替换为 `[图片]`（跨批次指纹一致）。
/// - 防御性同时识别 `[图片:` 前缀（历史版本可能残留带文件名的占位符）。
/// - 图片消息无文本风格信息，不作为 partner 或 reply。
pub fn is_image_message(msg: &Message) -> bool {
    msg.content.contains(IMAGE_PLACEHOLDER) || msg.content.contains("[图片:")
}

/// 从会话消息中抽取"对方消息 → persona 回复"相邻对。
///
/// 参数:
/// - `messages`: 会话消息（时间升序约定；内部防御性排序）。
/// - `target_persona_uid`: 目标 persona（"你"的回复归属）。
///
/// 返回:
/// - 抽取的回复对列表（时间升序，批内已按 partner+reply 去重）。
///
/// 过滤规则:
/// - 图片消息（partner 与 reply 均排除）。
/// - reply 字符数 < 5（过短无风格信息）。
/// - 系统/工具消息不参与配对，且中断待配对状态。
/// - 非目标 persona 的 assistant 消息中断待配对状态。
/// - 批内重复对（相同 partner+reply）只保留第一条。
///
/// 边界:
/// - 空输入 / 无目标回复 → 空列表。
/// - 消息乱序 → 按 created_at 稳定排序后处理。
/// - 用户消息后无 persona 回复（会话结尾）→ 丢弃该 partner。
pub fn extract_pairs(messages: &[Message], target_persona_uid: &str) -> Vec<ExtractedPair> {
    // 防御：时间升序稳定排序（输入约定升序，导入等场景可能乱序）
    let mut ordered: Vec<Message> = messages.to_vec();
    ordered.sort_by_key(|m| m.created_at);

    let mut pairs: Vec<ExtractedPair> = Vec::new();
    // 待配对的上一条用户消息
    let mut pending_partner: Option<Message> = None;
    // 最近 3 条对话消息窗口（不含系统/工具/图片消息，供 context 使用）
    let mut context_window: Vec<Message> = Vec::with_capacity(3);
    // 批内去重（partner+reply）
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for m in ordered {
        match m.role {
            MessageRole::System | MessageRole::Tool => {
                // 系统消息中断配对（persona 对系统消息的"回复"不是对用户的回复）
                pending_partner = None;
                // 不进 context 窗口
                continue;
            }
            MessageRole::User => {
                if is_image_message(&m) || m.content.trim().is_empty() {
                    // 图片/空用户消息：中断配对（不能作为 partner）
                    pending_partner = None;
                } else {
                    // 连续多条用户消息 → 覆盖前序（只与最后一条配对）
                    pending_partner = Some(m.clone());
                }
                push_window(&mut context_window, &m);
            }
            MessageRole::Assistant => {
                if m.persona_uid.as_deref() == Some(target_persona_uid) {
                    // 目标 persona 回复：与待配对用户消息组成一对
                    if let Some(partner) = pending_partner.take()
                        && !is_image_message(&m)
                        && m.content.trim().chars().count() >= 5
                        && !m.content.trim().is_empty()
                    {
                        let key = (partner.content.clone(), m.content.clone());
                        if seen.insert(key) {
                            pairs.push(build_pair(&partner, &m, &context_window));
                        }
                    }
                    push_window(&mut context_window, &m);
                } else {
                    // 非目标 persona 的回复：中断配对（不是对用户的回复）
                    pending_partner = None;
                    push_window(&mut context_window, &m);
                }
            }
            // 防御：未知角色（non_exhaustive 枚举未来扩展）按中断处理
            _ => {
                pending_partner = None;
            }
        }
    }

    pairs
}

/// 将消息推入 context 窗口（仅对话消息，裁剪到最近 3 条）。
fn push_window(window: &mut Vec<Message>, msg: &Message) {
    if is_image_message(msg) {
        return; // 图片占位符无背景价值
    }
    window.push(msg.clone());
    if window.len() > 3 {
        window.remove(0);
    }
}

/// 构建回复对（含 context 与 tags）。
fn build_pair(partner: &Message, reply: &Message, context_window: &[Message]) -> ExtractedPair {
    let context = if context_window.is_empty() {
        None
    } else {
        let lines: Vec<String> = context_window
            .iter()
            .map(|m| format!("{}: {}", role_label(m), m.content.trim()))
            .collect();
        Some(lines.join("\n"))
    };

    let tags = extract_keywords(&format!("{} {}", partner.content, reply.content)).join(",");

    ExtractedPair {
        partner: partner.content.trim().to_string(),
        reply: reply.content.trim().to_string(),
        session_id: reply.session_id,
        context,
        tags,
    }
}

/// context 行使用的角色标签。
fn role_label(msg: &Message) -> &'static str {
    match msg.role {
        MessageRole::User => "用户",
        _ => "你",
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "char-0001";

    fn msg(role: MessageRole, persona_uid: Option<&str>, content: &str, t: i64) -> Message {
        let mut m = Message::new(
            Uuid::new_v4(),
            role,
            content.to_string(),
            ramaria_core::types::MessageSource::Local,
        )
        .with_persona_uid(persona_uid.map(|s| s.to_string()));
        // Message::new 使用 now_ms()，测试需显式覆盖以模拟时间序（乱序/间隙场景）
        m.created_at = t;
        m
    }

    fn user(content: &str, t: i64) -> Message {
        msg(MessageRole::User, None, content, t)
    }

    fn reply(content: &str, t: i64) -> Message {
        msg(MessageRole::Assistant, Some(TARGET), content, t)
    }

    /// 断言存在一对 (partner, reply)。
    fn assert_has_pair(pairs: &[ExtractedPair], partner: &str, reply: &str) {
        assert!(
            pairs
                .iter()
                .any(|p| p.partner == partner && p.reply == reply),
            "应包含回复对 ({partner}) → ({reply})，实际: {pairs:?}"
        );
    }

    // ---- 基础抽取 ----

    #[test]
    fn empty_input_yields_empty() {
        assert!(extract_pairs(&[], TARGET).is_empty());
    }

    #[test]
    fn single_pair_extracted() {
        let msgs = vec![
            user("今天天气真好呀", 1000),
            reply("是啊，我们出去走走吧！", 2000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1);
        assert_has_pair(&pairs, "今天天气真好呀", "是啊，我们出去走走吧！");
    }

    #[test]
    fn no_target_reply_yields_empty() {
        let msgs = vec![
            user("你好呀", 1000),
            msg(
                MessageRole::Assistant,
                Some("char-9999"),
                "我是另一个角色",
                2000,
            ),
        ];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    #[test]
    fn user_message_without_reply_is_dropped() {
        // 会话结尾的用户消息没有回复 → 丢弃
        let msgs = vec![
            user("第一条消息", 1000),
            reply("第一条回复", 2000),
            user("没有回复的尾巴", 3000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn consecutive_user_messages_pair_with_last_one() {
        let msgs = vec![
            user("第一条用户消息", 1000),
            user("第二条用户消息", 2000),
            reply("回复第二条", 3000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1);
        assert_has_pair(&pairs, "第二条用户消息", "回复第二条");
    }

    #[test]
    fn system_message_breaks_pairing() {
        // 系统消息后 persona 的"回复"不是对用户的回复 → 不配对
        let msgs = vec![
            user("用户问题", 1000),
            msg(MessageRole::System, None, "系统注入内容", 2000),
            reply("对系统内容的回应", 3000),
        ];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    #[test]
    fn foreign_assistant_breaks_pairing() {
        let msgs = vec![
            user("用户问题", 1000),
            msg(
                MessageRole::Assistant,
                Some("char-9999"),
                "其他角色插话",
                2000,
            ),
            reply("目标角色回复", 3000),
        ];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    #[test]
    fn tool_message_breaks_pairing() {
        let msgs = vec![
            user("用户问题", 1000),
            msg(MessageRole::Tool, None, "工具调用结果", 2000),
            reply("目标角色回复", 3000),
        ];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    // ---- 过滤规则 ----

    #[test]
    fn image_partner_is_filtered() {
        let msgs = vec![
            user("[图片]", 1000),
            user("这张照片好看吗？", 2000),
            reply("好看呀，构图很棒！", 3000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        // 图片消息不配对；后续用户消息正常配对
        assert_eq!(pairs.len(), 1);
        assert_has_pair(&pairs, "这张照片好看吗？", "好看呀，构图很棒！");
    }

    #[test]
    fn image_reply_is_filtered() {
        let msgs = vec![user("发张照片看看", 1000), reply("[图片]", 2000)];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    #[test]
    fn short_reply_is_filtered() {
        // reply < 5 字符 → 丢弃
        let msgs = vec![user("你好吗？", 1000), reply("嗯", 2000)];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    #[test]
    fn five_char_reply_is_kept() {
        // 边界：恰好 5 字符保留（挺/好/的/呀/！）
        let msgs = vec![user("你好吗？", 1000), reply("挺好的呀！", 2000)];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn blank_partner_is_filtered() {
        let msgs = vec![user("   ", 1000), reply("你好呀朋友", 2000)];
        assert!(extract_pairs(&msgs, TARGET).is_empty());
    }

    #[test]
    fn duplicate_pairs_deduplicated_within_batch() {
        // 批内去重：相同 partner+reply 只保留一条
        let msgs = vec![
            user("同一个问题", 1000),
            reply("同一个回答", 2000),
            user("同一个问题", 3000),
            reply("同一个回答", 4000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1, "重复对只保留一条");
    }

    // ---- 附属信息 ----

    #[test]
    fn context_captures_previous_messages() {
        let msgs = vec![
            user("第一句", 1000),
            reply("第一句回复", 2000),
            user("第二句问题", 3000),
            reply("第二句回复", 4000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 2);
        let second = &pairs[1];
        let ctx = second.context.as_deref().expect("第二对有前文");
        assert!(
            ctx.contains("用户: 第二句问题"),
            "前文含 partner 前一条消息: {ctx}"
        );
        assert!(ctx.contains("你: 第一句回复"), "前文含更早的回复: {ctx}");
        assert!(
            ctx.lines().all(|l| l.contains(':')),
            "每行都应有角色标注: {ctx}"
        );
    }

    #[test]
    fn context_limited_to_three_messages() {
        let mut msgs = Vec::new();
        for i in 0..6 {
            msgs.push(user(&format!("用户第{i}句"), i * 1000));
            msgs.push(reply(&format!("回复第{i}句内容"), i * 1000 + 500));
        }
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 6);
        let last = &pairs[5];
        let ctx = last.context.as_deref().unwrap();
        let lines = ctx.lines().count();
        assert!(lines <= 3, "前文最多 3 条，实际 {lines}");
    }

    #[test]
    fn tags_extracted_from_partner_and_reply() {
        let msgs = vec![
            user("今天去公园散步吧", 1000),
            reply("好呀，天气这么好正适合！", 2000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        let tags = &pairs[0].tags;
        assert!(tags.contains("公园"), "tags 应含话题关键词: {tags}");
        assert!(!tags.is_empty());
    }

    #[test]
    fn context_skips_image_and_system_messages() {
        let msgs = vec![
            user("[图片]", 1000),
            msg(MessageRole::System, None, "系统注入", 1500),
            user("真正的问题", 2000),
            reply("真正的回答内容", 3000),
        ];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1);
        let ctx = pairs[0].context.as_deref().unwrap_or("");
        assert!(!ctx.contains("系统注入"), "系统消息不进 context");
        assert!(!ctx.contains("[图片]"), "图片消息不进 context");
    }

    // ---- 防御 ----

    #[test]
    fn out_of_order_messages_are_sorted() {
        let msgs = vec![reply("回复内容在前面", 3000), user("这个问题很重要", 1000)];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1, "乱序输入按时间排序后配对");
        assert_has_pair(&pairs, "这个问题很重要", "回复内容在前面");
    }

    #[test]
    fn is_image_message_detects_placeholder() {
        assert!(is_image_message(&user("[图片]", 1)));
        assert!(is_image_message(&user("[图片: abc123.jpg]", 1)));
        assert!(!is_image_message(&user("正常文本", 1)));
    }

    #[test]
    fn session_id_propagated() {
        let msgs = vec![user("问题内容很详细", 1000), reply("回答内容很详细", 2000)];
        let pairs = extract_pairs(&msgs, TARGET);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].session_id, msgs[1].session_id);
    }
}
