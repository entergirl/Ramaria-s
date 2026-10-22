//! crates/ramaria-memory/src/event/batcher/buffer/tests.rs - 话题批次缓冲单元测试
//!
//! 设计特点:
//! - 位于 event::batcher::buffer 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由父文件以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::keyword::KeywordToken;
use uuid::Uuid;

/// 辅助函数：创建带关键词的 L1Item
fn make_l1(keywords: Vec<&str>, created_at: i64) -> L1Item {
    L1Item {
        id: Uuid::new_v4(),
        summary: format!("s_{}", keywords.join("_")),
        keywords: keywords.into_iter().filter_map(KeywordToken::new).collect(),
        evidence_notes: vec![],
        embedding: None,
        salience: 0.5,
        created_at,
        occurred_at: created_at,
    }
}

/// 辅助函数：创建带 embedding 的 L1Item（保留供 PendingBuffer 语义匹配扩展使用）
#[allow(dead_code)]
fn make_l1_with_emb(keywords: Vec<&str>, embedding: Vec<f32>, created_at: i64) -> L1Item {
    L1Item {
        id: Uuid::new_v4(),
        summary: format!("s_{}", keywords.join("_")),
        keywords: keywords.into_iter().filter_map(KeywordToken::new).collect(),
        evidence_notes: vec![],
        embedding: Some(embedding),
        salience: 0.5,
        created_at,
        occurred_at: created_at,
    }
}

// ---- PendingFragment ----

#[test]
fn pending_fragment_new() {
    let item = make_l1(vec!["工作", "压力"], 1000);
    let frag = PendingFragment::new(item, 1_700_000_000_000);
    assert_eq!(frag.len(), 1);
    assert!(!frag.is_empty());
    assert_eq!(frag.cluster_keywords.len(), 2);
    assert_eq!(frag.added_at, 1_700_000_000_000);
}

#[test]
fn pending_fragment_merge_adds_item_and_keywords() {
    let item1 = make_l1(vec!["工作", "压力"], 1000);
    let mut frag = PendingFragment::new(item1, 1_000_000);

    let item2 = make_l1(vec!["工作", "倦怠"], 2000);
    frag.merge(item2);

    assert_eq!(frag.len(), 2);
    // 关键词并集：工作、压力、倦怠 → 3 个
    assert_eq!(frag.cluster_keywords.len(), 3);
}

#[test]
fn pending_fragment_merge_dedup_keywords() {
    let item1 = make_l1(vec!["工作", "压力"], 1000);
    let mut frag = PendingFragment::new(item1, 1_000_000);

    // 相同关键词应不重复添加
    let item2 = make_l1(vec!["工作", "压力"], 2000);
    frag.merge(item2);

    assert_eq!(frag.len(), 2);
    assert_eq!(frag.cluster_keywords.len(), 2);
}

#[test]
fn pending_fragment_is_expired() {
    let item = make_l1(vec!["工作"], 1000);
    let frag = PendingFragment::new(item, 1_000_000);
    // 30 天后：1_000_000 + 30*86_400_000 = 2_593_000_000
    // 现在 3_000_000_000 > 2_593_000_000 → 超时
    assert!(frag.is_expired(3_000_000_000, 30));
    // 现在 2_000_000_000 < 2_593_000_000 → 未超时
    assert!(!frag.is_expired(2_000_000_000, 30));
}

// ---- PendingBuffer::new / default ----

#[test]
fn buffer_new_is_empty() {
    let buf = PendingBuffer::new(3, 30);
    assert!(buf.is_empty());
    assert_eq!(buf.fragment_count(), 0);
    assert_eq!(buf.total_items(), 0);
}

#[test]
fn buffer_default_values() {
    let buf = PendingBuffer::default();
    assert_eq!(buf.min_cluster_size, 3);
    assert_eq!(buf.max_age_days, 30);
}

// ---- add_fragment ----

