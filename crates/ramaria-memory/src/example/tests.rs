//! crates/ramaria-memory/src/example/tests.rs - 示例回复对抽取单元测试
//!
//! 设计特点:
//! - 位于 example 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由父文件以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
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

/// 主动消息口径：前有用户消息时照常配对；无前序用户消息时不配对（零来源过滤）。
#[test]
fn proactive_reply_pairs_only_after_user_message() {
    // 前有 user → 主动消息与普通回复同口径配对
    let mut proactive = reply("主动问候一下最近还好吗", 2000);
    proactive.is_proactive = true;
    let pairs = extract_pairs(&[user("在吗", 1000), proactive], TARGET);
    assert_eq!(pairs.len(), 1, "前有用户消息时主动消息应正常配对");
    assert_has_pair(&pairs, "在吗", "主动问候一下最近还好吗");

    // 无前序 user（主动消息开场）→ 不配对（锁定"仅前有 user 才配对"现有规则）
    let mut opening = reply("今天过得怎么样呀", 1000);
    opening.is_proactive = true;
    assert!(
        extract_pairs(&[opening], TARGET).is_empty(),
        "无前序用户消息的主动消息不应产出回复对"
    );
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
    assert!(
        is_image_message(&user("[图片#abcd1234]", 1)),
        "带指纹占位符应判定为图片消息"
    );
    assert!(is_image_message(&user("看这个 [图片#abcd1234] 好看吗", 1)));
    assert!(!is_image_message(&user("正常文本", 1)));
}

/// 带指纹占位符的图片消息同样不参与配对与 context。
#[test]
fn hash_image_message_is_filtered_from_pairs() {
    let msgs = vec![
        user("[图片#abcd1234]", 1000),
        user("这张照片好看吗？", 2000),
        reply("好看呀，构图很棒！", 3000),
    ];
    let pairs = extract_pairs(&msgs, TARGET);
    assert_eq!(pairs.len(), 1, "带指纹图片消息不配对");
    assert_has_pair(&pairs, "这张照片好看吗？", "好看呀，构图很棒！");

    let msgs = vec![user("发张照片看看", 1000), reply("[图片#abcd1234]", 2000)];
    assert!(extract_pairs(&msgs, TARGET).is_empty(), "图片回复不配对");
}

#[test]
fn session_id_propagated() {
    let msgs = vec![user("问题内容很详细", 1000), reply("回答内容很详细", 2000)];
    let pairs = extract_pairs(&msgs, TARGET);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].session_id, msgs[1].session_id);
}

#[test]
fn stats_total_is_sum() {
    let stats = ExampleSaveStats {
        saved: 3,
        skipped: 2,
    };
    assert_eq!(stats.total(), 5);
}
