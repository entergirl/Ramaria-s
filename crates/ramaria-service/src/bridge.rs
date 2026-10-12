//! crates/ramaria-service/src/bridge.rs - 会话桥接（新会话衔接上一会话尾部原文）
//!
//! 设计特点:
//! - 新会话创建时加载"上一会话尾部"原文，帮助 LLM 保持对话连贯性
//! - 两级降级：优先取最近一个已关闭会话的最后一个 utt 块；
//!   无 utt 块时降级取该会话末 N 条原文消息；仍无则跳过（不注入）
//! - 只取最近一个已关闭会话（不链式回溯，防止级联错误传播）
//! - `bridge.enabled=false` 或 persona 类型不在原文白名单内 → 不加载
//! - 预算从头部截断、保最近内容（`bridge.max_chars`）
//!
//! 安全约束（隐私红线）:
//! - 桥接内容承载原文级信息（最高敏感层）：不写入日志（仅计数/来源），
//!   注入受 `utt.persona_kind_whitelist` 白名单约束（类型取自 persona 行，与原文片段一致）

use ramaria_core::config::{BridgeConfig, UttConfig};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, Session};
use tracing::{debug, info, warn};

/// 降级路径取末 N 条原文消息（无 utt 块时）。
///
/// 说明:
/// - 固定常量而非配置项（未定义该参数；最小改动原则）。
/// - 取值 5：足够传递上一会话尾部语境，又避免长会话原文过载。
pub const BRIDGE_FALLBACK_MESSAGE_COUNT: usize = 5;

/// 桥接内容来源（供日志/诊断使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeSource {
    /// 来源：最近已关闭会话的最后一个 utt 块。
    UttBlock,
    /// 来源：降级路径——该会话末 N 条原文消息。
    RecentMessages,
}

/// 桥接加载结果。
#[derive(Debug, Clone)]
pub struct BridgeContext {
    /// 渲染后的桥接内容（已按预算头部截断；None 表示不注入）。
    pub content: Option<String>,
    /// 内容来源（None 表示未加载）。
    pub source: Option<BridgeSource>,
}

impl BridgeContext {
    /// 构造"不注入"结果（开关关闭/白名单外/无上一会话）。
    fn none() -> Self {
        Self {
            content: None,
            source: None,
        }
    }

    /// 是否加载了桥接内容。
    pub fn is_empty(&self) -> bool {
        self.content.is_none() || self.content.as_deref().is_none_or(str::is_empty)
    }
}

