//! crates/ramaria-memory/src/inference/stats/representative.rs - Ramaria A7 代表性事件选取模块
//!
//! 设计特点:
//! - 每分类取 salience 最高的 max_representative_events 条作为 RepresentativeEvent。
//! - 保留原始 attitude 文本（非 paraphrase），供 LLM 推断阶段看到具体语境。
//! - 输出按分类 group_weight 降序、分类内按 salience 降序。
//! - 只保留对性格推断有信息价值的字段，不泄露内部 ID。
//!
//! 安全约束:
//! - 纯数值计算，零 I/O；不记录任何事件原文或隐私数据。

use super::category::group_by_category;
use super::config::{CategoryStats, RepresentativeEvent, StatsConfig};
use ramaria_core::types::MemoryEvent;

// =========================================================
// A7: 代表性事件选取
// =========================================================

/// 选取每分类的代表性事件（A7）。
///
/// 策略:
/// - 每分类取 salience 最高的 `max_representative_events` 条。
/// - 保留原始 attitude 文本而非 paraphrase——LLM 推断阶段需要看到具体语境。
/// - 按分类的 group_weight 降序输出，每分类内按 salience 降序。
///
/// 参数:
/// - `events`: 预过滤后的事件列表（需与 categories 对应）。
/// - `categories`: 所有分类的统计摘要（用于确定分类顺序）。
/// - `config`: 统计配置。
///
/// 返回:
/// - 代表性事件列表，按分类权重降序、分类内按 salience 降序。
pub fn select_representative_events(
    events: &[MemoryEvent],
    categories: &[CategoryStats],
    config: &StatsConfig,
) -> Vec<RepresentativeEvent> {
    // 按分类分组原始事件
    let grouped = group_by_category(events);
    let category_order: std::collections::HashMap<&str, usize> = categories
        .iter()
        .enumerate()
        .map(|(i, c)| (c.category.as_str(), i))
        .collect();

    let mut results = Vec::new();

    for (category, mut cat_events) in grouped {
        // 按 salience 降序排列
        cat_events.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let take_n = config.max_representative_events.min(cat_events.len());
        for event in cat_events.iter().take(take_n) {
            results.push(RepresentativeEvent {
                title: event.title.clone(),
                summary: event.summary.clone(),
                attitude: event.attitude.clone(),
                valence: event.valence,
                salience: event.salience,
                category: category.clone(),
            });
        }
    }

    // 按分类权重排序（使用 category_order）
    results.sort_by(|a, b| {
        let order_a = category_order
            .get(a.category.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        let order_b = category_order
            .get(b.category.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        match order_a.cmp(&order_b) {
            std::cmp::Ordering::Equal => {
                // 同分类内按 salience 降序
                b.salience
                    .partial_cmp(&a.salience)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
            other => other,
        }
    });

    results
}
