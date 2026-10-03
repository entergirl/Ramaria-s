//! crates/ramaria-mcp/src/server.rs - MCP 服务端主体（协议处理与工具路由汇总）
//!
//! 设计特点:
//! - 协议处理只依赖 `ramaria-service` 的用例入口，不含检索 / 封存 / 装配实现
//! - 工具路由汇总：各工具组（召回 / 写入 / 人格 / 历史）各自声明路由，此处合并
//! - 门禁口径：总开关（`enabled`）与写入开关（`allow_ingest`）在工具入口统一执行，
//!   失败以结果内 `isError` 返回并给出可操作提示
//! - 白名单口径：人格可见性在协议壳做前置校验（服务层策略再次执行，双重保险）
//! - 会话标识：`external_ref` 三级规则 = 显式 `conversation_id` > 客户端身份名 > 单流退化
//! - 客户端身份名在 `initialize` 握手时记录（`clientInfo.name`），用于会话续写定位

use std::future::Future;
use std::sync::{Arc, RwLock};

use ramaria_core::config::McpConfig;
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_service::{Engine, RecallPolicy};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    CallToolResult, Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{RoleServer, ServerHandler};

use crate::result::tool_error;

/// 单流退化时的会话标识（客户端未提供 `conversation_id` 且身份名未知）。
pub(crate) const FALLBACK_EXTERNAL_REF: &str = "mcp-default";

/// 服务端自我介绍（客户端 `initialize` 时返回）。
///
/// 说明:
/// - 只写"何时该用哪类工具"的使用建议，不写实现细节（instructions 会进入客户端上下文）；
/// - 长度控制在数十字级别，避免占用客户端 token 预算。
const SERVER_INSTRUCTIONS: &str = "Ramaria 本地记忆与人格服务：需要个性化上下文时先调用 memory_recall；\
     外部对话结束后用 chat_ingest 回流，使内容进入记忆加工；\
     人格信息用 persona_list / persona_get 查询。所有数据来自本机，不含云端内容。";

/// MCP 服务端（协议壳）。
///
/// 职责:
/// - 持有服务层引擎与 `[mcp]` 配置快照，向各工具提供统一入口；
/// - 汇总工具路由并实现 MCP 协议处理（握手 / 工具列表 / 工具调用）。
///
/// 状态:
/// - `client_name`: 客户端身份名（`initialize` 握手写入，缺省未知）；
/// - 引擎与配置为进程级不变引用，工具调用期间只读。
pub struct RamariaMcpServer {
    engine: Arc<Engine>,
    config: Arc<McpConfig>,
    client_name: Arc<RwLock<Option<String>>>,
}

impl RamariaMcpServer {
    /// 构造服务端（引擎已装配、策略与钩子已注入，见 `host`）。
    ///
    /// 说明:
    /// - 门禁（召回策略 / 封存许可）由宿主装配时注入服务层，本构造器不重复注入；
    ///   直接构造服务端的调用方（测试）需自行补上宿主步骤（见 `tests/protocol_e2e.rs`）。
    pub fn new(engine: Arc<Engine>, config: McpConfig) -> Self {
        Self {
            engine,
            config: Arc::new(config),
            client_name: Arc::new(RwLock::new(None)),
        }
    }

    /// 服务层引擎引用（工具实现使用）。
    pub(crate) fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// `[mcp]` 配置快照引用。
    pub(crate) fn mcp_config(&self) -> &McpConfig {
        &self.config
    }

    /// 全部工具路由（各工具组路由合并）。
    ///
    /// 说明:
    /// - 与 `#[tool_handler]` 的默认路由表达式（`Self::tool_router()`）衔接；
    /// - 工具组分工见 `crate::tools`。
    pub fn tool_router() -> ToolRouter<Self> {
        Self::recall_router()
            + Self::chat_router()
            + Self::ingest_router()
            + Self::persona_router()
            + Self::history_router()
    }

    // =========================================================
    // 门禁与前置校验
    // =========================================================

    /// 总开关门禁：未开启时所有工具返回可操作错误。
    ///
    /// 说明:
    /// - 配置解析失败会静默回退默认配置（总开关随默认值为关闭），
    ///   文案在此时附加回退原因与修复提示，避免"配置过却被要求重新开启"的困惑。
    pub(crate) fn gate_enabled(&self) -> Option<CallToolResult> {
        if self.config.enabled {
            return None;
        }
        Some(tool_error(self.gate_disabled_message()))
    }

