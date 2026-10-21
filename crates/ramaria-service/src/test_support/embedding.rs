//! crates/ramaria-service/src/test_support/embedding.rs - 服务层测试嵌入 mock 出口
//!
//! 设计特点:
//! - 单份实现下沉到 ramaria-testkit：本模块只做 crate 内转出
//! - 保持 test_support::DeterministicEmbedding / CountingEmbedding 名称与构造 API 稳定
//! - 确定性向量、调用计数与批量失败注入随 testkit 统一维护

pub(crate) use ramaria_testkit::mock::embedding::{CountingEmbedding, DeterministicEmbedding};