#[test]
fn add_first_fragment_creates_new() {
    let mut buf = PendingBuffer::new(3, 30);
    let item = make_l1(vec!["工作"], 1000);
    buf.add_fragment(item, 1_000_000);

    assert_eq!(buf.fragment_count(), 1);
    assert_eq!(buf.total_items(), 1);
}

#[test]
fn add_similar_fragment_merges() {
    let mut buf = PendingBuffer::new(3, 30);

    // 先添加一个碎片
    buf.add_fragment(make_l1(vec!["工作", "压力", "加班"], 1000), 1_000_000);
    assert_eq!(buf.fragment_count(), 1);

    // 添加关键词高度重叠的第二个 L1（Jaccard = 2/4 = 0.5 ≥ 0.4）
    buf.add_fragment(make_l1(vec!["工作", "压力", "倦怠"], 2000), 1_000_000);
    assert_eq!(buf.fragment_count(), 1, "应合并到已有碎片");
    assert_eq!(buf.total_items(), 2);
}

#[test]
fn add_dissimilar_fragment_creates_new() {
    let mut buf = PendingBuffer::new(3, 30);

    buf.add_fragment(make_l1(vec!["工作", "压力"], 1000), 1_000_000);
    // 关键词无交集 → 创建新碎片
    buf.add_fragment(make_l1(vec!["休闲", "旅游"], 2000), 1_000_000);

    assert_eq!(buf.fragment_count(), 2);
    assert_eq!(buf.total_items(), 2);
}

#[test]
fn add_fragment_below_merge_threshold_creates_new() {
    let mut buf = PendingBuffer::new(3, 30);

    // 碎片 A: 工作相关
    buf.add_fragment(make_l1(vec!["工作", "加班"], 1000), 1_000_000);
    // 碎片 B: 休闲相关
    buf.add_fragment(make_l1(vec!["休闲", "旅游"], 2000), 1_000_000);

    assert_eq!(buf.fragment_count(), 2);

    // 新条目与碎片 A 的关键词重叠（"工作"），与碎片 B 无交集
    // A: Jaccard = 1/3 ≈ 0.33（< 0.4，不合并！）
    // → 创建新碎片
    buf.add_fragment(make_l1(vec!["工作", "压力"], 3000), 1_000_000);
    // 但 Jaccard 为 1/3 ≈ 0.33 < 0.4，所以创建新碎片
    assert_eq!(buf.fragment_count(), 3);
}

// ---- drain_promoted ----

#[test]
fn drain_promoted_empty_buffer() {
    let mut buf = PendingBuffer::new(3, 30);
    let promoted = buf.drain_promoted();
    assert!(promoted.is_empty());
}

#[test]
fn drain_promoted_below_threshold_stays() {
    let mut buf = PendingBuffer::new(3, 30);

    buf.add_fragment(make_l1(vec!["工作"], 1000), 1_000_000);
    buf.add_fragment(make_l1(vec!["工作", "压力"], 2000), 1_000_000);
    // 2 条 < 3，不应提升
    let promoted = buf.drain_promoted();
    assert!(promoted.is_empty(), "2 条不应达到提升阈值");
    assert_eq!(buf.fragment_count(), 1);
    assert_eq!(buf.total_items(), 2);
}

#[test]
fn drain_promoted_reaches_threshold() {
    let mut buf = PendingBuffer::new(3, 30);

    // 连续添加 3 条相似 L1
    buf.add_fragment(make_l1(vec!["工作", "加班"], 1000), 1_000_000);
    buf.add_fragment(make_l1(vec!["工作", "加班", "报告"], 2000), 1_000_000);
    buf.add_fragment(make_l1(vec!["工作", "加班", "会议"], 3000), 1_000_000);

    assert_eq!(buf.total_items(), 3);

    let promoted = buf.drain_promoted();
    assert_eq!(promoted.len(), 1, "应提升 1 个簇");
    assert_eq!(promoted[0].len(), 3, "提升的簇应含 3 条 L1");
    assert!(buf.is_empty(), "提升后缓冲区应为空");
}

