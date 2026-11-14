//! crates/ramaria-eval/src/score/retrieval.rs - Ramaria 评测消息级检索指标
//!
//! 设计特点:
//! - 纯计算 + 解析器抽象：不碰引擎、不做 I/O，指标可脱离库直接单测
//! - 标签解析集中一处：L1 / L2 / L0 三个前缀，graph、未知前缀与解析失败都记为不可映射
//! - 预算前取融合结果顺序，预算后取注入条目顺序再接 utt 候选名次
//! - 覆盖率按消息口径：单元消息并集与 gold 求交，不可映射单元不参与分子分母
//! - 精确率按单元口径：分母只算可映射单元，单元粒度不被消息条数加权

use std::collections::HashSet;

use ramaria_service::{EvalRecallObservation, RetrievalTrace};

use super::artifact::ItemRetrieval;
use crate::run::RunItemRecord;

// =========================================================
// 解析器抽象
// =========================================================

/// 单元标签 → 覆盖的消息 id 集合。
///
/// 返回:
/// - `Some(messages)`: 该单元可映射（列表可能为空，表示区间缺失或未回填）；
/// - `None`: 该单元不可映射（前缀未知、主键形态不符或库侧解析失败）。
pub trait SourceResolver {
    /// 解析一个单元的覆盖消息。
    ///
    /// 参数:
    /// - `layer`: 单元层级（`l1` / `l2` / `utt`）；
    /// - `id`: 单元主键。
    fn resolve(&mut self, layer: &str, id: &str) -> Option<Vec<String>>;
}

impl<F> SourceResolver for F
where
    F: FnMut(&str, &str) -> Option<Vec<String>>,
{
    fn resolve(&mut self, layer: &str, id: &str) -> Option<Vec<String>> {
        self(layer, id)
    }
}

/// 解析召回标签。
///
/// 参数:
/// - `label`: 检索轨迹里的文档标识（`L1:{uuid}` / `L2:{id}` / `L0:{id}`）。
///
/// 返回:
/// - 可映射时返回 `(层名, 主键)`；
/// - `graph:`、`L3:`、未知前缀、缺少分隔符或主键为空返回 `None`。
pub fn parse_label(label: &str) -> Option<(&str, &str)> {
    let (prefix, id) = label.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    let layer = match prefix {
        "L1" => "l1",
        "L2" => "l2",
        // 原文块在轨迹里记作 L0:{id}，存储层按 utt 读块区间。
        "L0" => "utt",
        _ => return None,
    };
    Some((layer, id))
}

// =========================================================
// 集合指标
// =========================================================

/// 一个单元集合上的消息级指标。
///
/// 字段约定:
/// - `coverage`: gold 被集合内单元消息并集覆盖的比例（0..=1）；
/// - `first_hit_rank`: 首个命中单元在集合中的位置（1 起），未命中为 `None`；
/// - `units`: 集合单元数（含不可映射单元）；
/// - `unmappable`: 不可映射单元数；
/// - `hit_units`: 消息并集与 gold 相交的单元数（精确率分子）。
#[derive(Debug, Clone, PartialEq)]
pub struct SetMetrics {
    pub coverage: f64,
    pub first_hit_rank: Option<usize>,
    pub units: usize,
    pub unmappable: usize,
    pub hit_units: usize,
}

/// 精确率 = 命中 gold 的单元数 / 可映射单元数；分母为 0 时为 0。
pub fn precision(hit_units: usize, mappable_units: usize) -> f64 {
    if mappable_units == 0 {
        0.0
    } else {
        hit_units as f64 / mappable_units as f64
    }
}

/// 预算前集合：融合结果按产物存储顺序。
pub fn labels_before(trace: &RetrievalTrace) -> Vec<String> {
    trace.fused.iter().map(|entry| entry.id.clone()).collect()
}

