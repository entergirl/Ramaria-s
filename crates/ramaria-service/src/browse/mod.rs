//! crates/ramaria-service/src/browse/mod.rs - 记忆与会话浏览用例（查询用例组）
//!
//! 设计特点:
//! - 以只读为主：L1 / L2 / L3 / 性格画像 / 事实 / 证据链 / 会话列表 / 会话消息与详情，
//!   不修改状态；会话未读标记（标记已读）是唯一的写操作
//! - 单份实现覆盖两条调用链路：桌面口径与 CLI 口径的差异由请求参数表达（未吸收过滤 / 分页），
//!   不写第二份实现
//! - 逐段独立降级：消息计数聚合失败按 0 处理、证据链单条查询失败跳过，不阻塞整体读取
//! - 分页与上限钳制：单页消息上限 1000、L1 / L2 截断上限 1000、会话扫描上限 500、
//!   证据链事件扫描上限 5000，防御超大请求
//! - 隐私：视图为结构化数据（摘要 / 事件 / 性格 / 事实陈述），不含 utt 原文块
//!
//! 模块划分:
//! - `l1`：L1 记忆摘要浏览（按会话收集 / 未吸收口径）与按会话读取；
//! - `l2_l3`：L2 事件浏览与 L3 性格标签浏览；
//! - `profile`：L3 三层画像与画像数据状态；
//! - `evidence`：性格标签证据链（trait → 证据 → 事件 → L1 溯源）；
//! - `facts`：知识事实浏览（活跃事实 / 详情 / 分组与版本链）；
//! - `session`：会话浏览（列表聚合 / 消息分页 / 详情 / 计数）；
//! - `unread`：会话未读标记与汇总（标记已读 / 全局未读总数）；
//! - `channel`：通道会话概览（活跃数与最近活动时间）；
//! - `view`：跨域共享的浏览口径常量、输入归一与视图组装。

mod channel;
mod evidence;
mod facts;
mod l1;
mod l2_l3;
mod profile;
mod session;
mod unread;
mod view;

// 用例 re-export：`crate::browse::X` 为引擎既有调用路径
pub(crate) use channel::channel_overview;
pub(crate) use evidence::trait_evidence;
pub(crate) use facts::{fact_detail, facts, facts_grouped};
pub(crate) use l1::{l1, l1_by_session};
pub(crate) use l2_l3::{l2, l3};
pub(crate) use profile::{personality_profile, profile_status};
pub(crate) use session::{count_session_messages, session_detail, session_messages, sessions};
pub(crate) use unread::{mark_session_read, unread_total};

#[cfg(test)]
mod tests;
