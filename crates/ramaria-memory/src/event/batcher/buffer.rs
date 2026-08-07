//! rust/crates/ramaria-memory/src/event/batcher/buffer.rs - 碎片簇缓冲区（Pending Buffer）
//!
//! 设计特点:
//! - `PendingFragment`: 暂存不足 min_cluster_size 的孤立 L1 条目
//! - `PendingBuffer::add_fragment()`: 按关键词 Jaccard ≥ 0.4 将新 L1 归入同类碎片，跨批次自动积累
//! - `PendingBuffer::drain_promoted()`: 碎片达到 min_cluster_size 后自动提升为正式簇
//! - `PendingBuffer::collect_expired()`: 超过 max_age_days 未归并的碎片超时降级
//! - 关键词 Jaccard 去重合并：归入碎片时自动更新 cluster_keywords 并集
//! - 纯计算模块，零 I/O，零 async，可独立单元测试

use super::L1Item;
use ramaria_core::keyword::KeywordToken;

/// 一天的毫秒数（用于超时计算）。
const MS_PER_DAY: i64 = 86_400_000;

/// 碎片合并相似度阈值：新 L1 与已有碎片的关键词 Jaccard ≥ 此值时归入。
const FRAGMENT_MERGE_THRESHOLD: f64 = 0.4;

// =========================================================
// PendingFragment — 碎片簇条目
// =========================================================

/// 碎片簇缓冲区条目。
///
/// 职责:
/// - 暂存不足 min_cluster_size 的孤立 L1 条目。
/// - 同类主题（关键词 Jaccard ≥ 0.4）的碎片合并到一个 PendingFragment 中。
/// - 积累到 min_cluster_size 后由 `PendingBuffer::drain_promoted()` 提升为正式簇。
///
/// 字段约定:
/// - `l1_items`: 积累的 L1 条目（按添加时间正序）。
/// - `added_at`: 该碎片首次创建的 Unix 毫秒时间戳，用于超时计算。
/// - `cluster_keywords`: 碎片内所有 L1 关键词的去重并集，用于相似度匹配。
#[derive(Debug, Clone)]
pub struct PendingFragment {
    /// 积累的 L1 条目（按添加时间正序）
    pub l1_items: Vec<L1Item>,
    /// 首次添加时间（Unix 毫秒）
    pub added_at: i64,
    /// 碎片级别的去重关键词并集
    pub cluster_keywords: Vec<KeywordToken>,
}

impl PendingFragment {
    /// 创建新的碎片条目。
    ///
    /// 参数:
    /// - `item`: 要添加的第一个 L1 条目。
    /// - `now_ms`: 当前 Unix 毫秒时间戳。
    pub fn new(item: L1Item, now_ms: i64) -> Self {
        let keywords = item.keywords.clone();
        Self {
            l1_items: vec![item],
            added_at: now_ms,
            cluster_keywords: keywords,
        }
    }

    /// 向碎片中追加一个 L1 条目。
    ///
    /// 说明:
    /// - 将 item 追加到 `l1_items` 末尾。
    /// - 将 item 的关键词合并到 `cluster_keywords` 并集（去重）。
    pub fn merge(&mut self, item: L1Item) {
        // 合并关键词到并集
        for kw in &item.keywords {
            if !self.cluster_keywords.contains(kw) {
                self.cluster_keywords.push(kw.clone());
            }
        }
        self.l1_items.push(item);
    }

    /// 返回碎片内 L1 条目数量。
    pub fn len(&self) -> usize {
        self.l1_items.len()
    }

    /// 碎片是否为空（由构造保证非空，保留用于泛型一致性）。
    pub fn is_empty(&self) -> bool {
        self.l1_items.is_empty()
    }

    /// 碎片是否已超时。
    ///
    /// 参数:
    /// - `now_ms`: 当前 Unix 毫秒时间戳。
    /// - `max_age_days`: 最大存活天数。
    pub fn is_expired(&self, now_ms: i64, max_age_days: u32) -> bool {
        let max_age_ms = max_age_days as i64 * MS_PER_DAY;
        now_ms - self.added_at > max_age_ms
    }
}

// =========================================================
// PendingBuffer — 碎片簇缓冲区
// =========================================================

