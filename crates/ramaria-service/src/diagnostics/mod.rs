//! crates/ramaria-service/src/diagnostics/mod.rs - 诊断信息导出用例
//!
//! 设计特点:
//! - 收集：日志(最近1000行)、配置(API key 脱敏)、数据库 schema 版本、系统信息。
//! - 打包为 .zip 文件供用户手动发送给开发者排查问题。
//! - 所有敏感信息（API key）在收集阶段即脱敏，写入 zip 前已安全。
//! - 先写同目录临时文件，成功后 `fs::rename` 原子替换目标，避免中断留下半成品覆盖旧文件。
//! - 收集阶段错误不阻塞导出：缺失项记录占位文本而非报错退出。
//!
//! 安全约束:
//! - API key 脱敏使用 `[REDACTED]` 替换，不可逆。
//! - 不收集用户对话内容、记忆数据等隐私信息。
//! - 打包前对日志与配置做**二次脱敏**：绝对路径 → 仅保留文件名；消息类字段
//!   的值（preview/content/message/...）→ 字符数占位（`<N chars>`），
//!   杜绝原文片段随诊断包离开本机。
//! - 输出路径的安全性由调用方保证（入口层的路径防护策略不属本层职责）。
//!
//! 模块划分:
//! - `export`：导出用例入口与请求 / 结果类型；
//! - `collect`：信息采集（系统信息 / 日志 / 配置 / 检索索引构建状态）；
//! - `redact`：敏感信息脱敏（API key / 绝对路径 / 消息类字段）；
//! - `render`：zip 打包与渲染（临时文件 + 原子替换）。

mod collect;
mod export;
mod redact;
mod render;

// 类型 re-export：`crate::diagnostics::{DiagnosticsReport, DiagnosticsRequest}` 为入口层既有调用路径
pub use export::{DiagnosticsReport, DiagnosticsRequest};

// 用例入口 re-export：`crate::diagnostics::export` 为引擎既有调用路径
pub(crate) use export::export;

// 脱敏原语 re-export：`crate::diagnostics::redact_for_export` 为索引失败原因脱敏的既有调用路径
pub(crate) use redact::redact_for_export;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
