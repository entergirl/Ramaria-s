//! crates/ramaria-memory/src/event/batcher/cluster.rs - TopicCluster 主题簇
//!
//! 设计特点:
//! - 封装一组语义相近的 L1 摘要，作为 L1→L2 事件提取的批次输入
//! - 携带簇级别聚合统计（平均显著性、时间跨度、代表性关键词）
//! - 构造时自动按 `created_at` 正序排列并去重关键词
//! - 纯数据结构，不依赖 LLM 或数据库

use ramaria_core::keyword::KeywordToken;

use super::item::L1Item;

// =========================================================
// TopicCluster — 主题簇
// =========================================================

/// 由 TopicBatcher 产出的主题簇。
///
/// 职责:
/// - 封装一组语义相近的 L1 摘要，作为后续事件提取（L1→L2）的批次输入。
/// - 携带簇级别的聚合统计信息（平均显著性、时间跨度、代表性关键词）。
///
/// 字段约定:
/// - `l1_items`: 按 `created_at` 时间正序排列。
/// - `cluster_keywords`: 簇内所有 L1 关键词的去重并集（保留高频词顺序）。
/// - `avg_salience`: 所有 L1 salience 的算术均值。
/// - `time_span`: (最早时间戳, 最晚时间戳)，均为 Unix 毫秒。
#[derive(Debug, Clone)]
pub struct TopicCluster {
    /// 簇内的 L1 条目（按 created_at 正序）
    pub l1_items: Vec<L1Item>,
    /// 簇级别的去重关键词集合
    pub cluster_keywords: Vec<KeywordToken>,
    /// 平均显著性
    pub avg_salience: f64,
    /// 时间跨度 (earliest_ms, latest_ms)
    pub time_span: (i64, i64),
}

impl TopicCluster {
    /// 从一组 L1Item 构造 TopicCluster。
    ///
    /// 说明:
    /// - 自动计算 `cluster_keywords`（去重并集）、`avg_salience`、`time_span`。
    /// - 调用方应保证 `l1_items` 非空。
    pub fn new(mut l1_items: Vec<L1Item>) -> Self {
        // 按时间正序排列
        l1_items.sort_by_key(|item| item.created_at);

        // 收集簇内所有关键词（去重）
        let mut kw_set: std::collections::BTreeMap<String, KeywordToken> =
            std::collections::BTreeMap::new();
        for item in &l1_items {
            for kw in &item.keywords {
                kw_set
                    .entry(kw.as_str().to_string())
                    .or_insert_with(|| kw.clone());
            }
        }
        let cluster_keywords: Vec<KeywordToken> = kw_set.into_values().collect();

        // 平均显著性
        let avg_salience = if l1_items.is_empty() {
            0.0
        } else {
            let sum: f64 = l1_items.iter().map(|i| i.salience).sum();
            sum / l1_items.len() as f64
        };

        // 时间跨度
        let time_span = if l1_items.is_empty() {
            (0, 0)
        } else {
            let earliest = l1_items.first().map(|i| i.created_at).unwrap_or(0);
            let latest = l1_items.last().map(|i| i.created_at).unwrap_or(0);
            (earliest, latest)
        };

        Self {
            l1_items,
            cluster_keywords,
            avg_salience,
            time_span,
        }
    }

    /// 返回簇内 L1 条目数量。
    pub fn len(&self) -> usize {
        self.l1_items.len()
    }

    /// 簇是否为空。
    pub fn is_empty(&self) -> bool {
        self.l1_items.is_empty()
    }
}