    /// 总开关关闭时的错误文案（有配置回退告警时附带原因与修复提示）。
    fn gate_disabled_message(&self) -> String {
        const BASE: &str = "MCP 接入未开启：请在 Ramaria 桌面「设置 → MCP 接入」中打开总开关后重试";
        match self.engine.config_warning() {
            Some(warning) => format!(
                "{BASE}。注意：{warning}——若你曾配置过 [mcp] 接入，请修正配置文件语法后重启本服务"
            ),
            None => BASE.to_string(),
        }
    }

    /// 写工具门禁（`chat_ingest`）。
    pub(crate) fn gate_ingest(&self) -> Option<CallToolResult> {
        if self.config.allow_ingest {
            return None;
        }
        Some(tool_error(
            "外部写入已被禁用（[mcp].allow_ingest = false）：请在 Ramaria 设置中开启「允许外部写入回流」后重试",
        ))
    }

    /// 人格可见性前置校验（与服务层策略同一口径）。
    ///
    /// 返回:
    /// - `None`: 该人格在可见白名单内（或白名单为 `*`）。
    /// - `Some(错误结果)`: 越权访问，提示去设置中调整白名单。
    pub(crate) fn check_persona_visible(&self, persona_uid: &str) -> Option<CallToolResult> {
        if self.current_policy().persona_allowed(persona_uid) {
            return None;
        }
        Some(tool_error(format!(
            "人格 {persona_uid} 不在 MCP 可见白名单内（[mcp].allowed_personas）；请在 Ramaria 设置中调整后重试"
        )))
    }

    /// 当前召回策略快照（人格白名单 / 原文开关由宿主按 `[mcp]` 注入）。
    pub(crate) fn current_policy(&self) -> RecallPolicy {
        self.engine.recall_policy()
    }

    /// 解析外部对话标识（会话标识三级规则）。
    ///
    /// 参数:
    /// - `provided`: 工具调用显式传入的 `conversation_id`。
    ///
    /// 返回:
    /// - 显式标识（去空白后非空）优先；
    /// - 否则用客户端身份名（`initialize` 记录）；
    /// - 都缺省时退化为单流标识（同人格一条流，按空闲切段）。
    pub(crate) fn external_ref(&self, provided: Option<&str>) -> String {
        if let Some(value) = provided.map(str::trim).filter(|v| !v.is_empty()) {
            return value.to_string();
        }
        let client = read_recover(&self.client_name, "mcp.client_name");
        match client.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            Some(name) => name.to_string(),
            None => FALLBACK_EXTERNAL_REF.to_string(),
        }
    }
}

// =========================================================
// MCP 协议处理（ServerHandler）
// =========================================================

