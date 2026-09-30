//! crates/ramaria-service/tests/suites/support/mod.rs - 服务层用例集成测试基建出口
//!
//! 设计特点:
//! - `mock_backend`：内存存储 + 脚本化 / 可失败 mock LLM + 预计算嵌入，零网络零真实模型
//! - `engine_env`：引擎装配、就绪推进、事件流消费与完整封存钩子辅助
//! - 断言口径：只读取注入依赖的可观测状态（内存表 / 事件序列），不绕过被测用例
//! - 每个测试二进制独立编译本模块，各目标使用子集不同 → 允许未使用项

#![allow(dead_code)]

pub mod engine_env;
pub mod mock_backend;
