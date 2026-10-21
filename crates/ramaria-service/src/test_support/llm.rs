//! crates/ramaria-service/src/test_support/llm.rs - 服务层测试 LLM mock 出口
//!
//! 设计特点:
//! - 单份实现下沉到 ramaria-testkit：本模块只做 crate 内转出
//! - 保持 test_support::MockLlm / ScriptedLlm 名称与构造 API 稳定
//! - 调用序列、vision、健康探测语义随 testkit 统一维护

pub(crate) use ramaria_testkit::mock::{MockLlm, ScriptedLlm};
