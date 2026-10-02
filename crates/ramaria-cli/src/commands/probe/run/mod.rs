//! crates/ramaria-cli/src/commands/probe/run/mod.rs - 探针 probe run 档位实验模块入口
//!
//! 设计特点:
//! - 从命令入口接收数据集与参数档位，逐档位批量执行对话管线，收集输出与可测指标
//! - 支持 `--repeat N` 统计法：多次独立运行后跨轮配对聚合均值 / 标准差 / 95% 置信区间
//! - 隐私确认与静默降级：线上 provider 需确认，档位/单题失败记 warn 不中断批量
//! - 档位 utt 块按切分参数去重重建，top_k 变化复用已建块，避免 embedding 调用倍增
//! - 子模块按域拆分，对外名称在此逐项 re-export（`probe::run::X` 路径不变）

mod experiment;
mod session;

pub use experiment::{build_experiment, build_experiment_with_repeat};
pub(crate) use experiment::{metric_stat, run_experiment};

#[cfg(test)]
pub(crate) use experiment::aggregate_repeat_stats;
#[cfg(test)]
pub(crate) use experiment::filter_variants;
#[cfg(test)]
pub(crate) use experiment::t_critical_975;
#[cfg(test)]
pub(crate) use session::STATEMENT_REGISTER_LEAD;
#[cfg(test)]
pub(crate) use session::effective_question;
#[cfg(test)]
pub(crate) use session::run_validity;
#[cfg(test)]
pub(crate) use session::seed_history_from_context;
