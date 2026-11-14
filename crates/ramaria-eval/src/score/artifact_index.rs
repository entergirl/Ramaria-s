//! crates/ramaria-eval/src/score/artifact_index.rs - Ramaria 评测 artifact 级指标
//!
//! 设计特点:
//! - 映射只读一次：档位 artifacts.json 建 message → artifact 反查表，跨题复用
//! - 缺文件不是错误：没有映射时整列留 null，由上层记一条缺项
//! - gold 只认映射表里存在的 evidence：找不到的计入 missing_gold，不进覆盖率分母
//! - 单元集合口径与消息级完全一致：预算前取融合结果，预算后取注入条目接 utt 候选
//! - 精确率分母只含可映射到 artifact 的单元：无归属消息不拉低精确率
//! - 纯计算 + 解析器抽象：不碰引擎、不做 I/O，指标可脱离库直接单测

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ramaria_core::error::RamariaResult;

use super::artifact::ItemArtifactRetrieval;
use super::retrieval::{self, SourceResolver};
use crate::baseline::artifacts::{self, ArtifactMap};
use crate::run::RunItemRecord;

// =========================================================
// 反查表
// =========================================================

/// 一个档位的 artifact 反查表。
///
/// 字段约定:
/// - `message_to_artifact`: 消息 id → （样本键, artifact 标识）；
/// - `sample_artifacts`: 样本键 → 该样本的 artifact 标识集合。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ArtifactIndex {
    message_to_artifact: HashMap<String, (String, String)>,
    sample_artifacts: HashMap<String, HashSet<String>>,
}

impl ArtifactIndex {
    /// 读取档位目录下的 artifacts.json 并建表；文件不存在返回 `None`。
    ///
    /// 参数:
    /// - `dir`: 档位产物目录（目录名即档位标识）。
    ///
    /// 返回:
    /// - 缺文件返回 `None`（该条件 artifact 列留 null）；
    /// - 文件不自洽返回 `Validation`，文案带路径。
    pub fn load(dir: &Path) -> RamariaResult<Option<Self>> {
        Ok(artifacts::load(dir)?.map(|map| Self::from_map(&map)))
    }

    /// 由映射建反查表。
    ///
    /// 说明: 同一消息只出现在一个 artifact 下；重复出现时以后写入的为准。
    pub fn from_map(map: &ArtifactMap) -> Self {
        let mut message_to_artifact = HashMap::new();
        let mut sample_artifacts: HashMap<String, HashSet<String>> = HashMap::new();
        for (sample_key, artifacts) in &map.samples {
            let entry = sample_artifacts.entry(sample_key.clone()).or_default();
            for (artifact_id, messages) in artifacts {
                entry.insert(artifact_id.clone());
                for message in messages {
                    message_to_artifact
                        .insert(message.clone(), (sample_key.clone(), artifact_id.clone()));
                }
            }
        }
        Self {
            message_to_artifact,
            sample_artifacts,
        }
    }

    /// 消息所属的 artifact 标识。
    pub fn artifact_of(&self, message: &str) -> Option<&str> {
        self.message_to_artifact
            .get(message)
            .map(|(_, artifact)| artifact.as_str())
    }

    /// 该样本的映射表是否收录了给定 artifact。
    pub fn contains(&self, sample_key: &str, artifact: &str) -> bool {
        self.sample_artifacts
            .get(sample_key)
            .is_some_and(|artifacts| artifacts.contains(artifact))
    }
}

// =========================================================
// 单题指标
// =========================================================

/// 单题 artifact 级测量结果。
///
/// 字段约定:
/// - `item`: 参与均值用的单题指标；gold artifact 为空时为 `None`；
/// - `missing_gold`: 该题 evidence 中不在映射表里的条数；
/// - `units_pre` / `units_post`: 两个集合里可映射到 artifact 的单元数。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactMeasure {
    pub item: Option<ItemArtifactRetrieval>,
    pub missing_gold: usize,
    pub units_pre: usize,
    pub units_post: usize,
}

/// 一个单元集合上的 artifact 级指标。
///
/// 字段约定:
/// - `coverage`: gold artifact 覆盖率；
/// - `first_hit_rank`: 首个命中单元位置（1 起）；
/// - `units`: 可映射到 artifact 的单元数（精确率分母）；
/// - `hit_units`: artifact 并集与 gold 相交的单元数（精确率分子）。
#[derive(Debug, Clone, PartialEq)]
pub struct SetMetrics {
    pub coverage: f64,
    pub first_hit_rank: Option<usize>,
    pub units: usize,
    pub hit_units: usize,
}

