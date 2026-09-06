//! crates/ramaria-memory/src/inference/stats/category.rs - Ramaria 按分类/动机聚合统计模块
//!
//! 设计特点:
//! - A3 按领域分类聚合: 按 keywords 主分类分组，产出 CategoryStats。
//! - E 动机维度二次分组: 在主分类之下按 motives 标签做二级加权聚合，产出 MotiveStats。
//! - compute_category_stats: 单分类全部加权统计量（效价/分享意愿/presentation 分布/组权重）。
//! - 复用 weighted 模块的加权均值/方差/占比原语；compute_motive_stats 复用 compute_category_stats。
//!
//! 安全约束:
//! - 纯数值计算，零 I/O；不记录任何事件原文或隐私数据。

use super::config::{CalibratedWeightConfig, CategoryStats, EventEnrichment, MotiveStats};
use super::weighted::{weighted_mean, weighted_ratio, weighted_variance};
use super::weights::{compute_calibrated_weight, compute_simple_weights_batch};
use ramaria_core::types::{MemoryEvent, Presentation};

// =========================================================
// A3: 按分类聚合
// =========================================================

/// 从事件的关键词中提取主分类标签。
///
/// 策略:
/// - 取 keywords 逗号分隔后的第一个非空标签作为主分类。
/// - 若 keywords 为 None 或为空串，返回 "未分类"。
///
/// 参数:
/// - `event`: 待提取分类的事件。
///
/// 返回:
/// - 主分类标签字符串。
pub fn extract_primary_category(event: &MemoryEvent) -> String {
    event
        .keywords
        .as_ref()
        .and_then(|kw| {
            let first = kw.split(',').next().unwrap_or("").trim();
            if first.is_empty() {
                None
            } else {
                Some(first.to_string())
            }
        })
        .unwrap_or_else(|| "未分类".to_string())
}

/// 按主分类分组事件。
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 分类标签 → 事件列表的映射。按分类标签字典序排列以保证确定性和可复现。
pub fn group_by_category(events: &[MemoryEvent]) -> Vec<(String, Vec<MemoryEvent>)> {
    let mut map: std::collections::BTreeMap<String, Vec<MemoryEvent>> =
        std::collections::BTreeMap::new();
    for event in events {
        let category = extract_primary_category(event);
        map.entry(category).or_default().push(event.clone());
    }
    map.into_iter().collect()
}
// =========================================================
// E: 动机维度二次分组统计
// =========================================================

/// 从事件的 motives 字段中提取动机标签列表。
///
/// 策略:
/// - motives 字段为逗号分隔的字符串（如 "地位维护,自主性"）。
/// - 拆分后 trim 每个标签，过滤空白和空字符串。
/// - 若 `motives` 为 None 或全部标签过滤后为空，返回空 Vec。
///
/// 参数:
/// - `event`: 待提取动机标签的事件。
///
/// 返回:
/// - 去空白后的动机标签列表。无动机时返回空 Vec。
pub fn extract_motive_tags(event: &MemoryEvent) -> Vec<String> {
    match &event.motives {
        Some(s) => {
            let tags: Vec<String> = s
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            tags
        }
        None => Vec::new(),
    }
}

/// 按动机标签分组事件。
///
/// 说明:
/// - 一个事件可能包含多个动机标签，会同时出现在多个分组中。
/// - 这是"二次分组"——在主分类（keywords）之下，按动机标签做二级聚合。
/// - 分组按动机标签字典序排列以保证确定性。
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 动机标签 → 事件列表的映射。仅包含至少 1 个事件的动机标签。
pub fn group_by_motive(events: &[MemoryEvent]) -> Vec<(String, Vec<MemoryEvent>)> {
    let mut map: std::collections::BTreeMap<String, Vec<MemoryEvent>> =
        std::collections::BTreeMap::new();
    for event in events {
        let tags = extract_motive_tags(event);
        for tag in tags {
            map.entry(tag).or_default().push(event.clone());
        }
    }
    map.into_iter().collect()
}

