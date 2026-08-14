//! crates/ramaria-desktop/src/commands/mod.rs - Tauri Commands 模块入口
//!
//! 设计特点:
//! - 聚合所有 Tauri Command 子模块
//! - 每个子模块只做参数转换 + 调用 ramaria_app，不写业务逻辑

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
