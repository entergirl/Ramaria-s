//! crates/ramaria-eval/src/lib.rs - Ramaria 评测工具层入口
//!
//! 设计特点:
//! - dev-only：不随安装包发布；产品能力层（core / service 等）不依赖本 crate，仅 CLI 入口接入
//! - 依赖面仅 core / service：需要存储或检索句柄时经服务层取用
//! - 按统计、测量、命令三域分模块，统计与测量为纯计算零 I/O
//! - 命令层只做参数承载与结果返回，沙盒界面与运行器在后续里程碑接入
//! - 评测装配使用独立配置快照、不拉起后台循环
//! - 评测只写评测根下的库与产物，不触碰用户库

pub mod assembly;
pub mod command;
pub mod measure;
pub mod stats;

pub use assembly::{EVAL_CONFIG_FILE, EVAL_DB_FILE, open_engine, write_config_snapshot};
pub use command::{EvalCommand, run};