/// 碎片簇缓冲区。
///
/// 职责:
/// - 管理所有未达到最小簇大小的碎片条目。
/// - `add_fragment()`: 接收孤立 L1，按关键词 Jaccard ≥ 0.4 归入同类碎片。
/// - `drain_promoted()`: 达到 min_cluster_size 的碎片提升为正式簇。
/// - `collect_expired()`: 超时未归并的碎片降级处理。
///
/// 字段约定:
/// - `fragments`: 当前缓冲区中的所有碎片。
/// - `min_cluster_size`: 触发自动提升的最小 L1 条目数，默认 3。
/// - `max_age_days`: 碎片的最大存活天数，超时降级合并，默认 30。
///
/// 使用示例:
/// ```ignore
/// let mut buf = PendingBuffer::default();
/// buf.add_fragment(orphan_l1, now_ms);
/// let promoted = buf.drain_promoted(); // 达到 3 条的碎片
/// let expired = buf.collect_expired(now_ms); // 超过 30 天的碎片
/// ```
#[derive(Debug, Clone)]
pub struct PendingBuffer {
    /// 缓冲区中的碎片列表
    pub fragments: Vec<PendingFragment>,
    /// 触发自动提升的最小簇大小
    pub min_cluster_size: usize,
    /// 碎片最大存活天数（超时降级合并）
    pub max_age_days: u32,
}

impl PendingBuffer {
    /// 创建新的空缓冲区。
    ///
    /// 参数:
    /// - `min_cluster_size`: 触发提升的阈值，默认 3。
    /// - `max_age_days`: 碎片超时天数，默认 30。
    pub fn new(min_cluster_size: usize, max_age_days: u32) -> Self {
        Self {
            fragments: Vec::new(),
            min_cluster_size,
            max_age_days,
        }
    }

    /// 返回缓冲区中碎片数量。
    pub fn fragment_count(&self) -> usize {
        self.fragments.len()
    }

    /// 返回所有碎片中的 L1 条目总数。
    pub fn total_items(&self) -> usize {
        self.fragments.iter().map(|f| f.len()).sum()
    }

    /// 缓冲区是否为空。
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    // =========================================================
    // 核心操作
    // =========================================================

    /// 接收一个孤立 L1 条目。
    ///
    /// 算法:
    /// 1. 若缓冲区为空，直接创建新碎片。
    /// 2. 遍历所有已有碎片，计算新条目与各碎片的 `cluster_keywords` 的 Jaccard 相似度。
    /// 3. 若最大相似度 ≥ 0.4，将条目归入该碎片（调用 `PendingFragment::merge`）。
    /// 4. 否则创建新碎片。
    ///
    /// 参数:
    /// - `item`: 要添加的孤立 L1 条目。
    /// - `now_ms`: 当前 Unix 毫秒时间戳。
    ///
    /// 说明:
    /// - 本方法不自动提升；提升由 `drain_promoted()` 批量执行，
    ///   调用方在 `add_fragment` 后可立即调用 `drain_promoted()` 获取提升的碎片。
    pub fn add_fragment(&mut self, item: L1Item, now_ms: i64) {
        if self.fragments.is_empty() {
            self.fragments.push(PendingFragment::new(item, now_ms));
            return;
        }

        let mut best_sim: f64 = 0.0;
        let mut best_idx: Option<usize> = None;

        for (idx, fragment) in self.fragments.iter().enumerate() {
            let sim = jaccard_keyword_sets(&item.keywords, &fragment.cluster_keywords);
            if sim > best_sim {
                best_sim = sim;
                best_idx = Some(idx);
            }
        }

        if best_sim >= FRAGMENT_MERGE_THRESHOLD
            && let Some(idx) = best_idx
        {
            tracing::debug!(
                jaccard = best_sim,
                fragment_idx = idx,
                fragment_size = self.fragments[idx].len(),
                "孤立 L1 归入已有碎片"
            );
            self.fragments[idx].merge(item);
            return;
        }

        // 无匹配碎片，创建新碎片
        tracing::debug!(
            best_jaccard = best_sim,
            threshold = FRAGMENT_MERGE_THRESHOLD,
            "孤立 L1 创建新碎片"
        );
        self.fragments.push(PendingFragment::new(item, now_ms));
    }

    /// 批量添加孤立 L1 条目（来自 `absorb_orphans` 的 remaining_orphans）。
    ///
    /// 参数:
    /// - `items`: 孤立 L1 条目列表。
    /// - `now_ms`: 当前 Unix 毫秒时间戳。
    pub fn add_fragments(&mut self, items: Vec<L1Item>, now_ms: i64) {
        for item in items {
            self.add_fragment(item, now_ms);
        }
    }

