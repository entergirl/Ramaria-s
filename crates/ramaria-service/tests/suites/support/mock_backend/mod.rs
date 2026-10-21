//! crates/ramaria-service/tests/suites/support/mock_backend/mod.rs - 服务层用例 mock 出口
//!
//! 设计特点:
//! - 单份实现下沉到 ramaria-testkit：本模块只做路径转出
//! - 保持 mock_backend::MockStorage / MockLlm / MockFailingLlm / MockEmbedding 名称稳定
//! - 测试用例不需要感知 mock 的存放位置

pub use ramaria_testkit::mock::{MockEmbedding, MockFailingLlm, MockLlm, MockStorage};
