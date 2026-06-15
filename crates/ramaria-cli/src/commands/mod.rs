//! rust/crates/ramaria-cli/src/commands/mod.rs - CLI 命令模块入口
//!
//! 设计特点:
//! - 每个命令一个子模块，职责单一
//! - 所有命令接收 `Arc<ramaria_app::App>` 作为统一依赖
//! - 命令函数返回 `anyhow::Result<()>`，错误由 main.rs 统一处理

pub mod ask;
pub mod chat;
pub mod config;
pub mod diagnostics;
pub mod export;
pub mod import_cmd;
pub mod index_cmd;
pub mod memory;
pub mod persona;
pub mod session;
pub mod setup;
