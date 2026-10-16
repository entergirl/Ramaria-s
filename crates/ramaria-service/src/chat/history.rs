//! crates/ramaria-service/src/chat/history.rs - Ramaria 历史窗口加载模块
//!
//! 设计特点:
//! - 分页倒序加载 + 条数上限与字符预算（`[session]` 配置驱动），预算按渲染后正文计
//! - 图片占位符按会话类型分口径渲染：社交会话转「对方发来一张图片（…）」语义，
//!   其余会话与 core 浏览口径一致（完成 → 描述、处理中 → 提示；其余状态保留裸占位符；
//!   附件查询失败降级原文）
//! - 社交会话按发言人前缀标注他人消息（`{sender_name}：`，全角冒号）；预置上文不标注
//! - 被撤回消息两态渲染：已消费（其后有助手消息）保留原内容并追加撤回信号；
//!   未消费仅保留信号；预算统计与最终注入同口径
//! - 读取失败记 warn 并保留已加载部分，不阻塞生成

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{ChatMessage, StorageBackend};
use ramaria_core::types::{Message, MessageAttachment, replace_image_placeholders};
use uuid::Uuid;

use super::render::{build_history_render_map, load_message_attachments};
use super::social::sender_prefix;

/// 历史加载分页条数（实现常量；加载条数与字符预算由 `[session]` 配置提供）。
const HISTORY_PAGE_SIZE: i64 = 20;

/// 步骤 5：载入会话历史（时间正序），配置驱动的分页窗口。
///
/// 语义:
/// - 分页 20 条倒序加载（先检查条数上限与字符预算，再取页）；
/// - 加载条数上限取 `[session].max_history_messages`，字符预算取 `[session].max_history_chars`，
///   每条按**渲染后正文**（`[图片#{hash}]` 占位符注入附件描述后）的
///   `content.chars().count() + 16`（role 标记开销）计；社交会话的他人消息
///   额外计入发言人前缀开销（`sender_name` 字符数 + 1）；
/// - 读取失败记 warn 并保留已加载部分（首屏失败即空历史），不阻塞生成；
///   附件查询失败同样降级为正文原文；
/// - 图片占位符渲染按会话类型分口径：社交通道会话（`social_senders=true`）完成 /
///   处理中条目转「对方发来一张图片（…）」语义；其余会话与 core 浏览口径逐字节一致
///   （完成 → 描述、处理中 → 提示）；未命中保留裸占位符；
/// - 社交会话（`social_senders=true`）对 `role=User` 且发言人非空的消息在
///   图片占位符渲染后于正文前加 `{sender_name}：` 前缀；assistant 消息与
///   无发言人信息的消息零变化；
/// - 按 `created_at` 升序排列后，前置拼接调用方预置上文（seed 早于本会话历史；
///   预置上文来自本地上下文、无发言人信息，一律不加前缀）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `config`: 生效配置（`[session]` 段提供窗口参数）。
/// - `session_id`: 目标会话。
/// - `seed_history`: 调用方预置上文（时间正序；直接前置，不做渲染与标注）。
/// - `social_senders`: 是否按发言人前缀标注历史消息（社交通道会话为 true）。
///
/// 返回:
/// - 可直接进入 Prompt 的 `ChatMessage` 列表（seed 在前、会话历史在后）。
pub(super) async fn step_load_history(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    session_id: Uuid,
    seed_history: &[ChatMessage],
    social_senders: bool,
) -> Vec<ChatMessage> {
    let max_messages = config.session.max_history_messages.max(1) as i64;
    let char_budget = config.session.max_history_chars.max(1) as usize;

    let mut loaded: Vec<Message> = Vec::new();
    let mut attachment_rows: Vec<MessageAttachment> = Vec::new();
    let mut total_chars: usize = 0;
    let mut offset: i64 = 0;

    loop {
        if loaded.len() as i64 >= max_messages {
            tracing::debug!(
                session_id = %session_id,
                loaded = loaded.len(),
                max = max_messages,
                "历史消息已达到加载条数上限"
            );
            break;
        }
        if total_chars >= char_budget {
            tracing::debug!(
                session_id = %session_id,
                total_chars,
                budget = char_budget,
                "历史消息已达到字符预算"
            );
            break;
        }

        let page = match storage
            .list_messages_paginated(session_id, HISTORY_PAGE_SIZE, offset)
            .await
        {
            Ok(page) => page,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    loaded = loaded.len(),
                    error = %e,
                    "分页加载会话历史失败，按已加载部分继续"
                );
                break;
            }
        };
        if page.is_empty() {
            break; // 没有更多消息
        }

        let page_len = page.len() as i64;
        // 字符预算按渲染后正文计算（附件描述注入会加长实际进入 Prompt 的文本；
        // 查询失败降级为空映射 → 按原文计量）；社交会话额外计入发言人前缀开销
        let page_attachments = load_message_attachments(storage, &page).await;
        let page_map = build_history_render_map(&page_attachments, social_senders);
        for message in &page {
            let rendered = replace_image_placeholders(&message.content, &page_map);
            let rendered = render_if_recalled(storage, session_id, message, rendered).await;
            total_chars += rendered.chars().count() + 16;
            if social_senders {
                if let Some(prefix) = sender_prefix(message) {
                    total_chars += prefix.chars().count() + 1;
                }
            }
        }
        attachment_rows.extend(page_attachments);
        loaded.extend(page);
        offset += HISTORY_PAGE_SIZE;

        // 如果返回的页不满，说明已是最后一批
        if page_len < HISTORY_PAGE_SIZE {
            break;
        }
    }

    // 按 created_at 升序排列（分页返回为倒序；相同时刻保持稳定不翻转）
    loaded.sort_by_key(|message| message.created_at);

    // 最终映射由全部已加载消息的附件行构建（同一 md5 描述一致，跨页无冲突）
    let render_map = build_history_render_map(&attachment_rows, social_senders);
    let mut merged = seed_history.to_vec();
    for message in loaded {
        let content = replace_image_placeholders(&message.content, &render_map);
        // 撤回渲染在占位符替换之后、发言人前缀之前
        let mut content = render_if_recalled(storage, session_id, &message, content).await;
        if social_senders {
            if let Some(prefix) = sender_prefix(&message) {
                content = format!("{prefix}：{content}");
            }
        }
        merged.push(ChatMessage {
            role: message.role,
            content,
        });
    }
    merged
}

/// 对被撤回消息应用两态渲染（撤回信号替换 / 追加）；其余消息原样返回。
///
/// 说明:
/// - 已消费判定查询失败按未消费处理（保守丢弃原文）；
/// - 仅在消息已撤回时才发起存储查询（撤回消息罕见）。
async fn render_if_recalled(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    message: &Message,
    rendered: String,
) -> String {
    if message.recalled_at.is_none() {
        return rendered;
    }
    let consumed = storage
        .has_assistant_message_after(session_id, message.created_at)
        .await
        .unwrap_or(false);
    crate::withdraw::render_recalled_content(&rendered, consumed)
}
