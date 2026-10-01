//! crates/ramaria-mcp/src/tools/persona.rs - 人格读取工具（persona_list / persona_get）
//!
//! 设计特点:
//! - 只读工具：只读人格元数据与画像数据，不改变任何状态
//! - 白名单过滤：`persona_list` 只返回可见人格；`persona_get` 越权直接拒绝
//! - 分段可选：`persona_get` 可按分段取用，避免一次性拉取全部画像数据
//! - 空库语义：无人格时返回空列表（不是错误）

use ramaria_service::{PersonaCardRequest, entry_error_message};
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use crate::params::{PersonaGetParams, SectionParam};
use crate::result::{success, tool_error};
use crate::server::RamariaMcpServer;

#[rmcp::tool_router(router = persona_router, vis = "pub(crate)")]
impl RamariaMcpServer {
    /// 列出当前可见的人格（白名单外的人格不可见）。
    #[rmcp::tool(
        description = "列出 Ramaria 中可用的人格（uid / 名称 / 类型 / 来源 / 简介 / 启用状态）。\
何时使用：需要确认有哪些人格或取 persona uid 时（无参数）。\
何时不要用：需要某人格的画像细节时（请用 persona_get）。",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn persona_list(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }

        let policy = self.current_policy();
        match self.engine().persona_list().await {
            Ok(list) => {
                // 白名单过滤：越权人格不可见（与服务层策略同一口径）
                let visible: Vec<_> = list
                    .into_iter()
                    .filter(|persona| policy.persona_allowed(&persona.uid))
                    .collect();
                let count = visible.len();
                tracing::debug!(count, "persona_list 完成");
                Ok(success(&serde_json::json!({
                    "personas": visible,
                    "count": count,
                })))
            }
            Err(e) => {
                tracing::warn!(error = %e, "persona_list 执行失败");
                Ok(tool_error(entry_error_message(&e, "读取人格列表失败")))
            }
        }
    }

    /// 获取某人格的完整卡片（可选分段）。
    #[rmcp::tool(
        description = "获取某个 Ramaria 人格的完整卡片：性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度。\
何时使用：需要该人格的设定细节（例如为外部前端自组人设提示词）时，uid 必填、sections 可选。\
何时不要用：只需要人格清单时（请用 persona_list）。",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn persona_get(
        &self,
        Parameters(params): Parameters<PersonaGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }

        let uid = params.uid.trim().to_string();
        if uid.is_empty() {
            return Ok(tool_error("uid 不能为空：请传入目标人格 uid"));
        }
        if let Some(err) = self.check_persona_visible(&uid) {
            return Ok(err);
        }

        let request = PersonaCardRequest {
            uid: uid.clone(),
            sections: params
                .sections
                .map(|sections| sections.into_iter().map(SectionParam::into).collect()),
        };

        match self.engine().persona_card(request).await {
            Ok(card) => {
                tracing::debug!(uid = %uid, "persona_get 完成");
                Ok(success(&card))
            }
            Err(e) => {
                tracing::warn!(uid = %uid, error = %e, "persona_get 执行失败");
                Ok(tool_error(format!(
                    "{}。可先用 persona_list 确认 uid 是否正确",
                    entry_error_message(&e, "读取人格卡片失败")
                )))
            }
        }
    }
}
