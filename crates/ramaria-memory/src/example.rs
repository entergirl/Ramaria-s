//! crates/ramaria-memory/src/example.rs - 回复对抽取与入库（examples 写侧）
//!
//! 设计特点:
//! - 纯函数抽取 + 存储编排两层：`extract_pairs` 零 I/O；`extract_and_save_for_session`
//!   负责会话读取、归属推断、查重与入库（同一份实现供在线管线封存与服务层封存共用）
//! - 抽取范围: 仅"对方消息 → 目标 persona 回复"相邻对（连续多条用户消息只与最后一条配对）
//! - 过滤规则: 图片消息 / 回复过短（< 5 字符）/ 系统与工具消息 / 批内重复对
//! - 每条回复对附带前文 context（最多 3 条）与话题 tags（CJK bigram 关键词），
//!   供注入时的话题匹配评分（example_selector）使用
//! - 幂等: 入库前按 (persona_uid, partner, reply) 查重，重复回复对不重复入库
//! - 隐私: 日志只记计数，不记对话原文

use ramaria_core::config::ExamplesConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, MessageRole, PersonaExample};
use uuid::Uuid;

use crate::prompt::example_selector::extract_keywords;

/// 图片消息占位符（导入器统一替换格式，见 importer/qq/parser.rs）。
const IMAGE_PLACEHOLDER: &str = "[图片]";

// =========================================================
// 抽取结果
// =========================================================

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

/// 单次封存抽取入库的统计（供日志聚合）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExampleSaveStats {
    /// 实际入库的回复对数
    pub saved: usize,
    /// 因查重跳过的回复对数
    pub skipped: usize,
}

impl ExampleSaveStats {
    /// 本次抽取到的回复对总数（入库 + 跳过）。
    pub fn total(&self) -> usize {
        self.saved + self.skipped
    }
}

// =========================================================
// 纯函数抽取
// =========================================================

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
                    let partner = pending_partner.take();
                    if let Some(partner) = partner {
                        let reply_ok = !is_image_message(&m)
                            && !m.content.trim().is_empty()
                            && m.content.trim().chars().count() >= 5;
                        if reply_ok {
                            let key = (partner.content.clone(), m.content.clone());
                            if seen.insert(key) {
                                pairs.push(build_pair(&partner, &m, &context_window));
                            }
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
// 存储编排（封存钩子入口）
// =========================================================

/// 抽取指定会话的回复对并查重入库（封存钩子入口，幂等）。
///
/// 流程:
/// 1. `[examples].enabled=false` → 跳过（行为回退旧版）；
/// 2. 读取会话；归属取 `sessions.persona_uid`，为 NULL 时从消息首条 assistant 发言推断
///    （存量 NULL 会话兼容），仍无法推断则跳过；
/// 3. `extract_pairs` 抽取 → 逐条按 (persona_uid, partner, reply) 查重 → 入库。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `session_id`: 目标会话（封存后调用）。
/// - `config`: `[examples]` 配置（总开关）。
///
/// 返回:
/// - 入库 / 跳过计数；任何单条失败仅记 warn 不影响其余（不阻塞封存主流程）。
///
/// 安全约束:
/// - 日志只记计数与 persona_uid，不记对话原文。
pub async fn extract_and_save_for_session(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    config: &ExamplesConfig,
) -> RamariaResult<ExampleSaveStats> {
    if !config.enabled {
        tracing::debug!(%session_id, "examples 配置关闭，跳过回复对抽取");
        return Ok(ExampleSaveStats::default());
    }

    let session = match storage.get_session(session_id).await? {
        Some(session) => session,
        None => {
            tracing::warn!(%session_id, "封存会话不存在，跳过 examples 抽取");
            return Ok(ExampleSaveStats::default());
        }
    };

    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        tracing::debug!(%session_id, "会话无消息，跳过 examples 抽取");
        return Ok(ExampleSaveStats::default());
    }

    // 归属：DB 真相源优先；NULL 会话从消息推断（存量兼容）
    let persona_uid = match session.persona_uid.clone() {
        Some(uid) => uid,
        None => match crate::utt::infer_target_persona_from_messages(&messages) {
            Some(inferred) => {
                tracing::warn!(
                    %session_id,
                    persona_uid = %inferred,
                    "会话 persona_uid 为 NULL，已从消息推断目标 persona（存量兼容）"
                );
                inferred
            }
            None => {
                tracing::debug!(%session_id, "会话无绑定 persona 且无法从消息推断，跳过 examples 抽取");
                return Ok(ExampleSaveStats::default());
            }
        },
    };

    let pairs = extract_pairs(&messages, &persona_uid);
    if pairs.is_empty() {
        tracing::debug!(%session_id, "本会话无有效回复对，跳过入库");
        return Ok(ExampleSaveStats::default());
    }

    Ok(save_pairs(storage, session_id, &persona_uid, pairs).await)
}

/// 把抽取的回复对查重后入库（幂等），并记录统计日志。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `session_id`: 来源会话。
/// - `persona_uid`: 归属人格（已解析，可能来自消息推断）。
/// - `pairs`: 抽取的回复对（非空，由调用方保证）。
async fn save_pairs(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    persona_uid: &str,
    pairs: Vec<ExtractedPair>,
) -> ExampleSaveStats {
    let mut stats = ExampleSaveStats::default();

    for pair in pairs {
        // 幂等查重：已存在相同回复对 → 跳过
        match storage
            .find_example_by_pair(persona_uid, &pair.partner, &pair.reply)
            .await
        {
            Ok(Some(_)) => {
                stats.skipped += 1;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(%session_id, %e, "examples 查重失败，跳过该回复对");
                continue;
            }
        }

        let mut example = PersonaExample::new(persona_uid.to_string(), pair.partner, pair.reply);
        example.session_id = Some(session_id);
        example.context = pair.context;
        example.tags = if pair.tags.is_empty() {
            None
        } else {
            Some(pair.tags)
        };

        match storage.save_example(&example).await {
            Ok(id) => {
                stats.saved += 1;
                tracing::info!(example_id = id, %session_id, persona_uid, "example 已入库");
            }
            Err(e) => {
                tracing::warn!(%session_id, %e, "example 入库失败（不阻塞封存）");
            }
        }
    }

    tracing::info!(
        %session_id,
        persona_uid,
        saved = stats.saved,
        skipped = stats.skipped,
        "examples 回复对抽取入库完成"
    );
    stats
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
        assert!(!is_image_message(&user("正常文本", 1)));
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
}
