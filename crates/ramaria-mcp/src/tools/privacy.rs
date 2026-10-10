//! crates/ramaria-mcp/src/tools/privacy.rs - 隐私确认工具（privacy_confirm）
//!
//! 设计特点:
//! - 调用即确认：记录当前后端的 provider + 地址组合，供线上生成工具通过隐私门禁
//! - 本地 provider 为无操作：回执说明"无需确认"，不写确认记录
//! - 幂等：重复确认为覆盖写入，语义与桌面「设置 → 后端」确认一致
//! - 隐私：返回只含 provider 与地址（不含 API key）；按 provider + base_url 粒度记录

use ramaria_service::entry_error_message;
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use crate::params::PrivacyConfirmParams;
use crate::result::{success, tool_error};
use crate::server::RamariaMcpServer;

#[rmcp::tool_router(router = privacy_router, vis = "pub(crate)")]
impl RamariaMcpServer {
    /// 记录线上 LLM 服务的隐私确认（本机落库，不涉及密钥）。
    #[rmcp::tool(
        description = "记录线上 LLM 服务的隐私确认（按 provider + 服务地址组合，本机落库、不涉及密钥）。\
调用即确认当前后端；本地模式调用为无操作（回执说明无需确认）。\
何时使用：线上 provider 首次调用 chat_send 被隐私门禁拒绝时（错误提示会指引本工具）。\
何时不要用：希望改换后端或模型时请走桌面「设置 → 后端」。",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn privacy_confirm(
        &self,
        Parameters(params): Parameters<PrivacyConfirmParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(err) = self.gate_enabled() {
            return Ok(err);
        }

        // 判定输入取 DB 侧后端配置（与服务层隐私用例同源；无记录按本地默认）
        let backend = match self.engine().backend_config().await {
            Ok(Some(config)) => config,
            Ok(None) => ramaria_core::types::BackendConfig::lm_studio_default(),
            Err(e) => {
                tracing::warn!(error = %e, "privacy_confirm 读取后端配置失败");
                return Ok(tool_error(format!(
                    "{}。请检查后端配置后重试（桌面「设置 → 后端」）",
                    entry_error_message(&e, "读取后端配置失败")
                )));
            }
        };

        if !backend.provider.is_online() {
            return Ok(success(&serde_json::json!({
                "confirmed": true,
                "provider": backend.provider.as_str(),
                "base_url": backend.base_url,
                "persistent": false,
                "note": "当前为本地模式，无需隐私确认",
            })));
        }

        let persistent = params.persistent.unwrap_or(true);
        match self.engine().confirm_privacy(persistent).await {
            Ok(()) => {
                tracing::info!(
                    provider = %backend.provider.as_str(),
                    persistent,
                    "隐私确认已记录"
                );
                Ok(success(&serde_json::json!({
                    "confirmed": true,
                    "provider": backend.provider.as_str(),
                    "base_url": backend.base_url,
                    "persistent": persistent,
                })))
            }
            Err(e) => {
                tracing::warn!(error = %e, "privacy_confirm 记录失败");
                Ok(tool_error(format!(
                    "{}。请检查后端配置后重试（桌面「设置 → 后端」）",
                    entry_error_message(&e, "隐私确认失败")
                )))
            }
        }
    }
}
