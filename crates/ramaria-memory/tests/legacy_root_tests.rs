//! ramaria-memory 集成测试 —— 自仓库根 `tests/` 迁移的 legacy 测试
//!
//! 迁移自 `tests/integration_tests.rs`（原 Phase 8 跨 crate 集成测试，13 个
//! #[test]）。原文件位于仓库根 virtual workspace 下，cargo 永不编译（死测试）；
//! 迁移后由 `ramaria-memory` crate 编译执行（决策 4.B 已批准）。
//!
//! 迁移决策：原 13 个测试中 9 个因行为已被 src 单元测试覆盖等价（且其中 4 个
//! 引用已移除/变更的 API：`apply_token_budget` 旧签名、`calculate_decay`）而
//! 删除；以下 4 个测试在本 crate 中无等价覆盖，迁移保留：
//! - `retriever_lru_eviction`：LRU 驱逐（`set_lru_max_entries` 全 crate 无测试）
//! - `retriever_remove_l1`：`remove_l1` 全 crate 无测试
//! - `retriever_lru_unlimited`：`lru_max_entries == 0` 无限制模式无测试
//! - `fixture_data_completeness`：fixtures/ 数据完整性（消息数、摘要/关键词非空）
//!
//! fixture 数据位于 `tests/fixtures/`（conversations.json / memory_events.json），
//! 经 `fixtures` 模块加载（`include_str!` 相对当前文件路径自动成立）。

mod fixtures;

use fixtures::load_conversation_fixtures;

// =========================================================
// 第 1 节：Retriever LRU 驱逐集成测试
// =========================================================

/// 验证 Retriever 的 LRU 驱逐在超过容量上限时正确触发。
///
/// 场景:
/// 1. 设置低 LRU 上限（5 条）
/// 2. 添加 10 条文档（超过上限）
/// 3. 验证驱逐后总文档数 ≤ 上限
#[test]
fn retriever_lru_eviction() {
    use ramaria_memory::retriever::{L1DocView, Retriever};
    use uuid::Uuid;

    let mut retriever = Retriever::new();
    // 设置极低的 LRU 上限以便测试
    retriever.set_lru_max_entries(5);

    // 添加 10 条文档（created_at 递增）
    for i in 0..10u64 {
        let doc = L1DocView {
            id: Uuid::new_v4(),
            summary: format!("这是第 {} 条测试文档", i + 1),
            keywords: Some(format!("测试,文档{}", i)),
            persona_uid: Some("user-0001".to_string()),
            created_at: (i + 1) as i64 * 1000, // 模拟递增时间戳
            salience: 0.5,
        };
        retriever.index_l1(&doc);
    }

    let total = retriever.doc_count();
    assert!(total <= 5, "LRU 驱逐后文档数应 ≤ 5，实际: {}", total);
}

/// 验证 Retriever remove_l1 正确从 BM25 和 HashMap 中移除。
#[test]
fn retriever_remove_l1() {
    use ramaria_memory::retriever::{L1DocView, Retriever};
    use uuid::Uuid;

    let mut retriever = Retriever::new();
    let doc_id = Uuid::new_v4();

    let doc = L1DocView {
        id: doc_id,
        summary: "可删除的测试文档".to_string(),
        keywords: Some("删除,测试".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: 1000,
        salience: 0.5,
    };
    retriever.index_l1(&doc);
    assert_eq!(retriever.doc_count(), 1);

    // 移除
    retriever.remove_l1(&doc_id);
    assert_eq!(retriever.doc_count(), 0);
}

/// 验证 LRU 上限为 0 时不进行驱逐（无限制模式）。
#[test]
fn retriever_lru_unlimited() {
    use ramaria_memory::retriever::{L1DocView, Retriever};
    use uuid::Uuid;

    let mut retriever = Retriever::new();
    retriever.set_lru_max_entries(0); // 无限制

    // 添加 20 条文档（远超默认上限）
    for i in 0..20u64 {
        let doc = L1DocView {
            id: Uuid::new_v4(),
            summary: format!("文档 {}", i),
            keywords: None,
            persona_uid: None,
            created_at: i as i64,
            salience: 0.3,
        };
        retriever.index_l1(&doc);
    }

    assert_eq!(retriever.doc_count(), 20, "无限制模式下不应驱逐任何文档");
}

// =========================================================
// 第 2 节：Fixture 数据完整性验证
// =========================================================

/// 验证所有 fixture 对话中有明确的消息角色和内容。
#[test]
fn fixture_data_completeness() {
    let conv = load_conversation_fixtures();

    assert_eq!(conv.fixtures.len(), 7, "预期 7 组对话 fixture");

    for f in &conv.fixtures {
        assert!(!f.id.is_empty(), "fixture 应有 ID");
        assert!(f.messages.len() >= 6, "每组至少 6 条消息（3轮对话）");

        for msg in &f.messages {
            assert!(!msg.content.is_empty(), "消息内容不应为空");
        }

        // L1 预期值验证
        assert!(!f.expected_l1.summary.is_empty(), "L1 summary 不应为空");
        assert!(!f.expected_l1.keywords.is_empty(), "L1 keywords 不应为空");
        assert!(
            (-1.0..=1.0).contains(&f.expected_l1.valence),
            "valence 应在 [-1, 1]"
        );
        assert!(
            (0.0..=1.0).contains(&f.expected_l1.salience),
            "salience 应在 [0, 1]"
        );
    }
}
