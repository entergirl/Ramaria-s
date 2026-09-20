//! crates/ramaria-mcp/src/tools/mod.rs - 工具实现分组
//!
//! 设计特点:
//! - 一个工具组一个文件：召回（recall）/ 生成（chat）/ 写入（ingest）/ 人格（persona）/
//!   历史（history）
//! - 每个文件自带 `#[tool_router]` 分组路由，协议壳在 `server` 汇总
//! - 实现纪律：只做参数转换 + 门禁 + 调用服务层用例 + 结果包装，不写业务逻辑
//! - 错误口径：可预期的用户 / 模型侧问题（开关关闭、参数缺失、白名单拒绝）走 `isError` 结果

pub mod chat;
pub mod history;
pub mod ingest;
pub mod persona;
pub mod recall;
