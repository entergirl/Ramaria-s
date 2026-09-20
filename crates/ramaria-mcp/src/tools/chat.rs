//! crates/ramaria-mcp/src/tools/chat.rs - 生成工具（chat_send）
//!
//! 设计特点:
//! - 写工具：受总开关与写入开关（`allow_ingest`）双重门禁（生成会落库并消耗 LLM）
//! - 与桌面同源：服务层生成用例走共用召回与共用 Prompt 装配（见 `ramaria-service::chat`）
//! - 会话口径：显式 `session_id` 优先，缺省用客户端身份名续写同一会话流
//! - 幂等说明：同一消息可能生成不同回复（生成本质非幂等），注解如实标注
//! - 降级提示：LLM 不可用时返回可操作错误（提示检查后端设置），不返回空回复

use ramaria_service::{CHANNEL_MCP, ChatSendRequest, DEFAULT_PERSONA_UID};
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use uuid::Uuid;

use crate::params::ChatSendParams;
use crate::result::{success, tool_error};
use crate::server::RamariaMcpServer;

#[rmcp::tool_router(router = chat_router, vis = "pub(crate)")]
impl RamariaMcpServer {
    /// 以指定人格身份回复一条消息（记忆检索 + 五段式装配 + LLM，回复与消息落库）。
    #[rmcp::tool(
        description = "由 Ramaria 人格直接回复一条消息：内部完成记忆检索与人格化装配，并调用 LLM 生成回复；\
用户消息与回复都会写入记忆库（桌面可见，参与后续记忆加工）。\
何时使用：外部只做传声筒、希望人格与记忆由 Ramaria 提供时。\
何时不要用：只想取记忆上下文自行组织对话时（请用 memory_recall，本工具会消耗 LLM 调用）。",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn chat_send(
        &self,
        Parameters(params): Parameters<ChatSendParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }
        if let Some(err) = self.gate_ingest() {
            return Ok(err);
        }
        if params.message.trim().is_empty() {
            return Ok(tool_error("message 不能为空：请提供本轮用户消息后重试"));
        }

        let persona = params
            .persona
            .clone()
            .unwrap_or_else(|| DEFAULT_PERSONA_UID.to_string());
        if let Some(err) = self.check_persona_visible(&persona) {
            return Ok(err);
        }

        // 显式会话 id：协议壳先解析，非法 UUID 给出可操作提示（不下抛）
        let session_id = match params.session_id.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => match Uuid::parse_str(raw) {
                Ok(id) => Some(id),
                Err(_) => {
                    return Ok(tool_error(format!(
                        "session_id 不是合法的 UUID：{raw}（省略该参数即按客户端身份名续写会话）"
                    )));
                }
            },
            _ => None,
        };

        let request = ChatSendRequest {
            message: params.message,
            persona: Some(persona.clone()),
            session_id,
            // 缺省按客户端身份名续写同一会话流（显式会话 id 时该值不参与定位）
            conversation_id: Some(self.external_ref(None)),
            channel: CHANNEL_MCP.to_string(),
        };

        match self.engine().chat_send(request).await {
            Ok(outcome) => {
                tracing::info!(
                    persona = %persona,
                    session_id = %outcome.session_id,
                    reply_chars = outcome.chars,
                    "chat_send 完成"
                );
                Ok(success(&outcome))
            }
            Err(e) => {
                tracing::warn!(persona = %persona, error = %e, "chat_send 执行失败");
                Ok(tool_error(format!(
                    "生成失败：{e}。可检查 LLM 后端是否可用（桌面「设置 → 后端」）与人格 uid 后重试"
                )))
            }
        }
    }
}