/// 计算全部动机标签的聚合统计。
///
/// 策略:
/// - 对每个动机标签，调用 `compute_category_stats` 复用已有的加权统计算法。
/// - 结果按 `n_eff` 降序排列（有效样本量大的动机优先展示）。
/// - 仅对 confirmed + tentative 事件进行统计，discarded 已在上游排除。
/// - 若所有事件均无 motives 数据，返回空 Vec。
///
/// 参数:
/// - `events`: 活跃事件列表（confirmed + tentative，不含 discarded）。
/// - `enrichments`: 与 events 一一对应的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 按 n_eff 降序排列的动机统计列表。无动机数据时为空。
pub fn compute_motive_stats(
    events: &[MemoryEvent],
    enrichments: &[EventEnrichment],
    config: &CalibratedWeightConfig,
) -> Vec<MotiveStats> {
    if events.is_empty() {
        return Vec::new();
    }

    let grouped = group_by_motive(events);
    if grouped.is_empty() {
        return Vec::new();
    }

    // 按 n_eff 降序排列
    let mut stats: Vec<MotiveStats> = grouped
        .iter()
        .map(|(motive, motive_events)| {
            // 为每个动机分组构造对应的 enrichments 子集
            let motive_enrichments: Vec<EventEnrichment> = motive_events
                .iter()
                .map(|e| {
                    let idx = events.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                    enrichments.get(idx).cloned().unwrap_or_default()
                })
                .collect();

            // 复用 compute_category_stats 计算加权统计量
            let cat_stats =
                compute_category_stats(motive, motive_events, Some(&motive_enrichments), config);

            MotiveStats {
                motive: motive.clone(),
                event_count: cat_stats.event_count,
                n_eff: cat_stats.n_eff,
                valence_mean: cat_stats.valence_mean,
                valence_std: cat_stats.valence_std,
                valence_positive_ratio: cat_stats.valence_positive_ratio,
                share_mean: cat_stats.share_mean,
                share_std: cat_stats.share_std,
                presentation_objective_ratio: cat_stats.presentation_objective_ratio,
                presentation_subjective_ratio: cat_stats.presentation_subjective_ratio,
                presentation_mixed_ratio: cat_stats.presentation_mixed_ratio,
                avg_salience: if motive_events.is_empty() {
                    0.0
                } else {
                    motive_events.iter().map(|e| e.salience).sum::<f64>()
                        / motive_events.len() as f64
                },
            }
        })
        .collect();

    // 按 n_eff 降序排列
    stats.sort_by(|a, b| {
        b.n_eff
            .partial_cmp(&a.n_eff)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    stats
}
/// 计算单个分类的全部统计量（使用校准权重）。
///
/// 参数:
/// - `category`: 分类标签。
/// - `events`: 该分类下的全部事件。
/// - `enrichments`: 与 events 一一对应的增强数据。若为 None，使用简单权重（向后兼容）。
/// - `config`: 校准权重链配置（仅在 enrichments 不为 None 时使用）。
///
/// 返回:
/// - 包含所有加权统计量的 CategoryStats。
pub fn compute_category_stats(
    category: &str,
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> CategoryStats {
    let event_count = events.len();

    // 根据是否提供增强数据选择权重计算方式
    let weights: Vec<f64> = match enrichments {
        Some(enr) => {
            assert_eq!(
                events.len(),
                enr.len(),
                "events 与 enrichments 长度必须一致"
            );
            events
                .iter()
                .zip(enr.iter())
                .map(|(event, enrichment)| compute_calibrated_weight(event, enrichment, config))
                .collect()
        }
        None => compute_simple_weights_batch(events),
    };

    let n_eff: f64 = weights.iter().sum();

    // 效价特征
    let valences: Vec<f64> = events.iter().map(|e| e.valence).collect();
    let valence_mean = weighted_mean(&valences, &weights);
    let valence_std = weighted_variance(&valences, &weights, valence_mean).sqrt();
    let valence_positive: Vec<f64> = events
        .iter()
        .map(|e| if e.valence > 0.0 { 1.0 } else { 0.0 })
        .collect();
    let valence_positive_ratio = weighted_ratio(&valence_positive, &weights);

    // 分享意愿特征
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let share_mean = weighted_mean(&shares, &weights);
    let share_std = weighted_variance(&shares, &weights, share_mean).sqrt();

    // 表达特征 —— 单次遍历收集三种 presentation 指示器
    let mut is_objective = Vec::with_capacity(event_count);
    let mut is_subjective = Vec::with_capacity(event_count);
    let mut is_mixed = Vec::with_capacity(event_count);
    for e in events {
        is_objective.push(if matches!(e.presentation, Presentation::Objective) {
            1.0
        } else {
            0.0
        });
        is_subjective.push(if matches!(e.presentation, Presentation::Subjective) {
            1.0
        } else {
            0.0
        });
        is_mixed.push(if matches!(e.presentation, Presentation::Mixed) {
            1.0
        } else {
            0.0
        });
    }

    let presentation_objective_ratio = weighted_ratio(&is_objective, &weights);
    let presentation_subjective_ratio = weighted_ratio(&is_subjective, &weights);
    let presentation_mixed_ratio = weighted_ratio(&is_mixed, &weights);

    // 平均 salience（供后续 group_weight 计算）
    let avg_salience = if event_count > 0 {
        events.iter().map(|e| e.salience).sum::<f64>() / event_count as f64
    } else {
        0.0
    };

    CategoryStats {
        category: category.to_string(),
        event_count,
        n_eff,
        valence_mean,
        valence_std,
        valence_positive_ratio,
        share_mean,
        share_std,
        presentation_objective_ratio,
        presentation_subjective_ratio,
        presentation_mixed_ratio,
        group_weight: n_eff * avg_salience, // 临时值，后续按全局归一化
    }
}
