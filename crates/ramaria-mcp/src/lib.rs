//! crates/ramaria-mcp/src/lib.rs - Ramaria MCP 服务端（协议壳）
//!
//! 设计特点:
//! - 协议壳定位：只做工具注册、参数校验、结果包装与错误映射；业务一律经 `ramaria-service`
//! - 依赖纪律：只依赖 `ramaria-service` 与 `ramaria-core`，禁止 app / cli / desktop / tauri
//! - stdio 传输：stdout 只允许协议消息，日志一律走 stderr（见 `host`）
//! - 工具错误放结果内返回（`isError: true` + 可操作描述），不使用协议级错误
//! - 开关面：`[mcp]` 配置组（总开关 / 写入 / 封存 / 人格白名单 / 原文块 / 默认预算）
//! - 会话标识：外部会话统一落 `channel = "mcp"`，`external_ref` 取显式标识或客户端身份名
//!
//! 模块划分:
//! - `host`：宿主装配与 stdio 服务循环（引擎 / 策略 / 默认钩子 / 日志）；
//! - `server`：服务端主体（协议处理与工具路由汇总）；
//! - `params`：工具入参（wire 类型，含 JSON Schema）；
//! - `result`：工具结果与错误构造（JSON 文本 + `isError`）；
//! - `tools`：各工具实现（召回 / 写入 / 人格 / 历史）。

pub mod host;
pub mod params;
pub mod result;
pub mod server;
pub mod tools;

pub use host::{McpHostOptions, init_stderr_logging, serve_stdio};
pub use server::RamariaMcpServer;
