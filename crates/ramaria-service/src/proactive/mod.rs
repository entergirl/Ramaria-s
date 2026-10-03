//! crates/ramaria-service/src/proactive/mod.rs - Ramaria 主动对话服务模块入口
//!
//! 设计特点:
//! - 承载主动对话的服务层支撑：运行时状态持久化与读写封装
//! - 运行时状态存 `settings` 表（JSON 文本），重启保持，不新建表
//! - 命名说明: `[proactive]` 配置组与行为层 `proactiveness` 参数无语义关联——
//!   前者控制主动对话调度的开关与打扰控制，后者是行为规则的结构化参数
//!
//! 后续扩展（调度循环 / 选题器 / 投放 sink）按同一目录收敛。

// 状态读写供主动对话调度消费；调度装配接入前，非测试构建下保留定义。
#[allow(dead_code)]
mod state;
