//! crates/ramaria-service/src/proactive/sink/tests.rs - 主动消息投递接收端单元测试
//!
//! 设计特点:
//! - 覆盖未注册 / 注册 / 替换三态：注册后引擎可读回同一句柄，替换后旧接收端不再被调用
//! - 使用真实 SQLite 临时库装配引擎，验证注册槽位与生产同路径
//! - 投递负载为合成数据，不依赖网络 / LLM

use super::*;
use crate::test_support::engine_with_db;
use std::sync::{Arc, Mutex};

/// 记录型接收端（投递负载留档供断言）。
struct RecordingSink {
    received: Mutex<Vec<ProactiveMessage>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
        }
    }

    fn received(&self) -> Vec<ProactiveMessage> {
        self.received.lock().expect("接收端留档锁不应中毒").clone()
    }
}

impl ProactiveSink for RecordingSink {
    fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        self.received
            .lock()
            .expect("接收端留档锁不应中毒")
            .push(message.clone());
        Ok(())
    }
}

/// 构造一条投递负载（合成数据）。
fn sample_message() -> ProactiveMessage {
    ProactiveMessage {
        message_id: Uuid::new_v4(),
        content: "主动消息内容".to_string(),
        session_id: Uuid::new_v4(),
        persona: "char-0001".to_string(),
        source: "event".to_string(),
        created_at: 1_700_000_000_000,
    }
}

/// 未注册 → None；注册后可读回并投递；替换后新接收端生效、旧接收端不再收到。
#[tokio::test]
async fn sink_register_replace_and_absent() {
    let (engine, _storage, dir) = engine_with_db("proactive-sink-states").await;

    // 未注册：读回 None
    assert!(engine.proactive_sink().is_none(), "未注册应为 None");

    // 注册：读回句柄并投递，负载无损到达
    let first = Arc::new(RecordingSink::new());
    engine.set_proactive_sink(first.clone());
    let registered = engine.proactive_sink().expect("注册后应可读回");
    let message = sample_message();
    registered.deliver(&message).expect("投递应成功");
    assert_eq!(first.received(), vec![message.clone()], "负载应无损到达");

    // 替换：新接收端生效，旧接收端不再被调用
    let second = Arc::new(RecordingSink::new());
    engine.set_proactive_sink(second.clone());
    let replaced = engine.proactive_sink().expect("替换后应可读回");
    replaced.deliver(&message).expect("投递应成功");
    assert_eq!(second.received(), vec![message], "替换后新接收端应收到");
    assert_eq!(first.received().len(), 1, "旧接收端不应再被调用");

    let _ = std::fs::remove_dir_all(dir);
}
