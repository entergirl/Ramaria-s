//! ramaria-memory 测试 fixture 加载器
//!
//! 迁移自仓库根 `tests/fixtures.rs`（决策 4.B：死测试复活）。
//! 原文件在仓库根 virtual workspace 下 cargo 永不编译，迁移后由
//! `ramaria-memory` crate 的集成测试编译执行。
//!
//! 设计特点:
//! - 提供类型安全的 fixture 加载函数
//! - 包含预期值的结构体定义
//! - 纯数据模块，零 I/O 依赖
//!
//! 数据文件与本模块同目录（conversations.json），
//! `include_str!` 相对当前文件解析，路径自动成立。

// 各测试二进制独立编译本模块，各目标使用子集不同 → 允许未使用项
#![allow(dead_code)]

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
// 加载函数
// =========================================================

/// 加载对话 fixtures。
///
/// 返回:
/// - 解析后的 ConversationFixtures。
pub fn load_conversation_fixtures() -> ConversationFixtures {
    let json_str = include_str!("conversations.json");
    serde_json::from_str(json_str).expect("对话 fixtures JSON 解析失败")
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
