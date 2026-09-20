//! crates/ramaria-mcp/src/tools/history.rs - 会话历史工具（chat_history）
//!
//! 设计特点:
//! - 只读工具：分页回看会话消息，不改变状态
//! - 定位二选一：`session_id`（具体会话）或 `persona`（该人格最近会话），都缺省时报可操作错误
//! - 白名单双路径：`persona` 入参与会话归属人格都要通过可见性校验（防越权读取）
//! - 参数校验在协议壳完成：UUID 解析失败给出明确提示，不下抛到服务层

use ramaria_service::HistoryRequest;
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use uuid::Uuid;

use crate::params::HistoryParams;
use crate::result::{success, tool_error};
use crate::server::RamariaMcpServer;

#[rmcp::tool_router(router = history_router, vis = "pub(crate)")]
impl RamariaMcpServer {
    /// 回看会话消息历史（分页）。
    #[rmcp::tool(
        description = "回看 Ramaria 会话的消息历史（分页，按时间正序返回）。\
何时使用：需要某会话或某人格最近会话的原始消息时；session_id 与 persona 二选一（同时给出时以 session_id 为准）。\
何时不要用：需要的是已加工的记忆（摘要 / 事件 / 画像）时（请用 memory_recall 或 persona_get）。",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn chat_history(
        &self,
        Parameters(params): Parameters<HistoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }
        if params.session_id.is_none() && params.persona.is_none() {
            return Ok(tool_error(
                "请提供 session_id 或 persona 之一：session_id 指定具体会话，persona 取该人格最近会话",
            ));
        }

        // ---- 会话 id 解析（协议壳校验，错误可直接操作） ----
        let session_id = match params.session_id.as_deref() {
            Some(raw) => match Uuid::parse_str(raw.trim()) {
                Ok(id) => Some(id),
                Err(_) => {
                    return Ok(tool_error(format!(
                        "session_id 不是合法的 UUID：{raw}。可先用 chat_history + persona 查询该人格最近会话"
                    )));
                }
            },
            None => None,
        };

        // ---- 白名单校验：persona 入参路径 ----
        if let Some(persona) = params
            .persona
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            if let Some(err) = self.check_persona_visible(persona) {
                return Ok(err);
            }
        }

        // ---- 白名单校验：会话归属路径（防用会话 id 绕过白名单） ----
        if let Some(id) = session_id {
            match self.engine().storage().get_session(id).await {
                Ok(Some(session)) => {
                    if let Some(uid) = session.persona_uid.as_deref() {
                        if let Some(err) = self.check_persona_visible(uid) {
                            return Ok(err);
                        }
                    }
                }
                Ok(None) => {
                    return Ok(tool_error(format!(
                        "会话不存在：{id}（可先用 persona 查询最近会话）"
                    )));
                }
                Err(e) => {
                    tracing::warn!(session_id = %id, error = %e, "chat_history 读取会话失败");
                    return Ok(tool_error(format!("读取会话失败：{e}")));
                }
            }
        }

        let request = HistoryRequest {
            session_id,
            persona: params.persona,
            limit: params.limit,
            offset: params.offset,
        };
        match self.engine().history(request).await {
            Ok(result) => {
                tracing::debug!(
                    messages = result.messages.len(),
                    total = result.total,
                    "chat_history 完成"
                );
                Ok(success(&result))
            }
            Err(e) => {
                tracing::warn!(error = %e, "chat_history 执行失败");
                Ok(tool_error(format!("读取会话历史失败：{e}")))
            }
        }
    }
}
