//! crates/ramaria-service/tests/suites/smoke.rs - 交互式生成状态门禁用例
//!
//! 设计特点:
//! - 未就绪状态拒绝交互式生成，且不创建会话
//! - 以最小装配（MockStorage + MockLlm）验证状态门禁先于落库

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{Persona, PersonaKind};

use crate::support::engine_env::{build_engine, stream_request, try_send_stream};
use crate::support::mock_backend::{MockLlm, MockStorage};

/// 固定助手回复（断言直接用）。
const REPLY: &str = "好的，我记住了。";

/// 构造带一个角色人格的空存储。
fn storage_with_persona() -> Arc<MockStorage> {
    let storage = Arc::new(MockStorage::new());
    storage.add_persona(Persona::new(
        "char-0001".to_string(),
        "小菌".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    ));
    storage
}

/// 状态门禁：未就绪的交互式生成被拒绝，且不产生会话。
#[tokio::test]
async fn interactive_generate_rejected_before_ready() {
    let storage = storage_with_persona();
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::new(MockLlm::new(REPLY)),
        RamariaConfig::default(),
    );

    let err = try_send_stream(&engine, stream_request("你好", Some("char-0001"), None))
        .await
        .expect_err("未就绪状态应拒绝交互式生成");
    assert_eq!(err.category(), "validation", "应为业务校验类错误");

    let sessions = storage.list_sessions().await.expect("读取会话列表应成功");
    assert!(sessions.is_empty(), "被拒的生成不应创建会话");
}
