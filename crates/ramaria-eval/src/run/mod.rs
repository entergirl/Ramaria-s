//! crates/ramaria-eval/src/run/mod.rs - Ramaria 评测运行入口
//!
//! 设计特点:
//! - 按职责分文件：矩阵定义、分层抽样、题目枚举、条件工作区、运行状态、运行控制、上下文装配、单题执行、条件内管线、运行器
//! - 矩阵只描述轴与条件，工作区只准备库与快照，单题执行只驱动生成，条件内管线负责并发与有序写出，运行器只编排循环
//! - 档位产物是题目与基线库的唯一来源，运行期不再回读语料
//! - 状态与逐题产物分文件：状态可重入，逐题产物只追加
//! - 取消与进度事件只在 eval 内部：CLI 不订阅时行为与未接入时一致
//! - 错误统一走 core 结构化错误，不吞错也不 panic

mod context;
mod control;
mod exec;
mod item;
mod matrix;
mod oracle_context;
mod pipeline;
mod plan;
mod runner;
mod sample;
mod state;
mod workspace;

pub use context::build_full_context;
#[cfg(test)]
pub(crate) use control::ConditionPlan;
pub(crate) use control::{ItemEvent, RunControl, RunEvent, RunStarted};
pub use item::{ConditionLine, RunItemRecord};
pub use matrix::{AnswerModel, AnswerModelKind, Condition, Pipeline, RunMatrix, RunSection};
pub use plan::{BaselineTier, RunItem, plan_items};
pub(crate) use plan::{PlanCounts, plan_counts};
pub use runner::{RunReport, run, should_skip};
pub(crate) use runner::{execute, load_tiers, run_with_control};
pub(crate) use state::RUN_ARTIFACT_KIND;
pub use state::{ConditionState, ItemKey, RunState, RunStatus, RunTierRef};
pub use workspace::{BaselineProduct, ITEMS_FILE, RUN_FILE, RunWorkspace, WORK_DIR};
