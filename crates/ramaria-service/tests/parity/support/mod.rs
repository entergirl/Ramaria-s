//! crates/ramaria-service/tests/parity/support/mod.rs - 平行对照测试基建出口
//!
//! 设计特点:
//! - 环境（`env`）：`ParityEnv` 提供"临时 SQLite + 脚本化 mock LLM + 预计算向量 mock 嵌入"的
//!   隔离环境；用例在真实存储语义上执行，LLM 与嵌入完全离线且确定
//! - 造数（`fixtures`）：persona / 会话 / 消息 / L1 的固定口径造数，四条路径共用，避免各写一套
//! - 快照（`snapshot`）：把用例输出规范化为稳定 JSON（键排序 / 浮点取整）；对照不等价时
//!   输出逐路径差异报告，便于定位行为漂移
//! - 基线（`golden`）：把规范化快照冻结为文件基线；缺失时生成、不一致时报错并提示更新方式
//! - 日志（`log`）：按需把服务层 tracing 日志输出到 stderr（`PARITY_LOG` 环境变量控制），
//!   默认静默；断言失败始终输出差异报告
//! - 错误（`error`）：基建侧统一错误类型 `ParityError`，环境构建失败给出可读原因与路径

pub mod env;
pub mod error;
pub mod fixtures;
pub mod golden;
pub mod log;
pub mod mocks;
pub mod snapshot;

pub use env::ParityEnv;
pub use error::{ParityError, ParityResult};
pub use golden::GoldenStore;
pub use mocks::ScriptedLlm;
pub use snapshot::{Snapshot, assert_parity, assert_stable};