#[test]
fn drain_promoted_partial_promotion() {
    let mut buf = PendingBuffer::new(3, 30);

    // 碎片 A: 工作相关，3 条 → 应提升
    buf.add_fragment(make_l1(vec!["工作", "加班", "压力"], 1000), 1_000_000);
    buf.add_fragment(
        make_l1(vec!["工作", "加班", "报告", "压力"], 2000),
        1_000_000,
    );
    buf.add_fragment(
        make_l1(vec!["工作", "加班", "会议", "压力"], 3000),
        1_000_000,
    );

    // 碎片 B: 休闲相关，仅 2 条 → 不提升
    buf.add_fragment(make_l1(vec!["休闲", "旅游", "娱乐"], 4000), 1_000_000);
    buf.add_fragment(make_l1(vec!["休闲", "摄影", "娱乐"], 5000), 1_000_000);

    let promoted = buf.drain_promoted();
    assert_eq!(promoted.len(), 1, "仅碎片 A 应提升");
    assert_eq!(promoted[0].len(), 3);
    assert_eq!(buf.fragment_count(), 1, "碎片 B 应留在缓冲区");
    assert_eq!(buf.total_items(), 2);
}

// ---- collect_expired ----

#[test]
fn collect_expired_empty_buffer() {
    let mut buf = PendingBuffer::new(3, 30);
    let expired = buf.collect_expired(1_000_000_000);
    assert!(expired.is_empty());
}

#[test]
fn collect_expired_no_expired() {
    let mut buf = PendingBuffer::new(3, 30);
    let now = 1_000_000_000;
    buf.add_fragment(make_l1(vec!["工作"], 1000), now);

    // 刚添加 → 立即检查 → 不应超时
    let expired = buf.collect_expired(now);
    assert!(expired.is_empty());
    assert_eq!(buf.fragment_count(), 1);
}

#[test]
fn collect_expired_past_max_age() {
    let mut buf = PendingBuffer::new(3, 30);
    let created_at = 1_000_000_000;
    // 31 天后
    let now = created_at + 31 * MS_PER_DAY;

    buf.add_fragment(make_l1(vec!["工作"], 1000), created_at);

    let expired = buf.collect_expired(now);
    assert_eq!(expired.len(), 1, "应收集 1 个超时碎片");
    assert!(buf.is_empty(), "超时碎片应被移除");
}

#[test]
fn collect_expired_mixed() {
    let mut buf = PendingBuffer::new(3, 30);
    let now = 5_000_000_000;

    // 碎片 A: 刚添加，未超时
    buf.add_fragment(make_l1(vec!["工作"], 1000), now - MS_PER_DAY);
    // 碎片 B: 31 天前，已超时
    buf.add_fragment(make_l1(vec!["休闲"], 2000), now - 31 * MS_PER_DAY);

    let expired = buf.collect_expired(now);
    assert_eq!(expired.len(), 1, "仅碎片 B 应超时");
    assert_eq!(buf.fragment_count(), 1, "碎片 A 应保留");
}

// ---- 跨批次积累（add_fragment + drain_promoted 协同） ----

#[test]
fn cross_batch_accumulation() {
    let mut buf = PendingBuffer::new(3, 30);

    // 批次 1：2 条共享 "工作"+"压力"+"加班"（Jaccard ≥ 0.5 确保归入同一碎片）
    buf.add_fragment(make_l1(vec!["工作", "加班", "压力"], 1000), 1_000_000);
    buf.add_fragment(make_l1(vec!["工作", "报告", "压力"], 2000), 1_000_000);

    // 排出 → 2 条不足阈值，无提升
    let promoted1 = buf.drain_promoted();
    assert!(promoted1.is_empty());
    assert_eq!(buf.total_items(), 2);

    // 批次 2：再添加 1 条同类 → 总共 3 条，应提升
    buf.add_fragment(make_l1(vec!["工作", "会议", "压力"], 3000), 1_000_000);

    let promoted2 = buf.drain_promoted();
    assert_eq!(promoted2.len(), 1, "跨批次积累后应提升");
    assert_eq!(promoted2[0].len(), 3);
    assert!(buf.is_empty());
}
