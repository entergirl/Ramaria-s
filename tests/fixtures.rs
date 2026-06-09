//! rust/tests/fixtures/mod.rs - 测试 fixture 加载器
//!
//! 设计特点:
//! - 提供类型安全的 fixture 加载函数
//! - 包含预期值的结构体定义
//! - 纯数据模块，零 I/O 依赖

use serde::Deserialize;

// =========================================================
// 对话 Fixture 结构
// =========================================================

/// 单条对话消息。
#[derive(Debug, Deserialize, Clone)]
pub struct FixtureMessage {
    pub role: String,
    pub content: String,
}

/// L1 摘要预期值。
#[derive(Debug, Deserialize, Clone)]
pub struct ExpectedL1 {
    pub summary: String,
    pub keywords: String,
    pub time_period: String,
    pub atmosphere: String,
    pub valence: f64,
    pub salience: f64,
}

/// 单组对话 fixture。
#[derive(Debug, Deserialize, Clone)]
pub struct ConversationFixture {
    pub id: String,
    pub scenario: String,
    pub persona_uid: String,
    pub messages: Vec<FixtureMessage>,
    pub expected_l1: ExpectedL1,
}

/// 对话 fixtures 容器。
#[derive(Debug, Deserialize)]
pub struct ConversationFixtures {
    pub description: String,
    pub fixtures: Vec<ConversationFixture>,
}

// =========================================================
// 记忆事件 Fixture 结构
// =========================================================

/// 单条预计算记忆事件。
#[derive(Debug, Deserialize, Clone)]
pub struct MemoryEventFixture {
    pub id: i64,
    pub persona_uid: String,
    pub title: String,
    pub summary: String,
    pub keywords: String,
    pub participants: Vec<String>,
    pub confidence: f64,
    pub salience: f64,
    pub valence: f64,
    pub presentation: String,
    pub share: f64,
    pub attitude: String,
    pub paraphrase: String,
    pub category: String,
    pub l1_sources: Vec<String>,
}

/// 记忆事件 fixtures 容器。
#[derive(Debug, Deserialize)]
pub struct MemoryEventFixtures {
    pub description: String,
    pub events: Vec<MemoryEventFixture>,
}

// =========================================================
// 加载函数
// =========================================================

/// 加载对话 fixtures。
///
/// 返回:
/// - 解析后的 ConversationFixtures。
pub fn load_conversation_fixtures() -> ConversationFixtures {
    let json_str = include_str!("fixtures/conversations.json");
    serde_json::from_str(json_str).expect("对话 fixtures JSON 解析失败")
}

/// 加载记忆事件 fixtures。
///
/// 返回:
/// - 解析后的 MemoryEventFixtures。
pub fn load_memory_event_fixtures() -> MemoryEventFixtures {
    let json_str = include_str!("fixtures/memory_events.json");
    serde_json::from_str(json_str).expect("记忆事件 fixtures JSON 解析失败")
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_conversation_fixtures() {
        let fixtures = load_conversation_fixtures();
        assert!(!fixtures.fixtures.is_empty(), "对话 fixtures 不应为空");
        assert_eq!(fixtures.fixtures.len(), 7);

        // 验证第一条 fixture
        let f = &fixtures.fixtures[0];
        assert_eq!(f.id, "conv-001");
        assert!(f.messages.len() >= 6);
        assert!(!f.expected_l1.summary.is_empty());
    }

    #[test]
    fn test_load_memory_event_fixtures() {
        let fixtures = load_memory_event_fixtures();
        assert!(!fixtures.events.is_empty(), "记忆事件 fixtures 不应为空");
        assert_eq!(fixtures.events.len(), 10);

        // 验证第一条事件
        let ev = &fixtures.events[0];
        assert_eq!(ev.id, 1);
        assert_eq!(ev.persona_uid, "user-0001");
        assert!(!ev.title.is_empty());
    }

    #[test]
    fn test_all_conversations_have_valid_roles() {
        let fixtures = load_conversation_fixtures();
        for conv in &fixtures.fixtures {
            for msg in &conv.messages {
                assert!(
                    msg.role == "user" || msg.role == "assistant",
                    "无效的消息角色: {}",
                    msg.role
                );
                assert!(!msg.content.is_empty(), "消息内容为空");
            }
        }
    }

    #[test]
    fn test_all_expected_l1_have_valid_valence() {
        let fixtures = load_conversation_fixtures();
        for conv in &fixtures.fixtures {
            let v = conv.expected_l1.valence;
            assert!(
                v == -1.0 || v == -0.5 || v == 0.0 || v == 0.5 || v == 1.0,
                "无效的 valence: {} (fixture: {})",
                v,
                conv.id
            );
        }
    }

    #[test]
    fn test_all_expected_l1_have_valid_salience() {
        let fixtures = load_conversation_fixtures();
        for conv in &fixtures.fixtures {
            let s = conv.expected_l1.salience;
            assert!(
                s == 0.0 || s == 0.25 || s == 0.5 || s == 0.75 || s == 1.0,
                "无效的 salience: {} (fixture: {})",
                s,
                conv.id
            );
        }
    }

    #[test]
    fn test_all_events_have_valid_confidence() {
        let fixtures = load_memory_event_fixtures();
        for ev in &fixtures.events {
            assert!(
                (0.0..=1.0).contains(&ev.confidence),
                "无效的 confidence: {} (event: {})",
                ev.confidence,
                ev.id
            );
        }
    }

    #[test]
    fn test_all_events_have_valid_share() {
        let fixtures = load_memory_event_fixtures();
        for ev in &fixtures.events {
            assert!(
                (0.0..=1.0).contains(&ev.share),
                "无效的 share: {} (event: {})",
                ev.share,
                ev.id
            );
        }
    }

    #[test]
    fn test_conversations_have_expected_persona() {
        let fixtures = load_conversation_fixtures();
        for conv in &fixtures.fixtures {
            assert_eq!(
                conv.persona_uid, "user-0001",
                "所有对话 fixture 当前仅用于 user-0001"
            );
        }
    }
}
