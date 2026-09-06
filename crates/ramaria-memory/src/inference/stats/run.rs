//! crates/ramaria-memory/src/inference/stats/run.rs - Ramaria Phase A 统计主编排模块
//!
//! 设计特点:
//! - run_phase_a_stats: 执行完整 Phase A 管线（A1 三轨 → 派生增强 → A3 聚合 → A6 跨分类 → A7 代表事件 → E 动机）。
//! - use_calibrated_weights=true 走三轨准入 + 校准权重链；false 回退旧行为（硬截断 + 简单权重）。
//! - 本函数为纯数值收口，不执行 I/O，日志由调用方记录；调用方负责从存储读取事件列表。
//! - normalize_group_weights 原为 stats.rs 内私有、仅 run_phase_a_stats 使用，故随主编排迁至本模块。
//!
//! 可见性说明:
//! - normalize_group_weights 为 pub(super): 仅供 stats 模块根级测试套件（tests.rs）直接调用，非外部 API。
//!
//! 安全约束:
//! - 纯数值计算，零 I/O；不记录任何事件原文或隐私数据。

use super::admission::{classify_events, prefilter_events};
use super::category::{compute_category_stats, compute_motive_stats, group_by_category};
use super::config::{
    CategoryStats, CrossCategoryMetrics, EventEnrichment, StatsConfig, StatsSummary,
};
use super::cross::compute_cross_category_metrics;
use super::representative::select_representative_events;
use ramaria_core::types::MemoryEvent;

