//! crates/ramaria-memory/src/event/extractor/relations.rs - 事件关系映射
//!
//! 设计特点:
//! - 将 LLM 输出的关系位置引用映射为批次数组下标
//! - 端点越界/未保存（相似度去重跳过）/自引用均丢弃，不错误连边
//! - 仅被父模块 `extractor` 调用，方法以 `pub(super)` 对外可见
//! - 纯计算，不写库

use ramaria_core::EventRelationKind;

use super::EventExtractor;
use super::parse::{EventRelationOutput, parse_relation_kind};

impl<'a> EventExtractor<'a> {
    /// 将 LLM 返回的事件关系映射为批次下标关系列表（纯计算，不写库）。
    ///
    /// 参数:
    /// - `rels`: LLM 输出的关系列表（from_index/to_index 引用提取结果 events 数组位置）。
    /// - `batch_index_by_position`: 与提取结果等长的并行数组，记录每个位置对应的事件
    ///   在批次数组中的下标；相似度去重跳过/未保存的位置为 `None`。
    /// - `persona_uid`: 人格标识（用于日志）。
    /// - `cluster_idx`: 簇索引（用于日志）。
    ///
    /// 说明:
    /// - LLM 的索引引用"提取结果数组"位置，而不是"批次数组"位置。
    ///   若按压缩后的批次列表直接取下标，相似度去重跳过后会把关系连到错误事件，
    ///   故统一经 `remap_relation` 按位置解析（越界/未保存/自引用均丢弃）。
    /// - 映射结果由调用方追加到 `EventBatchWrite.relations`，随整批单事务写入。
    ///
    /// 返回:
    /// - 成功映射的关系列表（from/to 均为批次数组下标）。
    pub(super) fn map_cluster_relations(
        rels: &[EventRelationOutput],
        batch_index_by_position: &[Option<usize>],
        persona_uid: &str,
        cluster_idx: usize,
    ) -> Vec<(usize, usize, EventRelationKind, f64)> {
        let mut mapped: Vec<(usize, usize, EventRelationKind, f64)> = Vec::new();
        let mut dropped_count: usize = 0;

        for rel in rels {
            // 位置 → 批次下标；端点越界/未保存/自引用返回 None → 丢弃
            let Some((from_idx, to_idx)) = Self::remap_relation(rel, batch_index_by_position)
            else {
                dropped_count += 1;
                continue;
            };

            let kind = parse_relation_kind(&rel.kind);
            mapped.push((from_idx, to_idx, kind, rel.weight.clamp(0.0, 1.0)));
        }

        if dropped_count > 0 {
            tracing::debug!(
                %persona_uid,
                cluster_idx,
                dropped_relation_count = dropped_count,
                "事件关系：部分关系端点未保存（相似度去重/越界/自引用），已丢弃"
            );
        }

        mapped
    }

    /// 将单条 LLM 关系引用映射为批次数组下标对。
    ///
    /// 参数:
    /// - `rel`: LLM 输出的关系（from_index/to_index 引用提取结果 events 数组位置）。
    /// - `batch_index_by_position`: 与提取结果等长的并行数组，记录每个位置对应的
    ///   批次下标，被相似度去重跳过/未保存的位置为 `None`。
    ///
    /// 返回:
    /// - `Some((from_idx, to_idx))`: 两端点均已进入批次且非自引用。
    /// - `None`: 端点越界、端点未保存或自引用——调用方应丢弃该关系。
    pub(super) fn remap_relation(
        rel: &EventRelationOutput,
        batch_index_by_position: &[Option<usize>],
    ) -> Option<(usize, usize)> {
        let from_idx = batch_index_by_position
            .get(rel.from_index)
            .copied()
            .flatten()?;
        let to_idx = batch_index_by_position
            .get(rel.to_index)
            .copied()
            .flatten()?;
        if from_idx == to_idx {
            return None;
        }
        Some((from_idx, to_idx))
    }
}