    /// 排出所有已达到 min_cluster_size 的碎片，并将其 l1_items 提升为正式簇。
    ///
    /// 返回:
    /// - `Vec<Vec<L1Item>>`: 每个元素是一个已提升的簇（L1 条目列表）。
    ///   未达到阈值的碎片保留在缓冲区中。
    ///
    /// 说明:
    /// - 提升后的碎片从缓冲区移除。
    /// - 返回值中的每个 Vec 可直接用于构造 `TopicCluster`。
    pub fn drain_promoted(&mut self) -> Vec<Vec<L1Item>> {
        let mut promoted: Vec<Vec<L1Item>> = Vec::new();
        let mut remaining: Vec<PendingFragment> = Vec::new();

        for fragment in self.fragments.drain(..) {
            if fragment.len() >= self.min_cluster_size {
                tracing::debug!(
                    fragment_size = fragment.len(),
                    min_cluster_size = self.min_cluster_size,
                    keyword_count = fragment.cluster_keywords.len(),
                    "碎片达到阈值，自动提升"
                );
                promoted.push(fragment.l1_items);
            } else {
                remaining.push(fragment);
            }
        }

        self.fragments = remaining;
        promoted
    }

    /// 收集所有超时的碎片。
    ///
    /// 超时条件: `added_at + max_age_days × MS_PER_DAY < now_ms`
    ///
    /// 参数:
    /// - `now_ms`: 当前 Unix 毫秒时间戳。
    ///
    /// 返回:
    /// - `Vec<PendingFragment>`: 所有超时碎片（按创建时间正序）。
    ///   这些碎片应被合并为一条低置信度降级事件（由 EventExtractor 处理）。
    ///
    /// 说明:
    /// - 超时碎片从缓冲区移除。
    /// - 即使碎片内只有 1 条 L1，超时后也会被收集。
    /// - 调用方应将返回的多个碎片合并处理（见 M3 事件提取）。
    pub fn collect_expired(&mut self, now_ms: i64) -> Vec<PendingFragment> {
        let max_age_ms = self.max_age_days as i64 * MS_PER_DAY;
        let mut expired: Vec<PendingFragment> = Vec::new();
        let mut remaining: Vec<PendingFragment> = Vec::new();

        for fragment in self.fragments.drain(..) {
            if now_ms - fragment.added_at > max_age_ms {
                tracing::debug!(
                    fragment_size = fragment.len(),
                    age_days = (now_ms - fragment.added_at) / MS_PER_DAY,
                    max_age_days = self.max_age_days,
                    "碎片超时，降级合并"
                );
                expired.push(fragment);
            } else {
                remaining.push(fragment);
            }
        }

        // 按创建时间正序排列
        expired.sort_by_key(|f| f.added_at);
        self.fragments = remaining;
        expired
    }
}

impl Default for PendingBuffer {
    fn default() -> Self {
        Self::new(3, 30)
    }
}

// =========================================================
// 关键词 Jaccard 相似度（buffer 专用，与 mod.rs 中的逻辑一致）
// =========================================================

/// 计算两组 KeywordToken 的 Jaccard 相似度。
///
/// 公式: `J(A, B) = |A ∩ B| / |A ∪ B|`
///
/// 与 `mod.rs::jaccard_similarity` 逻辑相同，在此重复以避免跨模块循环依赖。
fn jaccard_keyword_sets(kw_a: &[KeywordToken], kw_b: &[KeywordToken]) -> f64 {
    if kw_a.is_empty() && kw_b.is_empty() {
        return 0.0;
    }

    // 计算交集
    let intersection = kw_a.iter().filter(|k| kw_b.contains(k)).count();

    // 计算并集（去重）
    let mut union_set: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for k in kw_a {
        union_set.insert(k.as_str());
    }
    for k in kw_b {
        union_set.insert(k.as_str());
    }
    let union_size = union_set.len();

    if union_size == 0 {
        return 0.0;
    }

    intersection as f64 / union_size as f64
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
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
    fn add_fragment_merges_to_best_match() {
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

    // ---- jaccard_keyword_sets ----

    #[test]
    fn jaccard_identical() {
        let a: Vec<KeywordToken> = vec![
            KeywordToken::new("工作").unwrap(),
            KeywordToken::new("压力").unwrap(),
        ];
        let b: Vec<KeywordToken> = vec![
            KeywordToken::new("工作").unwrap(),
            KeywordToken::new("压力").unwrap(),
        ];
        assert!((jaccard_keyword_sets(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint() {
        let a = vec![KeywordToken::new("工作").unwrap()];
        let b = vec![KeywordToken::new("休闲").unwrap()];
        assert!((jaccard_keyword_sets(&a, &b) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_empty() {
        let a: Vec<KeywordToken> = vec![];
        let b: Vec<KeywordToken> = vec![];
        assert!((jaccard_keyword_sets(&a, &b) - 0.0).abs() < f64::EPSILON);
    }
}