/// 统计一个单元集合上的 artifact 级指标。
///
/// 参数:
/// - `labels`: 单元标签，保持集合内顺序（顺序决定首个命中名次）；
/// - `gold`: gold artifact 集合（非空由调用方保证）；
/// - `resolver`: 单元 → 消息解析器；
/// - `index`: 消息 → artifact 反查表。
///
/// 返回:
/// - 覆盖率 = |gold ∩ 单元覆盖 artifact 并集| / |gold|；
/// - 首个命中名次 = 第一个 artifact 并集与 gold 相交的单元位置（1 起）；
/// - `units` 只计可映射到 artifact 的单元，同时作为精确率分母；
/// - 不可映射单元与无 artifact 归属的消息都不计数。
pub fn measure_units(
    labels: &[String],
    gold: &HashSet<String>,
    resolver: &mut dyn SourceResolver,
    index: &ArtifactIndex,
) -> SetMetrics {
    let mut hit: HashSet<&str> = HashSet::new();
    let mut first_hit_rank = None;
    let mut units = 0usize;
    let mut hit_units = 0usize;

    for (position, label) in labels.iter().enumerate() {
        let Some((layer, id)) = retrieval::parse_label(label) else {
            continue;
        };
        let Some(messages) = resolver.resolve(layer, id) else {
            continue;
        };
        let mut mapped = false;
        let mut intersects = false;
        for message in &messages {
            let Some(artifact) = index.artifact_of(message) else {
                continue;
            };
            mapped = true;
            if gold.contains(artifact) {
                intersects = true;
                hit.insert(artifact);
            }
        }
        if !mapped {
            continue;
        }
        units += 1;
        if intersects {
            hit_units += 1;
            if first_hit_rank.is_none() {
                first_hit_rank = Some(position + 1);
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
        units,
        hit_units,
    }
}

/// 单题 artifact 级测量。
///
/// 参数:
/// - `record`: 逐题产物（需要 evidence、样本键与召回观测）；
/// - `resolver`: 单元 → 消息解析器；
/// - `index`: 该档位的 artifact 反查表。
///
/// 返回:
/// - 无召回观测（失败行）返回 `None`，表示该题不参与 artifact 口径；
/// - 其余返回测量结果；gold artifact 为空时 `item` 为 `None`，缺失 evidence 仍计入条件累计。
pub fn measure_item(
    record: &RunItemRecord,
    resolver: &mut dyn SourceResolver,
    index: &ArtifactIndex,
) -> Option<ArtifactMeasure> {
    let recall = record.recall.as_ref()?;
    let (gold, missing_gold) = gold_artifacts(record, index);

    let before = measure_units(
        &retrieval::labels_before(&recall.trace.retrieval),
        &gold,
        resolver,
        index,
    );
    let after = measure_units(&retrieval::labels_after(recall), &gold, resolver, index);

    let item = if gold.is_empty() {
        None
    } else {
        Some(ItemArtifactRetrieval {
            coverage_pre: before.coverage,
            coverage_post: after.coverage,
            precision_pre: retrieval::precision(before.hit_units, before.units),
            precision_post: retrieval::precision(after.hit_units, after.units),
            rank_pre: before.first_hit_rank,
            rank_post: after.first_hit_rank,
            gold_artifacts: gold.len(),
            missing_gold,
        })
    };

    Some(ArtifactMeasure {
        item,
        missing_gold,
        units_pre: before.units,
        units_post: after.units,
    })
}

/// 该题 gold artifact 集合与缺失 evidence 数。
///
/// 说明: 只有映射表里存在的 evidence 才进 gold；其余按缺失计数，不参与覆盖率分母。
fn gold_artifacts(record: &RunItemRecord, index: &ArtifactIndex) -> (HashSet<String>, usize) {
    let mut gold = HashSet::new();
    let mut missing = 0usize;
    for evidence in &record.evidence {
        if index.contains(&record.sample_key, evidence) {
            gold.insert(evidence.clone());
        } else {
            missing += 1;
        }
    }
    (gold, missing)
}
