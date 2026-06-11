//! ramaria-cli lib — 为集成测试暴露 CLI 命令模块
//!
//! 说明:
//! - 命令模块原先声明在 main.rs 的私有 `mod` 中，集成测试无法访问。
//! - 此 lib.rs 重新声明所有模块为 pub，使 `tests/` 目录可直接使用。
//! - main.rs 通过 `use ramaria_cli::...` 引用，而非私有 mod。

pub mod commands;
pub mod privacy;
pub mod ui;
pub mod util;