/// 预算后集合：注入条目按存储顺序，再接 utt 候选的名次顺序。
///
/// 说明:
/// - utt 候选只在通道生效且有渲染体量时计入，未注入的候选不算预算后集合；
/// - 候选按 `rank` 升序拼接（稳定排序），同 rank 保持存储顺序。
pub fn labels_after(recall: &EvalRecallObservation) -> Vec<String> {
    let mut labels: Vec<String> = recall.items.iter().map(|item| item.id.clone()).collect();
    if recall.trace.utt.active && recall.trace.utt.rendered_chars > 0 {
        let mut candidates: Vec<&ramaria_service::ChannelCandidate> =
            recall.trace.utt.candidates.iter().collect();
        candidates.sort_by_key(|candidate| candidate.rank);
        labels.extend(candidates.into_iter().map(|candidate| candidate.id.clone()));
    }
    labels
}

/// 统计一个单元集合的消息级指标。
///
/// 参数:
/// - `labels`: 单元标签，保持集合内顺序（顺序决定首个命中名次）；
/// - `gold`: 参考消息 id 集合（非空由调用方保证）；
/// - `resolver`: 单元 → 消息解析器。
///
/// 返回:
/// - 覆盖率 = |gold ∩ 单元消息并集| / |gold|；
/// - 首个命中名次 = 第一个消息并集与 gold 相交的单元位置（1 起）；
/// - 精确率分子 = 与 gold 相交的单元数，分母 = 可映射单元数；
/// - 不可映射单元（前缀未知 / 主键为空 / 库侧解析失败）只计数，不进入两率的分子与分母。
pub fn measure_units(
    labels: &[String],
    gold: &HashSet<String>,
    resolver: &mut dyn SourceResolver,
) -> SetMetrics {
    let mut hit: HashSet<&str> = HashSet::new();
    let mut first_hit_rank = None;
    let mut unmappable = 0usize;
    let mut hit_units = 0usize;

    for (index, label) in labels.iter().enumerate() {
        let Some((layer, id)) = parse_label(label) else {
            unmappable += 1;
            continue;
        };
        let Some(messages) = resolver.resolve(layer, id) else {
            unmappable += 1;
            continue;
        };
        let mut intersects = false;
        for message in &messages {
            if let Some(gold_id) = gold.get(message.as_str()) {
                intersects = true;
                hit.insert(gold_id.as_str());
            }
        }
        if intersects {
            hit_units += 1;
            if first_hit_rank.is_none() {
                first_hit_rank = Some(index + 1);
            }
        }
    }
    let coverage = if gold.is_empty() {
        0.0
    } else {
        hit.len() as f64 / gold.len() as f64
    };

    SetMetrics {
        coverage,
        first_hit_rank,
        units: labels.len(),
        unmappable,
        hit_units,
    }
}

/// 单题两口径指标。
///
/// 参数:
/// - `record`: 逐题产物（需要 gold 与召回观测）；
/// - `resolver`: 单元 → 消息解析器。
///
/// 返回:
/// - 空 gold 或失败行（无召回观测）返回 `None`，表示本题不参与检索指标；
/// - 否则返回预算前 / 预算后的覆盖率、精确率与首个命中名次。
pub fn measure_item(
    record: &RunItemRecord,
    resolver: &mut dyn SourceResolver,
) -> Option<ItemRetrieval> {
    if record.gold_message_ids.is_empty() {
        return None;
    }
    let recall = record.recall.as_ref()?;
    let gold: HashSet<String> = record.gold_message_ids.iter().cloned().collect();

    let before = measure_units(&labels_before(&recall.trace.retrieval), &gold, resolver);
    let after = measure_units(&labels_after(recall), &gold, resolver);

    Some(ItemRetrieval {
        coverage_pre: before.coverage,
        coverage_post: after.coverage,
        precision_pre: precision(before.hit_units, before.units - before.unmappable),
        precision_post: precision(after.hit_units, after.units - after.unmappable),
        rank_pre: before.first_hit_rank,
        rank_post: after.first_hit_rank,
        units_pre: before.units,
        units_post: after.units,
        unmappable_pre: before.unmappable,
        unmappable_post: after.unmappable,
        artifact: None,
    })
}