/// 加载桥接上下文（新会话创建时调用一次）。
///
/// 流程（两级降级，任一环节失败不阻塞，降级到下一级）:
/// 1. `bridge.enabled == false` → 不加载。
/// 2. persona 行缺失或类型不在 `utt.persona_kind_whitelist`（助手/系统类）→ 不加载。
/// 3. 取最近一个已关闭会话（`ended_at` 倒序取最大；无已关闭会话 → 跳过）。
/// 4. 取该会话最后一个 utt 块 → 渲染 + 预算截断。
/// 5. 无 utt 块 → 降级取该会话末 `BRIDGE_FALLBACK_MESSAGE_COUNT` 条原文 → 渲染 + 截断。
/// 6. 仍无 → 跳过。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `bridge_cfg`: 桥接配置（enabled/max_chars）。
/// - `utt_cfg`: utt 配置（persona 类型白名单）。
/// - `persona_uid`: 当前对话人格 UID（None = 无人格上下文，跳过桥接）。
///
/// 返回:
/// - `BridgeContext`：内容与来源（不注入时两者均为 None）。
pub async fn load_bridge_context(
    storage: &dyn StorageBackend,
    bridge_cfg: &BridgeConfig,
    utt_cfg: &UttConfig,
    persona_uid: Option<&str>,
) -> BridgeContext {
    // 1. 开关闸门
    if !bridge_cfg.enabled {
        debug!("bridge.enabled=false，跳过桥接加载");
        return BridgeContext::none();
    }

    // 2. 原文白名单闸门（与 utt 原文片段一致：类型取自 persona 行；行缺失 / 读取失败保守跳过）
    let Some(uid) = persona_uid else {
        debug!("无人格上下文（persona_uid=None），跳过桥接");
        return BridgeContext::none();
    };
    let persona = match storage.get_persona_by_uid(uid).await {
        Ok(Some(persona)) => persona,
        Ok(None) => {
            warn!(persona = %uid, "persona 行不存在，跳过桥接");
            return BridgeContext::none();
        }
        Err(e) => {
            warn!(persona = %uid, error = %e, "persona 类型读取失败，跳过桥接");
            return BridgeContext::none();
        }
    };
    if !utt_cfg.persona_kind_whitelist.contains(&persona.kind) {
        debug!(persona_kind = ?persona.kind, "persona 类型不在原文白名单内，跳过桥接");
        return BridgeContext::none();
    }

    // 3. 最近一个已关闭会话（不链式：只取最近一个，不递归回溯）
    let sessions = match storage.list_sessions().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "查询会话列表失败，跳过桥接");
            return BridgeContext::none();
        }
    };
    let last_closed: Option<&Session> = sessions
        .iter()
        .filter(|s| s.ended_at.is_some())
        .max_by_key(|s| s.ended_at.unwrap_or(0));
    let Some(prev) = last_closed else {
        debug!("无已关闭会话，跳过桥接");
        return BridgeContext::none();
    };
    let prev_id = prev.id;
    let prev_ended = prev.ended_at.unwrap_or(0);
    debug!(session_id = %prev_id, ended_at = prev_ended, "找到最近已关闭会话（桥接来源）");

    // 4. 一级来源：最后一个 utt 块（块文本已是 [时间] 角色: 内容 行序列）
    match storage.get_latest_utt_block_by_session(prev_id).await {
        Ok(Some(block)) if !block.block_text.trim().is_empty() => {
            let content = truncate_from_head(&block.block_text, bridge_cfg.max_chars as usize);
            info!(
                session_id = %prev_id,
                block_id = block.id,
                chars = content.chars().count(),
                "桥接已加载（来源：上一会话 utt 块）"
            );
            return BridgeContext {
                content: Some(content),
                source: Some(BridgeSource::UttBlock),
            };
        }
        Ok(Some(_)) => {
            debug!(session_id = %prev_id, "上一会话 utt 块为空，降级取原文");
        }
        Ok(None) => {
            debug!(session_id = %prev_id, "上一会话无 utt 块，降级取原文");
        }
        Err(e) => {
            warn!(session_id = %prev_id, error = %e, "读取 utt 块失败，降级取原文");
        }
    }

    // 5. 二级降级：末 N 条原文消息
    let messages = match storage.list_messages(prev_id).await {
        Ok(m) => m,
        Err(e) => {
            warn!(session_id = %prev_id, error = %e, "读取上一会话消息失败，跳过桥接");
            return BridgeContext::none();
        }
    };
    let tail: Vec<&Message> = messages
        .iter()
        .skip(messages.len().saturating_sub(BRIDGE_FALLBACK_MESSAGE_COUNT))
        .collect();
    if tail.is_empty() {
        debug!(session_id = %prev_id, "上一会话无消息，跳过桥接");
        return BridgeContext::none();
    }

    // 人物名渲染：复用已解析 persona 的注册名（空名回退 uid）
    let persona_name = if persona.name.is_empty() {
        uid.to_string()
    } else {
        persona.name.clone()
    };

    let rendered = render_messages(&tail, persona_uid, &persona_name);
    if rendered.trim().is_empty() {
        return BridgeContext::none();
    }
    let content = truncate_from_head(&rendered, bridge_cfg.max_chars as usize);
    info!(
        session_id = %prev_id,
        source = "recent_messages",
        msg_count = tail.len(),
        chars = content.chars().count(),
        "桥接已加载（来源：上一会话末 N 条原文）"
    );
    BridgeContext {
        content: Some(content),
        source: Some(BridgeSource::RecentMessages),
    }
}

/// 按预算从头部截断、保最近内容。
///
/// 规则（预算从头部截断保最近）:
/// - 文本字符数 ≤ 预算 → 原样返回。
/// - 超预算 → 保留末尾 `max_chars` 个字符，前缀省略标记行
///   （标记行不计入预算，确保省略语义清晰）。
///
/// 参数:
/// - `text`: 原始文本。
/// - `max_chars`: 字符预算上限。
///
/// 返回:
/// - 截断后的文本。
pub fn truncate_from_head(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count - max_chars).collect();
    format!("…（前文已省略，以下为上一会话尾部）\n{kept}")
}

/// 渲染消息序列为 `[时间] 角色: 内容` 行（时间升序，对齐 utt 块文本格式）。
///
/// 规则:
/// - `msg.persona_uid == Some(target_uid)` → `target_name`（目标 persona 发言）。
/// - `msg.persona_uid` 为 None（用户消息）→ "用户"。
/// - 其他 uid（跨 persona 防御）→ 直接显示 uid。
fn render_messages(messages: &[&Message], target_uid: Option<&str>, target_name: &str) -> String {
    let mut lines = Vec::with_capacity(messages.len());
    for m in messages {
        let speaker = match m.persona_uid.as_deref() {
            Some(uid) if Some(uid) == target_uid => target_name.to_string(),
            Some(uid) => uid.to_string(),
            None => "用户".to_string(),
        };
        let time = format_bridge_time(m.created_at);
        lines.push(format!("[{time}] {speaker}: {}", m.content));
    }
    lines.join("\n")
}

/// 将时间戳格式化为 `YYYY-MM-DD HH:MM`（本地时区；非法值回退毫秒，不 panic）。
fn format_bridge_time(created_at_ms: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(created_at_ms) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => created_at_ms.to_string(),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
