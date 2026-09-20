//! crates/ramaria-mcp/src/tools/ingest.rs - 外部对话回流工具（chat_ingest）
//!
//! 设计特点:
//! - 写工具：受总开关与写入开关（`allow_ingest`）双重门禁
//! - 幂等语义：同一 `conversation_id` 重复提交安全（服务层指纹去重）
//! - 封存治理：`finalize` 受 `allow_seal` 约束；关闭时只写不封存并在回执中说明
//! - 回执透明：写入成功但封存未完成（LLM 不可用 / 已被其他进程封存）也给出说明
//! - 会话标识：显式 `conversation_id` 优先，缺省用客户端身份名（见协议壳）

use ramaria_service::{CHANNEL_MCP, DEFAULT_PERSONA_UID, IngestRequest};
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use crate::params::{IngestParams, MessageParam};
use crate::result::{success_value, tool_error};
use crate::server::RamariaMcpServer;

#[rmcp::tool_router(router = ingest_router, vis = "pub(crate)")]
impl RamariaMcpServer {
    /// 把外部对话写回 Ramaria 记忆库（进 L0，桌面可见并参与后续记忆加工）。
    #[rmcp::tool(
        description = "把外部对话回流写入 Ramaria 记忆库，使内容在桌面可见并参与记忆加工（L1 摘要等）。\
何时使用：外部对话结束后（推荐）或每轮结束时提交本段对话；同一 conversation_id 重复提交安全（指纹去重）。\
何时不要用：只想读取记忆时（请用 memory_recall）。finalize=true 表示该段对话结束，将触发封存与摘要生成。",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn chat_ingest(
        &self,
        Parameters(params): Parameters<IngestParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }
        if let Some(err) = self.gate_ingest() {
            return Ok(err);
        }
        if params.messages.is_empty() {
            return Ok(tool_error(
                "messages 不能为空：请提供至少一条 user / assistant 消息后重试",
            ));
        }

        let persona = params
            .persona
            .clone()
            .unwrap_or_else(|| DEFAULT_PERSONA_UID.to_string());
        if let Some(err) = self.check_persona_visible(&persona) {
            return Ok(err);
        }

        let seal_allowed = self.mcp_config().allow_seal;
        let finalize_requested = params.finalize;
        let conversation_id = self.external_ref(params.conversation_id.as_deref());

        let request = IngestRequest {
            messages: params
                .messages
                .into_iter()
                .map(MessageParam::into_turn)
                .collect(),
            persona: Some(persona.clone()),
            conversation_id: Some(conversation_id),
            channel: CHANNEL_MCP.to_string(),
            // allow_seal=false：只写不封存（回执中说明本次未触发摘要生成）
            finalize: finalize_requested && seal_allowed,
        };

        match self.engine().ingest(request).await {
            Ok(outcome) => {
                let mut value = serde_json::to_value(&outcome).map_err(|e| {
                    tracing::error!(error = %e, "chat_ingest 结果序列化失败");
                    ErrorData::internal_error(format!("结果序列化失败：{e}"), None)
                })?;
                if let Some(obj) = value.as_object_mut() {
                    if finalize_requested && !seal_allowed {
                        obj.insert(
                            "note".to_string(),
                            serde_json::json!(
                                "内容已写入；本次未触发封存：MCP 侧封存已禁用（[mcp].allow_seal = false），后续封存时再加工"
                            ),
                        );
                    } else if finalize_requested && !outcome.finalized {
                        obj.insert(
                            "note".to_string(),
                            serde_json::json!(
                                "内容已写入；封存未完成（LLM 不可用或该会话已被其他进程封存），空闲检查会继续重试"
                            ),
                        );
                    }
                }
                tracing::info!(
                    persona = %persona,
                    session_id = %outcome.session_id,
                    written = outcome.written,
                    deduplicated = outcome.deduplicated,
                    finalized = outcome.finalized,
                    "chat_ingest 完成"
                );
                Ok(success_value(value))
            }
            Err(e) => {
                tracing::warn!(persona = %persona, error = %e, "chat_ingest 执行失败");
                Ok(tool_error(format!(
                    "写入失败：{e}。可检查库文件权限与人格 uid 后重试（重复提交同一对话是安全的）"
                )))
            }
        }
    }
}