/// 归一化所有分类的组权重。
///
/// 说明:
/// - 将各分类的 group_weight 除以总和，使所有分类权重之和为 1。
/// - 若仅有 1 个分类，其权重完全保留。
///
/// 参数:
/// - `categories`: 可变引用的分类统计列表。
pub(super) fn normalize_group_weights(categories: &mut [CategoryStats]) {
    let total_weight: f64 = categories.iter().map(|c| c.group_weight).sum();
    if total_weight > 0.0 {
        for cat in categories.iter_mut() {
            cat.group_weight /= total_weight;
        }
    }
    // 按 group_weight 降序排列
    categories.sort_by(|a, b| {
        b.group_weight
            .partial_cmp(&a.group_weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}
// =========================================================
// 主编排函数
// =========================================================

/// 执行完整的 Phase A 统计管线。
///
/// 管线步骤:
/// 1. A1: 三轨分类（confirmed/tentative/discarded）
/// 2. 自动派生增强数据（主题复现、情绪强度、提及频率）
/// 3. A3: 按 keywords 主分类分组，计算校准加权统计量
/// 4. A6: 计算跨分类高阶指标
/// 5. A7: 选取代表性事件
///
/// 参数:
/// - `events`: 完整的 L2 事件列表（从 StorageBackend 读取）。
/// - `config`: 统计配置。
///
/// 返回:
/// - `StatsSummary`: 包含三轨分布、分类统计、跨分类指标和代表性事件的完整摘要。
///
/// 说明:
/// - 当 `config.use_calibrated_weights = true`（默认）时，使用三轨准入 + 校准权重链。
/// - 当 `config.use_calibrated_weights = false` 时，回退到旧行为（硬截断 + 简单权重）。
/// - 若所有事件都被 discarded，返回空的 StatsSummary。
/// - 日志在调用方记录，本函数不执行 I/O。
pub fn run_phase_a_stats(events: &[MemoryEvent], config: &StatsConfig) -> StatsSummary {
    let total_events_in = events.len();

    if events.is_empty() {
        return StatsSummary {
            total_events_in: 0,
            total_events_filtered: 0,
            confirmed_count: 0,
            tentative_count: 0,
            discarded_count: 0,
            category_count: 0,
            categories: Vec::new(),
            cross_category: CrossCategoryMetrics {
                emotional_stability: 0.0,
                narrative_consistency: 1.0,
                attitude_contradiction_count: 0,
                share_skewness: 0.0,
                share_kurtosis: 0.0,
            },
            representative_events: Vec::new(),
            motive_stats: Vec::new(),
        };
    }

    if config.use_calibrated_weights {
        // ---- 三轨准入 + 校准权重链 ----

        // A1: 三轨分类
        let classified = classify_events(events);
        let active = classified.active_events();

        if active.is_empty() {
            return StatsSummary {
                total_events_in,
                total_events_filtered: 0,
                confirmed_count: classified.confirmed.len(),
                tentative_count: classified.tentative.len(),
                discarded_count: classified.discarded_count,
                category_count: 0,
                categories: Vec::new(),
                cross_category: CrossCategoryMetrics {
                    emotional_stability: 0.0,
                    narrative_consistency: 1.0,
                    attitude_contradiction_count: 0,
                    share_skewness: 0.0,
                    share_kurtosis: 0.0,
                },
                representative_events: Vec::new(),
                motive_stats: Vec::new(),
            };
        }

        // 派生增强数据
        let enrichments = EventEnrichment::derive_batch(&active);

        // A3: 按分类聚合（使用校准权重）
        let grouped = group_by_category(&active);
        let mut categories: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| {
                // 为每个分类的事件构建对应的增强数据子集
                let cat_enrichments: Vec<EventEnrichment> = evts
                    .iter()
                    .map(|e| {
                        // 在 active 中找到此事件的索引来获取对应的增强数据
                        let idx = active.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                        enrichments.get(idx).cloned().unwrap_or_default()
                    })
                    .collect();
                compute_category_stats(
                    cat,
                    evts,
                    Some(&cat_enrichments),
                    &config.calibrated_weight_config,
                )
            })
            .collect();
        normalize_group_weights(&mut categories);
        let category_count = categories.len();

        // A6: 跨分类指标（使用校准权重）
        let cross_category = compute_cross_category_metrics(
            &active,
            &categories,
            Some(&enrichments),
            &config.calibrated_weight_config,
        );

        // A7: 代表性事件
        let representative_events = select_representative_events(&active, &categories, config);

        // E: 动机维度二次分组统计
        let motive_stats =
            compute_motive_stats(&active, &enrichments, &config.calibrated_weight_config);

        StatsSummary {
            total_events_in,
            total_events_filtered: active.len(),
            confirmed_count: classified.confirmed.len(),
            tentative_count: classified.tentative.len(),
            discarded_count: classified.discarded_count,
            category_count,
            categories,
            cross_category,
            representative_events,
            motive_stats,
        }
    } else {
        // ---- 兼容路径: 硬截断 + 简单权重 ----

        let (filtered, _excluded) = prefilter_events(events, config);
        let total_events_filtered = filtered.len();

        if filtered.is_empty() {
            return StatsSummary {
                total_events_in,
                total_events_filtered: 0,
                confirmed_count: 0,
                tentative_count: 0,
                discarded_count: total_events_in,
                category_count: 0,
                categories: Vec::new(),
                cross_category: CrossCategoryMetrics {
                    emotional_stability: 0.0,
                    narrative_consistency: 1.0,
                    attitude_contradiction_count: 0,
                    share_skewness: 0.0,
                    share_kurtosis: 0.0,
                },
                representative_events: Vec::new(),
                motive_stats: Vec::new(),
            };
        }

        let grouped = group_by_category(&filtered);
        let mut categories: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| {
                compute_category_stats(cat, evts, None, &config.calibrated_weight_config)
            })
            .collect();
        normalize_group_weights(&mut categories);
        let category_count = categories.len();

        let cross_category = compute_cross_category_metrics(
            &filtered,
            &categories,
            None,
            &config.calibrated_weight_config,
        );

        let representative_events = select_representative_events(&filtered, &categories, config);

        StatsSummary {
            total_events_in,
            total_events_filtered,
            confirmed_count: total_events_filtered,
            tentative_count: 0,
            discarded_count: total_events_in - total_events_filtered,
            category_count,
            categories,
            cross_category,
            representative_events,
            motive_stats: Vec::new(),
        }
    }
}
