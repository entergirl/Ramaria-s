//! crates/ramaria-service/src/recall/mod.rs - 召回用例（memory_recall 的服务层实现）
//!
//! 设计特点:
//! - 召回同源：记忆层检索走 `ramaria_memory::recall::assemble_recall`（与在线管线同一份
//!   实现），服务层只负责"分层装配 + 预算裁剪 + 结构化输出"
//! - 分层装配按注入优先级排列：行为 > 知识 > 表达（风格） > 脉络 > 记忆（L1/L2/L3） > 原文
//! - 两种模式：`Search`（有检索输入）与 `Overview`（无 query 且无 messages，按时间线概览）
//! - 隐私策略在服务层执行：人格白名单（`allowed_personas`）与原文开关（`allow_raw_text`）
//!   不满足时直接拒绝或不出原文，保证「策略执行」不依赖协议壳
//! - 预算纪律：`max_items` 上限 20（超出截断）、`max_chars` 按字符边界截断，均计入 stats
//! - 静默降级：知识 / 风格 / 画像 / 行为读取失败均记 warn 并按空处理，不阻塞召回
//!
//! 未接线说明:
//! - `RecallRequest.conversation_id`（"当前外部对话库内历史参与检索去重"）当前仅作数据属性
//!   透传，尚未参与检索去重；接线点为 `list_messages_by_channel_ref`。
//!
//! 模块划分:
//! - `policy`：召回隐私与边界策略（配置映射 / 链式覆盖 / 白名单判定）；
//! - `entry`：用例入口（策略校验、请求归一化、检索 / 概览分流）；
//! - `search`：检索模式分层装配与预算裁剪（记忆层共用召回）；
//! - `overview`：概览模式时间线装配与渲染；
//! - `layers`：各辅助分层读取与条目映射（行为 / 知识 / 风格 / 脉络 / 画像）。

mod entry;
mod layers;
mod overview;
mod policy;
mod search;

// 策略 re-export：`crate::recall::RecallPolicy` 为引擎与入口层既有调用路径
pub use policy::RecallPolicy;

// 用例入口 re-export：`crate::recall::run` 为引擎既有调用路径
pub(crate) use entry::run;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