#[rmcp::tool_handler]
impl ServerHandler for RamariaMcpServer {
    /// 服务端自查信息（能力声明 + 自我介绍 + 使用建议）。
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ramaria", env!("CARGO_PKG_VERSION")))
            .with_instructions(SERVER_INSTRUCTIONS)
    }

    /// 握手：记录客户端身份名并协商协议版本。
    ///
    /// 说明:
    /// - 客户端身份名（`clientInfo.name`）是会话标识三级规则的第二级：
    ///   客户端未显式传 `conversation_id` 时，用它区分不同客户端的会话流；
    /// - 其余行为与默认实现一致（写入 peer info + 版本协商）。
    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, rmcp::ErrorData>> + MaybeSendFuture + '_
    {
        {
            let mut guard = write_recover(&self.client_name, "mcp.client_name");
            *guard = Some(request.client_info.name.clone());
        }
        tracing::info!(
            client = %request.client_info.name,
            version = %request.client_info.version,
            "MCP 客户端握手（已记录客户端身份名，用于会话标识）"
        );
        context.peer.set_peer_info(request.clone());
        std::future::ready(self.negotiate_initialize(&request))
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 工具路由汇总：注册的工具清单与契约一致（六个工具的名称集合）。
    #[test]
    fn tool_router_exposes_contract_tools() {
        let router = RamariaMcpServer::tool_router();
        let names: Vec<String> = router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        for expected in [
            "memory_recall",
            "chat_send",
            "chat_ingest",
            "persona_list",
            "persona_get",
            "chat_history",
        ] {
            assert!(names.contains(&expected.to_string()), "缺少工具 {expected}");
        }
        assert_eq!(names.len(), 6, "工具数量应与注册实现一致：{names:?}");
    }

    /// 生成工具：非只读、非幂等、可能访问外部（LLM 在远端）。
    #[test]
    fn chat_send_tool_annotations() {
        let router = RamariaMcpServer::tool_router();
        let tool = router.get("chat_send").expect("chat_send 应已注册");
        let annotations = tool.annotations.clone().expect("应显式设置注解");
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(false), "生成非幂等");
        assert_eq!(annotations.open_world_hint, Some(true), "可能调用远端 LLM");

        let required = tool
            .input_schema
            .get("required")
            .and_then(|value| value.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(
            required.contains(&"message".to_string()),
            "message 应为必填参数：{required:?}"
        );
    }

    /// 工具 schema：`memory_recall` 必填 `messages`，且注解为只读 + 闭合世界。
    #[test]
    fn recall_tool_schema_and_annotations() {
        let router = RamariaMcpServer::tool_router();
        let tool = router
            .get("memory_recall")
            .expect("memory_recall 应已注册")
            .clone();

        let required = tool.input_schema.get("required").and_then(|v| v.as_array());
        let required: Vec<String> = required
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            required.contains(&"messages".to_string()),
            "messages 应为必填参数：{required:?}"
        );

        let annotations = tool.annotations.clone().expect("应显式设置注解");
        assert_eq!(annotations.read_only_hint, Some(true), "召回为只读工具");
        assert_eq!(
            annotations.open_world_hint,
            Some(false),
            "召回不访问外部世界"
        );
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));

        assert!(
            tool.description
                .as_deref()
                .unwrap_or_default()
                .contains("何时"),
            "描述应包含使用时机说明（做什么 + 何时用/不用）"
        );
    }

    /// 工具 schema：`chat_ingest` 为可重复提交（幂等）的非只读工具。
    #[test]
    fn ingest_tool_annotations_and_schema() {
        let router = RamariaMcpServer::tool_router();
        let tool = router.get("chat_ingest").expect("chat_ingest 应已注册");
        let annotations = tool.annotations.clone().expect("应显式设置注解");
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true), "指纹去重保证幂等");
        assert_eq!(annotations.destructive_hint, Some(false));
    }

    /// 只读工具注解：人格与历史查询均标注只读。
    #[test]
    fn read_only_tools_are_marked() {
        let router = RamariaMcpServer::tool_router();
        for name in ["persona_list", "persona_get", "chat_history"] {
            let tool = router.get(name).expect("只读工具应已注册");
            let annotations = tool.annotations.clone().expect("应显式设置注解");
            assert_eq!(
                annotations.read_only_hint,
                Some(true),
                "{name} 应标注 read_only_hint=true"
            );
        }
    }

    /// 工具定义体量与完整性：描述总量受限、每个工具都带描述与四个注解。
    ///
    /// 说明:
    /// - 工具定义会整段进入客户端上下文，描述膨胀会挤占对话预算；
    ///   目标 ≤ 3K token，按中英混排保守折算 1 token ≈ 3 字符 → 描述总量 ≤ 9000 字符。
    /// - 四个注解（readOnly / destructive / idempotent / openWorld）必须显式设置，
    ///   让客户端能据此判断调用风险。
    #[test]
    fn tool_definitions_stay_within_budget_and_are_annotated() {
        let router = RamariaMcpServer::tool_router();
        let mut total_desc_chars = 0usize;
        for tool in router.list_all() {
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                !description.trim().is_empty(),
                "{} 缺少描述（需写清做什么 + 何时用/不用）",
                tool.name
            );
            total_desc_chars += description.chars().count();

            let annotations = tool.annotations.clone().expect("应显式设置注解");
            assert!(
                annotations.read_only_hint.is_some()
                    && annotations.destructive_hint.is_some()
                    && annotations.idempotent_hint.is_some()
                    && annotations.open_world_hint.is_some(),
                "{} 四个注解必须显式设置",
                tool.name
            );
        }
        assert!(
            total_desc_chars <= 9000,
            "工具描述总字符数 {total_desc_chars} 超出预算（目标 ≤ 3K token）"
        );
    }
}
