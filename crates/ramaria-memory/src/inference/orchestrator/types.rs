//! crates/ramaria-memory/src/inference/orchestrator/types.rs - Phase B/C 输出类型
//!
//! 设计特点:
//! - PhaseBResult: Phase B 推断结果（新增/更新/废弃数量 + 来源 + 活跃 trait ID/列表）。
//! - PhaseBSource: 推断来源（真实 LLM / mock 降级）。
//! - PhaseCResult: 置信度更新与漂移检测输出。
//! - 纯类型定义，不含逻辑。

use ramaria_core::types::PersonalityTrait;

use crate::inference::confidence::ConfidenceSummary;
use crate::inference::drift::DriftSummary;

/// Phase B 推断结果。
///
/// 职责:
/// - 记录 LLM 推断或降级 mock 推断的完整结果。
/// - 供上层（session_lifecycle）判断是否需要触发 Phase C。
#[derive(Debug, Clone)]
pub struct PhaseBResult {
    /// 本次新增的 trait 数量
    pub traits_saved: usize,
    /// 本次更新的 trait 数量
    pub traits_updated: usize,
    /// 本次标记为废弃的 trait 数量
    pub traits_deprecated: usize,
    /// 推断来源：真实 LLM 推断 或 Mock 降级
    pub source: PhaseBSource,
    /// 本次保存/更新后所有活跃 trait 的 ID 列表（供 Phase C 使用）
    pub trait_ids: Vec<i64>,
    /// 推断产出的 PersonalityTrait 列表（供 Phase C 使用）
    pub traits: Vec<PersonalityTrait>,
}

/// Phase B 推断来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseBSource {
    /// 通过真实 LLM 三步推断产出
    LlmInference,
    /// LLM 调用失败，降级为基于统计规则的 mock 推断
    MockFallback,
}

/// Phase C 置信度更新结果。
///
/// 职责:
/// - 记录置信度更新和漂移检测的完整输出。
#[derive(Debug, Clone)]
pub struct PhaseCResult {
    /// 置信度被更新的 trait 数量
    pub traits_updated: usize,
    /// 新增的证据记录数
    pub evidence_saved: usize,
    /// 是否检测到显著漂移（任一分类 needs_review=true）
    pub has_significant_drift: bool,
    /// 触发漂移的分类列表
    pub drift_categories: Vec<String>,
    /// 详细置信度更新摘要
    pub confidence_summary: Option<ConfidenceSummary>,
    /// 详细漂移检测摘要
    pub drift_summary: Option<DriftSummary>,
}
