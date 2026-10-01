//! crates/ramaria-service/src/export.rs - 会话导出装配与载荷渲染模块
//!
//! 设计特点:
//! - 装配：按会话收集全量消息与人格关联的未吸收 L1 摘要（分页作用于过滤后的集合）
//! - 渲染：JSON / Markdown 载荷由本层单一实现，入口（CLI / 桌面）共用，不写第二份渲染
//! - 脱敏：`redact` 开关将消息正文与 L1 摘要替换为 `<N chars>`（字符数），其余字段不变
//! - 人格过滤：仅保留含目标 persona_uid 消息的会话（消息集合保持全量，不按消息二次过滤）
//! - 计数口径：`total_sessions` 为过滤前全部会话数，`sessions` 为过滤并分页后的装配结果
//! - 隐私：日志只记计数；渲染不落日志，消息正文与摘要不进日志

use chrono::TimeZone;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{MemoryL1, Message, MessageRole, Session};

use crate::engine::Engine;

// =========================================================
// 请求与结果类型
// =========================================================

/// 会话导出数据装配请求。
///
/// 字段约定:
/// - `persona`: 人格过滤（`None` = 不过滤）；仅保留含目标 persona_uid 消息的会话。
/// - `limit`: 会话条数上限（`None` = 全部；`Some(0)` 按下界 1 处理）。
/// - `offset`: 分页偏移（缺省 0）。
#[derive(Debug, Clone, Default)]
pub struct ExportDataRequest {
    pub persona: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 单个会话的导出数据。
///
/// 字段约定:
/// - `session`: 会话核心行（起止时间 / 人格归属 / 通道等信息）。
/// - `messages`: 会话全量消息（时间正序）。
#[derive(Debug, Clone)]
pub struct ExportSessionData {
    pub session: Session,
    pub messages: Vec<Message>,
}

/// 导出数据装配结果。
///
/// 字段约定:
/// - `total_sessions`: 过滤前的全部会话数（分页无关）。
/// - `sessions`: 过滤并分页后的会话数据（含各自全量消息）。
/// - `l1_persona`: 请求指定的人格（`None` = 未指定）；供 L1 段回填 `persona_uid`，
///   使空列表（该人格无未吸收摘要）时仍保留请求值。
/// - `l1_memories`: 指定人格时的未吸收 L1 摘要段（`None` = 未指定人格；
///   空列表 = 该人格无未吸收摘要）。
#[derive(Debug, Clone)]
pub struct ExportData {
    pub total_sessions: usize,
    pub sessions: Vec<ExportSessionData>,
    pub l1_persona: Option<String>,
    pub l1_memories: Option<Vec<MemoryL1>>,
}

// =========================================================
// 装配用例
// =========================================================

/// 装配会话导出数据（会话集合 + 消息 + 人格 L1 摘要段）。
///
/// 流程:
/// 1. 读取全部会话并逐会话读取全量消息；
/// 2. 人格过滤：仅保留含目标 persona_uid 消息的会话；
/// 3. 分页：对过滤后的会话集合应用 offset / limit；
/// 4. 指定人格时读取该人格的未吸收 L1 摘要（空列表为正常空态）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 装配请求（人格过滤 / 分页）。
///
/// 返回:
/// - `ExportData`：过滤前总数、过滤并分页后的会话数据与可选 L1 摘要段。
pub(crate) async fn collect(engine: &Engine, req: ExportDataRequest) -> RamariaResult<ExportData> {
    let storage = engine.storage_ref();
    let all_sessions = storage.list_sessions().await?;
    let total_sessions = all_sessions.len();

    // ---- 逐会话装配（过滤需要先读消息判定归属；消息集合保持全量） ----
    let mut filtered: Vec<ExportSessionData> = Vec::with_capacity(all_sessions.len());
    for session in all_sessions {
        let messages = storage.list_messages(session.id).await?;
        if let Some(persona) = req.persona.as_deref() {
            let matched = messages
                .iter()
                .any(|message| message.persona_uid.as_deref() == Some(persona));
            if !matched {
                continue;
            }
        }
        filtered.push(ExportSessionData { session, messages });
    }

    // ---- 分页（作用于过滤后的会话集合） ----
    let offset = req.offset.unwrap_or(0) as usize;
    let limit = req
        .limit
        .map(|limit| limit.max(1) as usize)
        .unwrap_or(usize::MAX);
    let sessions: Vec<ExportSessionData> = filtered.into_iter().skip(offset).take(limit).collect();
    let message_count: usize = sessions.iter().map(|session| session.messages.len()).sum();

    // ---- 人格 L1 摘要段（仅指定人格时装配） ----
    let l1_memories = match req.persona.as_deref() {
        Some(persona) => Some(storage.list_unabsorbed_l1(persona).await?),
        None => None,
    };

    tracing::debug!(
        total_sessions,
        exported_sessions = sessions.len(),
        message_count,
        persona_filter = req.persona.is_some(),
        "会话导出数据装配完成"
    );

    Ok(ExportData {
        total_sessions,
        sessions,
        l1_persona: req.persona.clone(),
        l1_memories,
    })
}

// =========================================================
// 载荷渲染（入口共用）
// =========================================================

/// 导出载荷格式版本（与 crate 版本无关）。
///
/// 说明:
/// - 表示 `ramaria_export` 信封内载荷的结构版本，载荷结构不兼容变化时递增；
/// - CLI / 桌面共用同一常量，避免两侧硬编码分叉。
pub const EXPORT_FORMAT_VERSION: &str = "0.1.0";

/// 渲染会话导出 JSON 载荷（CLI / 桌面共用）。
///
/// 参数:
/// - `data`: 导出装配结果。
/// - `redact`: 脱敏开关；`true` 时消息正文与 L1 摘要替换为 `<N chars>`（字符数），其余字段不变。
///
/// 返回:
/// - `{"ramaria_export": {"version", "exported_at", "sessions": [...]}}` 的格式化 JSON 文本；
///   `data.l1_memories` 为 `Some` 时在 `sessions` 数组末尾附加 `l1_memories` 段
///   （`persona_uid` 取 `data.l1_persona`，空列表时同样保留请求人格）。
///
/// 说明:
/// - 时间字段统一为 `%Y-%m-%d %H:%M`（UTC）；`ended_at` 为 `None`（未关闭）时输出 `null`；
/// - 会话集合不做空消息过滤（过滤只作用于 Markdown 渲染）；
/// - 序列化异常返回降级载荷（不 panic）。
pub fn render_sessions_json(data: &ExportData, redact: bool) -> String {
    let mut sections: Vec<serde_json::Value> = data
        .sessions
        .iter()
        .map(|entry| {
            serde_json::json!({
                "session_id": entry.session.id.to_string(),
                "started_at": format_timestamp(entry.session.started_at),
                "ended_at": format_timestamp(entry.session.ended_at.unwrap_or(0)),
                "messages": entry.messages.iter().map(|message| {
                    serde_json::json!({
                        "role": message.role.as_str(),
                        "content": render_body(&message.content, redact),
                        "source": message.source.to_string(),
                        "created_at": format_timestamp(message.created_at),
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();

    if let Some(l1_memories) = data.l1_memories.as_deref() {
        // persona 口径：取请求指定的人格；列表为空时同样保留请求值
        sections.push(serde_json::json!({
            "type": "l1_memories",
            "persona_uid": data.l1_persona.as_deref(),
            "count": l1_memories.len(),
            "items": l1_memories.iter().map(|memory| {
                serde_json::json!({
                    "id": memory.id.to_string(),
                    "session_id": memory.session_id.to_string(),
                    "summary": render_body(&memory.summary, redact),
                    "valence": memory.valence,
                    "salience": memory.salience,
                    "created_at": format_timestamp(memory.created_at),
                })
            }).collect::<Vec<_>>(),
        }));
    }

    let payload = serde_json::json!({
        "ramaria_export": {
            "version": EXPORT_FORMAT_VERSION,
            "exported_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "sessions": sections,
        }
    });

    serde_json::to_string_pretty(&payload).unwrap_or_else(|error| {
        // 内存值 → 文本的序列化不可失败；此分支仅作防御，保证调用方不面对 panic
        tracing::warn!(%error, "导出 JSON 序列化失败，返回降级载荷");
        degraded_export_json()
    })
}

/// 渲染会话导出 Markdown 载荷（CLI / 桌面共用）。
///
/// 参数:
/// - `data`: 导出装配结果。
/// - `redact`: 脱敏开关；`true` 时消息正文替换为 `<N chars>`（字符数）。
///
/// 返回:
/// - `Some(...)`: 至少一个会话含消息时的 Markdown 文本；头部含导出时间
///   （`%Y-%m-%d %H:%M UTC`），逐会话输出标题、创建时间与角色标签。
/// - `None`: 全部会话均无消息（调用方按"没有可导出的会话数据"处理，不写文件）。
///
/// 说明:
/// - 无消息会话跳过；角色标签: 用户 / AI / 系统 / 未知（未识别角色回退）。
pub fn render_sessions_markdown(data: &ExportData, redact: bool) -> Option<String> {
    let mut markdown = String::new();
    markdown.push_str("# Ramaria 对话导出\n\n");
    markdown.push_str(&format!(
        "导出时间: {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    markdown.push_str("---\n\n");

    let mut exported_sessions = 0usize;
    for entry in &data.sessions {
        if entry.messages.is_empty() {
            continue;
        }
        exported_sessions += 1;
        markdown.push_str(&format!("## 会话 {}\n\n", entry.session.id));
        if let Some(started_at) = format_timestamp(entry.session.started_at) {
            markdown.push_str(&format!("*创建时间: {started_at}*\n\n"));
        }

        for message in &entry.messages {
            let role_label = match message.role {
                MessageRole::User => "**👤 用户**",
                MessageRole::Assistant => "**🤖 AI**",
                MessageRole::System => "*⚙ 系统*",
                _ => "*❓ 未知*",
            };
            markdown.push_str(&format!("{role_label}\n\n"));
            markdown.push_str(&render_body(&message.content, redact));
            markdown.push_str("\n\n---\n\n");
        }
    }

    if exported_sessions == 0 {
        return None;
    }
    Some(markdown)
}

// =========================================================
// 渲染辅助（私有）
// =========================================================

/// 渲染可能脱敏的文本字段：开关打开时替换为 `<N chars>`（N 为字符数，不输出原文）。
fn render_body(text: &str, redact: bool) -> String {
    if redact {
        format!("<{} chars>", text.chars().count())
    } else {
        text.to_string()
    }
}

/// 将 Unix 毫秒时间戳格式化为 `%Y-%m-%d %H:%M`（UTC）。
///
/// 返回:
/// - `Some("2024-06-10 08:00")`: 有效时间戳（ms > 0）。
/// - `None`: ms ≤ 0（无效时间戳；JSON 输出 `null`，Markdown 省略该行）。
fn format_timestamp(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1000;
    chrono::Utc
        .timestamp_opt(secs, ((ms % 1000) * 1_000_000) as u32)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
}

/// 序列化异常时的降级 JSON 载荷：保留信封与格式版本，会话集合为空。
fn degraded_export_json() -> String {
    format!(
        "{{\"ramaria_export\":{{\"version\":\"{EXPORT_FORMAT_VERSION}\",\"exported_at\":\"\",\"sessions\":[]}}}}"
    )
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
