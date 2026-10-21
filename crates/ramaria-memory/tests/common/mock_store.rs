//! crates/ramaria-memory/tests/common/mock_store.rs - 集成测试 mock 存储出口
//!
//! 设计特点:
//! - 单份实现下沉到 ramaria-testkit：本文件只做路径转出
//! - 保持 MockStorage / MockLlm / MockFailingLlm 名称稳定，调用点无需感知位置
//! - 每个测试二进制独立编译本模块，各目标使用子集不同 → 允许未使用项

#![allow(dead_code)]

pub use ramaria_testkit::mock::{MockFailingLlm, MockLlm, MockStorage};
