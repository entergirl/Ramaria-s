//! crates/ramaria-eval/src/run/mod.rs - Ramaria 评测运行入口
//!
//! 设计特点:
//! - 按职责分文件：矩阵定义、分层抽样、题目枚举、条件工作区、运行状态、上下文装配、单题执行、条件内管线、运行器
//! - 矩阵只描述轴与条件，工作区只准备库与快照，单题执行只驱动生成，条件内管线负责并发与有序写出，运行器只编排循环
//! - 档位产物是题目与基线库的唯一来源，运行期不再回读语料
//! - 状态与逐题产物分文件：状态可重入，逐题产物只追加
//! - 错误统一走 core 结构化错误，不吞错也不 panic

mod context;
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
pub use item::{ConditionLine, RunItemRecord};
pub use matrix::{AnswerModel, AnswerModelKind, Condition, Pipeline, RunMatrix, RunSection};
pub use plan::{BaselineTier, RunItem, plan_items};
pub use runner::{RunReport, run, should_skip};
pub(crate) use runner::{execute, load_tiers};
pub use state::{ConditionState, ItemKey, RunState, RunStatus, RunTierRef};
pub use workspace::{BaselineProduct, ITEMS_FILE, RUN_FILE, RunWorkspace, WORK_DIR};
