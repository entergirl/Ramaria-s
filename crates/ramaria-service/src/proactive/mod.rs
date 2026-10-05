//! crates/ramaria-service/src/proactive/mod.rs - Ramaria 主动对话服务模块入口
//!
//! 设计特点:
//! - 承载主动对话的服务层支撑：运行时状态持久化与读写封装
//! - 运行时状态存 `settings` 表（JSON 文本），重启保持，不新建表
//! - 命名说明: `[proactive]` 配置组与行为层 `proactiveness` 参数无语义关联——
//!   前者控制主动对话调度的开关与打扰控制，后者是行为规则的结构化参数
//! - 生成形态：主动生成指令与结果（选题器 → 生成用例 → 调度投递）为跨模块契约，
//!   生成用例复用统一生成链路的主动模式
//! - 调度循环与打扰控制：单轮判定链（硬闸门 → 活跃时段门 → 判据节流 → 选题 →
//!   生成 → 投放），状态按人格隔离且处理尾部统一回写
//! - 人格开关：三态（自动 / 手动开 / 手动关）按画像存于 `settings` 表，
//!   由调度在资格闸门读取
//! - 投递注册：宿主实现接收端 trait 并注册到引擎；未注册时调度静默丢弃
//! - 活跃时段统计：user 消息时间直方图 + 软加权（权重设下限，样本不足退化放行）

mod activity;
mod judge;
mod picker;
mod schedule;
mod sink;
mod state;
mod switch;
mod topic;

pub use sink::{ProactiveMessage, ProactiveSink};

pub(crate) use picker::PickerTopicProvider;
pub(crate) use schedule::{TopicPicker, spawn};
pub(crate) use topic::{ProactiveDirective, ProactiveOutcome};

#[cfg(test)]
mod tests;
