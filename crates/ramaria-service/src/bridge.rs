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
//!   注入受 `utt.persona_kind_whitelist` 白名单约束（与原文片段一致）

use ramaria_core::config::{BridgeConfig, UttConfig};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, PersonaKind, Session};
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
/// 2. persona 类型不在 `utt.persona_kind_whitelist`（助手/系统类）→ 不加载。
/// 3. 取最近一个已关闭会话（`ended_at` 倒序取最大；无已关闭会话 → 跳过）。
/// 4. 取该会话最后一个 utt 块 → 渲染 + 预算截断。
/// 5. 无 utt 块 → 降级取该会话末 `BRIDGE_FALLBACK_MESSAGE_COUNT` 条原文 → 渲染 + 截断。
/// 6. 仍无 → 跳过。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `bridge_cfg`: 桥接配置（enabled/max_chars）。
/// - `utt_cfg`: utt 配置（persona 类型白名单）。
/// - `persona_uid`: 当前对话人格 UID（None 表示 rama 自身）。
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

    // 2. 原文白名单闸门（与 utt 原文片段一致：助手/系统类 persona 不注入原文）
    let kind = PersonaKind::from_uid(persona_uid.unwrap_or("rama-0001"));
    if !utt_cfg.persona_kind_whitelist.contains(&kind) {
        debug!(persona_kind = ?kind, "persona 类型不在原文白名单内，跳过桥接");
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

    // 解析 persona 注册名（失败回退 uid，不阻塞桥接）
    let persona_name = match persona_uid {
        Some(uid) => match storage.get_persona_by_uid(uid).await {
            Ok(Some(p)) if !p.name.is_empty() => p.name,
            _ => uid.to_string(),
        },
        None => "rama".to_string(),
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
mod tests {
    use super::*;
    use crate::test_support::{engine_with_db, seed_persona, seed_utt_block};
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{MessageRole, MessageSource};

    fn bridge_cfg(enabled: bool) -> BridgeConfig {
        BridgeConfig {
            enabled,
            max_chars: 800,
        }
    }

    fn utt_cfg() -> UttConfig {
        // 白名单 = 角色类（char），与默认一致
        UttConfig::default()
    }

    /// 造一条消息（角色 / 内容 / 发言人归属 / 时间由调用方指定）。
    async fn seed_message(
        storage: &ramaria_storage::SqliteStorage,
        session_id: uuid::Uuid,
        role: MessageRole,
        content: &str,
        persona: Option<&str>,
        created_at: i64,
    ) {
        let mut message = Message::new(session_id, role, content.to_string(), MessageSource::Local)
            .with_persona_uid(persona.map(str::to_string));
        message.created_at = created_at;
        storage
            .save_message(&message)
            .await
            .expect("写入消息应成功");
    }

    /// 桥接开关关闭 → 不加载（验收：开关）。
    #[tokio::test]
    async fn bridge_disabled_returns_none() {
        let (_engine, storage, dir) = engine_with_db("bridge-disabled").await;
        let ctx = load_bridge_context(
            storage.as_ref(),
            &bridge_cfg(false),
            &utt_cfg(),
            Some("char-0001"),
        )
        .await;
        assert!(ctx.is_empty(), "enabled=false 不应加载桥接");
        assert_eq!(ctx.source, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 白名单外 persona（助手类）→ 不加载（回归红线：与原文片段一致）。
    #[tokio::test]
    async fn bridge_skipped_for_non_whitelisted_persona() {
        let (_engine, storage, dir) = engine_with_db("bridge-whitelist").await;
        // rama 自身：Rama 类型不在角色白名单
        let ctx = load_bridge_context(storage.as_ref(), &bridge_cfg(true), &utt_cfg(), None).await;
        assert!(ctx.is_empty(), "助手类 persona 不应加载桥接原文");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无已关闭会话 → 跳过（验收：无会话跳过）。
    #[tokio::test]
    async fn bridge_no_closed_session_skips() {
        let (_engine, storage, dir) = engine_with_db("bridge-no-closed").await;
        seed_persona(&storage, "char-0001").await;
        storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建活跃会话应成功"); // 仅活跃会话

        let ctx = load_bridge_context(
            storage.as_ref(),
            &bridge_cfg(true),
            &utt_cfg(),
            Some("char-0001"),
        )
        .await;
        assert!(ctx.is_empty(), "无已关闭会话不应加载桥接");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一级来源：utt 块（验收：最近会话 + utt 块优先）。
    #[tokio::test]
    async fn bridge_loads_latest_utt_block_of_recent_closed_session() {
        let (_engine, storage, dir) = engine_with_db("bridge-utt").await;
        seed_persona(&storage, "char-0001").await;

        // 两个已关闭会话（ended_at 不同）：只取最近的一个（ended_at 最大）
        let older = crate::test_support::seed_closed_session_with_messages(
            &storage,
            "char-0001",
            1,
            1_700_000_000_000,
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let newer = crate::test_support::seed_closed_session_with_messages(
            &storage,
            "char-0001",
            1,
            1_700_000_000_000,
        )
        .await;

        // 造数前提：新会话关闭时间必须晚于旧会话（否则取最近会话的断言无意义）
        let sessions = storage.list_sessions().await.expect("查询会话应成功");
        let ended_at_of = |id: uuid::Uuid| {
            sessions
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.ended_at)
                .expect("会话应已关闭")
        };
        assert!(
            ended_at_of(newer) > ended_at_of(older),
            "造数前提：新会话关闭时间应晚于旧会话"
        );

        // 新会话有块、旧会话无块 → 加载新会话的块
        seed_utt_block(
            &storage,
            newer,
            "char-0001",
            "[2026-08-01 20:00] 测试人格: 上次聊到这里\n[2026-08-01 20:01] 用户: 好的",
        )
        .await;

        let ctx = load_bridge_context(
            storage.as_ref(),
            &bridge_cfg(true),
            &utt_cfg(),
            Some("char-0001"),
        )
        .await;
        assert_eq!(ctx.source, Some(BridgeSource::UttBlock));
        let content = ctx.content.expect("应加载桥接内容");
        assert!(
            content.contains("上次聊到这里"),
            "应含 utt 块内容: {content}"
        );
        assert!(
            !content.contains("消息内容 0"),
            "不应回退到旧会话消息: {content}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 二级降级：无 utt 块 → 取末 N 条原文（验收：两级降级）。
    #[tokio::test]
    async fn bridge_falls_back_to_recent_messages() {
        let (_engine, storage, dir) = engine_with_db("bridge-fallback").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");

        // 8 条消息：降级只取末 5 条（助手消息带人格、用户消息不带）
        for i in 0..8 {
            let (role, persona) = if i % 2 == 0 {
                (MessageRole::Assistant, Some("char-0001"))
            } else {
                (MessageRole::User, None)
            };
            seed_message(
                &storage,
                session.id,
                role,
                &format!("消息{i}"),
                persona,
                1_700_000_000_000 + i * 60_000,
            )
            .await;
        }
        storage
            .close_session(session.id)
            .await
            .expect("关闭会话应成功");

        let ctx = load_bridge_context(
            storage.as_ref(),
            &bridge_cfg(true),
            &utt_cfg(),
            Some("char-0001"),
        )
        .await;
        assert_eq!(ctx.source, Some(BridgeSource::RecentMessages));
        let content = ctx.content.expect("应加载桥接内容");
        assert!(
            content.contains("消息3"),
            "应含末 5 条的首条（消息3）: {content}"
        );
        assert!(content.contains("消息7"), "应含最后一条（消息7）");
        assert!(!content.contains("消息0"), "不应含被裁掉的早前消息");
        assert!(content.contains("测试人格"), "应含 persona 名称");
        assert!(content.contains("用户"), "应含用户角色标签");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 降级路径但会话无消息 → 跳过（验收：两级降级尽头）。
    #[tokio::test]
    async fn bridge_fallback_no_messages_skips() {
        let (_engine, storage, dir) = engine_with_db("bridge-empty").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        storage
            .close_session(session.id)
            .await
            .expect("关闭会话应成功");

        // 无 utt 块、无消息
        let ctx = load_bridge_context(
            storage.as_ref(),
            &bridge_cfg(true),
            &utt_cfg(),
            Some("char-0001"),
        )
        .await;
        assert!(ctx.is_empty(), "无块无消息应跳过桥接");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预算截断：超预算从头部截断、保最近（验收：预算截断）。
    #[test]
    fn truncate_from_head_keeps_recent_content() {
        let text = "第一行内容\n第二行内容\n第三行内容";
        let out = truncate_from_head(text, 10);
        assert!(out.contains("第三行内容"), "应保留最近内容: {out}");
        assert!(out.contains("前文已省略"), "应含省略标记");
        // 截断后实际内容（不含标记行）≤ 预算
        let body = out.split('\n').next_back().unwrap_or("");
        assert!(body.chars().count() <= 10, "保留内容不应超预算");
    }

    /// 预算充足 → 原样返回（不截断）。
    #[test]
    fn truncate_from_head_within_budget_unchanged() {
        let text = "短内容";
        assert_eq!(truncate_from_head(text, 800), text);
    }

    /// 单条超预算 → 仍保留最近尾部（不产生空注入）。
    #[test]
    fn truncate_from_head_single_line_over_budget() {
        let text = "这是一条非常长的消息内容，远超预算";
        let out = truncate_from_head(text, 8);
        assert!(!out.is_empty());
        let body = out.split('\n').next_back().unwrap_or("");
        assert!(body.chars().count() <= 8);
        assert!(body.contains("预算"), "应保留末尾字符");
    }
}
