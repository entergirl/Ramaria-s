//! crates/ramaria-memory/src/event/batcher/buffer.rs - 碎片簇缓冲区（Pending Buffer）
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
/// ```
/// use ramaria_memory::event::batcher::buffer::PendingBuffer;
/// use ramaria_memory::event::batcher::L1Item;
/// use ramaria_core::keyword::KeywordToken;
/// use uuid::Uuid;
///
/// let mut buf = PendingBuffer::default();
/// let orphan = L1Item {
///     id: Uuid::new_v4(),
///     summary: "孤立 L1 摘要".into(),
///     keywords: vec![KeywordToken::new("工作").unwrap()],
///     evidence_notes: vec![],
///     embedding: None,
///     salience: 0.5,
///     created_at: 1_000,
///     occurred_at: 1_000,
/// };
/// buf.add_fragment(orphan, 1_000_000);
/// let promoted = buf.drain_promoted(); // 未达 3 条，不会提升
/// let expired = buf.collect_expired(1_000_000); // 未超时，无过期
/// assert!(promoted.is_empty() && expired.is_empty());
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

        if let Some(idx) = best_idx.filter(|_| best_sim >= FRAGMENT_MERGE_THRESHOLD) {
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
// 关键词 Jaccard 相似度（buffer 专用薄包装，收敛到 similarity 模块）
// =========================================================

/// 计算两组 KeywordToken 的 Jaccard 相似度。
///
/// 公式: `J(A, B) = |A ∩ B| / |A ∪ B|`
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`，本函数为薄包装，
///   与 `mod.rs::jaccard_similarity` 共用同一实现（原"为避免跨模块循环依赖"
///   而逐行重复的实现体已消除）。
fn jaccard_keyword_sets(kw_a: &[KeywordToken], kw_b: &[KeywordToken]) -> f64 {
    crate::similarity::jaccard_similarity(kw_a.iter(), kw_b.iter())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests;
