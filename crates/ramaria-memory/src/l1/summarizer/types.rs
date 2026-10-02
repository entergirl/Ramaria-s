//! crates/ramaria-memory/src/l1/summarizer/types.rs - L1 LLM 响应反序列化目标
//!
//! 设计特点:
//! - L1SummaryResponse 全部字段 `Option`，容忍 LLM 输出缺失（校验阶段再填默认值）。
//! - evidence_notes 走自定义反序列化器，兼容对象数组 / 旧字符串数组 / 缺失。
//! - situation_strength 缺失时由配置注入或默认 3；continuation 无上一块时不输出。

use ramaria_core::types::EvidenceNote;
use serde::Deserialize;

// =========================================================
// LLM 响应 JSON 结构（反序列化目标）
// =========================================================

/// LLM 返回的 L1 摘要 JSON 结构。
///
/// 字段:
/// - 所有字段均为 `Option`，以容忍 LLM 输出缺失字段。
/// - 校验阶段再填充默认值，避免解析阶段 panic。
/// - `situation_strength` 为新增字段，prompt 已包含此输出，缺失时由 config 注入或默认 3。
/// - `evidence_notes` 为结构化证据线索（v1.4），LLM 可能输出缺失或空数组，
///   校验失败时降级为 `Some(vec![])` 但不阻塞 L1 生成。
///   M1~M3 过渡期 LLM 仍可能输出旧字符串数组，由自定义反序列化器统一转换为
///   对象数组（字符串落 `text` 槽位）；M4 起 prompt 升级为对象数组。
#[derive(Debug, Deserialize)]
pub(super) struct L1SummaryResponse {
    pub(super) summary: Option<String>,
    pub(super) keywords: Option<String>,
    pub(super) time_period: Option<String>,
    pub(super) atmosphere: Option<String>,
    pub(super) valence: Option<f64>,
    pub(super) salience: Option<f64>,
    /// 情境强度 1-5，None 时按默认值 3 处理
    #[serde(default)]
    pub(super) situation_strength: Option<i32>,
    /// 证据线索列表（宽容解析：对象数组 / 旧字符串数组 / 缺失）
    #[serde(default, deserialize_with = "deserialize_evidence_notes")]
    pub(super) evidence_notes: Option<Vec<EvidenceNote>>,
    /// 相对上一块的话题延续关系（v1.5 B2）：延续/转折/无关。
    /// 无上一块时 prompt 不含该字段，LLM 不会输出 → None。
    #[serde(default)]
    pub(super) continuation: Option<String>,
}

// =========================================================
// evidence_notes 宽容反序列化 + 校验
// =========================================================

/// 宽容反序列化 evidence_notes（v1.4 结构化升级的过渡兼容）。
///
/// 兼容三种输入:
/// 1. 对象数组 `[{"text": "...", "time": ..., "who": ..., "cause": ...}]` — 直接解析
/// 2. 旧字符串数组 `["...", "..."]` — 字符串落 `text` 槽位，其余置空
/// 3. 缺失 / null / 非数组 — 返回 None
///
/// 说明:
/// - 存储层（memory_l1 表）格式约定：只读写新格式，无旧格式解析分支（见 docs/dev-1.5/v1.5-decisions.md）；
///   此处的宽容解析仅针对 LLM 输出（M4 之前 prompt 仍为旧格式）。
fn deserialize_evidence_notes<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<EvidenceNote>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        _ => return Ok(None),
    };

    let mut notes = Vec::with_capacity(items.len());
    for item in items {
        match item {
            // 旧格式：字符串 → text 槽位
            serde_json::Value::String(s) => notes.push(EvidenceNote::new(s)),
            // 新格式：对象 → 结构化解析（缺失字段回退 None）
            serde_json::Value::Object(_) => match serde_json::from_value::<EvidenceNote>(item) {
                Ok(note) => notes.push(note),
                Err(e) => {
                    tracing::warn!(error = %e, "evidence_notes 条目解析失败，跳过该条");
                }
            },
            other => {
                tracing::warn!(
                    kind = %other,
                    "evidence_notes 条目类型非法（应为字符串或对象），跳过该条"
                );
            }
        }
    }
    Ok(Some(notes))
}
