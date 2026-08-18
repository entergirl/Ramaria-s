//! crates/ramaria-app/src/commands/mod.rs - 用例命令模块入口
//!
//! 设计特点:
//! - behavior: 行为层用例（学习/路由/规则管理/反馈环）
//! - 后续 v1.6/v1.7 新用例（probe evaluate 等）按需在此注册

pub mod behavior;
pub mod fact;
