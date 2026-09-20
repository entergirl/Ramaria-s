//! crates/ramaria-mcp/src/tools/recall.rs - 记忆召回工具（memory_recall）
//!
//! 设计特点:
//! - 只读工具：不改变任何状态，不访问外部世界（注解显式声明）
//! - 预算口径：请求参数优先，缺省取 `[mcp]` 配置；上限由服务层用例钳制
//! - 概览模式：不传 `query` 且 `messages` 为空时按时间线返回最近记忆
//! - 隐私：人格白名单前置校验；原文块是否出端由注入的召回策略决定

use ramaria_service::{DEFAULT_PERSONA_UID, RecallRequest};
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use crate::params::{MessageParam, RecallParams};
use crate::result::{success, tool_error};
use crate::server::RamariaMcpServer;

#[rmcp::tool_router(router = recall_router, vis = "pub(crate)")]
impl RamariaMcpServer {
    /// 按一段对话返回可直接拼入提示词的记忆上下文（不生成回复）。
    #[rmcp::tool(
        description = "Ramaria 记忆服务：按最近对话返回可直接拼入提示词的个性化上下文（L1 摘要 / L2 事件 / 知识事实 / 近期脉络）。\
何时使用：外部前端每轮对话前调用一次，取回记忆后由自己的模型继续对话。\
何时不要用：需要 Ramaria 人格直接生成回复时（本工具不产生回复）。",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn memory_recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }

        let persona = params
            .persona
            .clone()
            .unwrap_or_else(|| DEFAULT_PERSONA_UID.to_string());
        if let Some(err) = self.check_persona_visible(&persona) {
            return Ok(err);
        }

        let config = self.mcp_config();
        let request = RecallRequest {
            messages: params
                .messages
                .into_iter()
                .map(MessageParam::into_turn)
                .collect(),
            persona: Some(persona.clone()),
            query: params.query,
            include: params
                .include
                .map(|layers| layers.into_iter().map(Into::into).collect()),
            // 预算：请求值优先；缺省取 [mcp] 配置（服务层再做上限钳制）
            max_items: params.max_items.or(Some(config.max_items)),
            max_chars: params.max_chars.or(Some(config.max_chars)),
            // conversation_id 不在工具 schema 暴露，也不参与检索去重
            conversation_id: None,
        };

        match self.engine().recall(request).await {
            Ok(result) => {
                tracing::debug!(
                    persona = %persona,
                    items = result.items.len(),
                    mode = result.stats.mode.as_str(),
                    truncated = result.stats.truncated,
                    "memory_recall 完成"
                );
                Ok(success(&result))
            }
            Err(e) => {
                tracing::warn!(persona = %persona, error = %e, "memory_recall 执行失败");
                Ok(tool_error(format!(
                    "召回失败：{e}。可检查人格 uid 是否正确、库文件是否可读后重试"
                )))
            }
        }
    }
}
