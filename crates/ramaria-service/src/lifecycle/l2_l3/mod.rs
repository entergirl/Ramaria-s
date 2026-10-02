//! crates/ramaria-service/src/lifecycle/l2_l3/mod.rs - Ramaria L2 事件提取与 L3 性格推断调度模块
//!
//! 设计特点:
//! - 触发与执行分离：`l2` / `l3` 只做触发判定与单轮执行，`schedule` 负责后台定时编排
//! - L2 提取经 `JobManager` 包裹（指数退避重试）；成功后按开关执行知识事实抽取并级联 L3
//! - L3 全流程：Phase A 统计 + 分层收缩 → Phase B LLM 推断 → Phase C 置信度更新 + 漂移检测
//! - 无主 L1（数据断层修复）：按来源会话归属 persona 后进入标准 L2 提取链路
//! - 所有 LLM 失败均不阻塞级联，仅记日志降级；停止位由宿主以共享原子标志传入
//!
//! 模块划分:
//! - `l2`：L2 触发检查与提取执行（含共享停止位判定）；
//! - `unbound`：无主 L1 归属与触发处理（数据断层修复）；
//! - `l3`：L3 触发检查、性格推断全流程与首轮判定；
//! - `schedule`：后台定时任务与单轮定时检查。

mod l2;
mod l3;
mod schedule;
mod unbound;

pub(crate) use l2::check_l2_trigger;
pub(crate) use l3::check_l3_trigger;
pub(crate) use schedule::spawn_scheduler;

#[cfg(test)]
mod tests;
