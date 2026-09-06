//! crates/ramaria-memory/src/event/extractor/parse.rs - LLM 响应解析与事件 JSON 结构
//!
//! 设计特点:
//! - 定义 LLM 提取结果的反序列化结构（新格式对象 / 旧格式数组 / 单对象）。
//! - 统一 `EventResponse` → `ParsedExtractionResult` 的转换入口（`into_result`）。
//! - 提供 LLM 输出字段到领域类型的解析（presentation / relation kind / 时间戳）。
//! - 仅被父模块 `extractor` 依赖，项以 `pub(super)` 对外可见。

use ramaria_core::types::{EventRelationKind, Presentation};
use ramaria_core::{RamariaError, RamariaResult};
use serde::Deserialize;

// =========================================================
// LLM 响应 JSON 结构
// =========================================================

/// LLM 返回的单条事件 JSON。
///
/// 说明:
/// - 所有字段为 `Option`，容忍 LLM 输出缺失字段。
/// - 校验阶段填充默认值。
/// - 新增 `motives` 字段（底层动机标签列表）。
#[derive(Debug, Deserialize)]
pub(super) struct ExtractedEventJson {
    pub(super) title: Option<String>,
    pub(super) summary: Option<String>,
    pub(super) keywords: Option<String>,
    pub(super) participants: Option<serde_json::Value>, // JSON 数组或 null
    pub(super) confidence: Option<f64>,
    pub(super) salience: Option<f64>,
    pub(super) valence: Option<f64>,
    pub(super) presentation: Option<String>,
    pub(super) share: Option<f64>,
    pub(super) attitude: Option<String>,
    /// 底层动机标签列表，如 ["地位维护", "自主性"]
    #[serde(default)]
    pub(super) motives: Option<Vec<String>>,
}

/// LLM 返回的事件关系。
///
/// 字段约定:
/// - `from_index` / `to_index`: 引用 events 数组中的事件索引（从 0 开始）。
/// - `kind`: 六种关系类型之一。
/// - `weight`: 关系确信度 0.0..1.0。
#[derive(Debug, Deserialize)]
pub(super) struct EventRelationOutput {
    pub(super) from_index: usize,
    pub(super) to_index: usize,
    pub(super) kind: String,
    #[serde(default = "default_relation_weight")]
    pub(super) weight: f64,
    /// 关系逻辑的简要说明（由 LLM 输出，当前仅用于 Prompt 引导，未持久化存储）
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) detail: Option<String>,
}

fn default_relation_weight() -> f64 {
    0.5
}

/// LLM 返回的完整提取结果（events + relations）。
///
/// 字段约定:
/// - `events` 为必填字段（无 `#[serde(default)]`），用于区分新格式与旧格式单事件对象。
///   若 JSON 对象不含 `"events"` 键，serde 按缺少必填字段报错，降级到 `Array`/`Single` 变体。
/// - `relations` 为可选字段（`#[serde(default)]`），缺失时等价于 `None`。
#[derive(Debug, Deserialize)]
pub(super) struct EventExtractionResponse {
    pub(super) events: Vec<ExtractedEventJson>,
    #[serde(default)]
    pub(super) relations: Option<Vec<EventRelationOutput>>,
}

/// LLM 返回的顶层结构：支持新旧两种格式。
///
/// 解析策略:
/// 1. 尝试解析为 `EventExtractionResponse`（新格式: {"events": [...], "relations": [...]}）
/// 2. 尝试解析为 `Vec<ExtractedEventJson>` 数组（旧格式: [...]）
/// 3. 尝试解析为单对象 `ExtractedEventJson`，包装为单元素数组
/// 4. 失败 → 触发降级
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum EventResponse {
    Object(EventExtractionResponse),
    Array(Vec<ExtractedEventJson>),
    // 兼容 LLM 偶尔返回单对象而非数组
    Single(serde_json::Value),
}

/// 解析后的完整结果：事件列表 + 可选的关系列表。
///
/// 职责:
/// - 将 `EventResponse` 统一为此结构，供 `extract_events` 后续处理。
#[derive(Debug)]
pub(super) struct ParsedExtractionResult {
    pub(super) events: Vec<ExtractedEventJson>,
    pub(super) relations: Option<Vec<EventRelationOutput>>,
}

// =========================================================
// EventResponse 辅助
// =========================================================

impl EventResponse {
    /// 将 `EventResponse` 转为统一的 `ParsedExtractionResult`。
    ///
    /// 处理四种 LLM 返回形式:
    /// 1. 新格式: `{"events": [...], "relations": [...]}` → Object
    /// 2. 旧格式: JSON 数组 `[...]` → Array
    /// 3. 单个对象 → 包装为单元素事件列表
    /// 4. 嵌套数组（罕见）→ 二次解包
    pub(super) fn into_result(self) -> RamariaResult<ParsedExtractionResult> {
        match self {
            EventResponse::Object(resp) => Ok(ParsedExtractionResult {
                events: resp.events,
                relations: resp.relations,
            }),
            EventResponse::Array(events) => Ok(ParsedExtractionResult {
                events,
                relations: None,
            }),
            EventResponse::Single(val) => {
                // 尝试将单个对象包装为单元素事件列表
                if val.is_object() {
                    if let Ok(event) = serde_json::from_value::<ExtractedEventJson>(val) {
                        return Ok(ParsedExtractionResult {
                            events: vec![event],
                            relations: None,
                        });
                    }
                    return Err(RamariaError::validation("事件 JSON 对象反序列化失败"));
                }
                // 可能是嵌套数组（LLM 偶尔返回 [[...]] 形式）
                if let Ok(events) = serde_json::from_value::<Vec<ExtractedEventJson>>(val) {
                    return Ok(ParsedExtractionResult {
                        events,
                        relations: None,
                    });
                }
                Err(RamariaError::validation("事件响应格式无法识别"))
            }
        }
    }
}

pub(super) fn parse_presentation(s: Option<&str>) -> Presentation {
    match s {
        Some("objective") => Presentation::Objective,
        Some("subjective") => Presentation::Subjective,
        Some("mixed") => Presentation::Mixed,
        _ => Presentation::Mixed,
    }
}

/// 将 LLM 输出的关系类型字符串解析为 `EventRelationKind`。
///
/// 说明:
/// - 六种标准关系类型：CausedBy/PartOf/RelatedTo/ContinuedBy/Contradicts/Timeline
/// - 不可识别时默认 `RelatedTo`，并记录 warn 日志。
pub(super) fn parse_relation_kind(s: &str) -> EventRelationKind {
    match s {
        "CausedBy" => EventRelationKind::CausedBy,
        "PartOf" => EventRelationKind::PartOf,
        "RelatedTo" => EventRelationKind::RelatedTo,
        "ContinuedBy" => EventRelationKind::ContinuedBy,
        "Contradicts" => EventRelationKind::Contradicts,
        "Timeline" => EventRelationKind::Timeline,
        other => {
            tracing::warn!(kind = other, "未知事件关系类型，降级为 RelatedTo");
            EventRelationKind::RelatedTo
        }
    }
}

/// 将 Unix 毫秒时间戳转为 `YYYY-MM-DD` 字符串。
///
/// 用途: L1 摘要列表格式化，供 LLM 理解事件时间顺序。
///
/// 说明: 使用简化的儒略日算法，仅用于显示，不追求高精度到秒。
pub(super) fn timestamp_to_date_str(ts_ms: i64) -> String {
    let total_secs = ts_ms / 1000;
    let days_since_epoch = total_secs / 86400;
    let (y, m, d) = days_to_ymd(days_since_epoch);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(total_days: i64) -> (i64, u32, u32) {
    let mut days = total_days;
    let mut year = 1970i64;

    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let month_days: [i64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u32;
    for &md in month_days.iter() {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, (days + 1) as u32)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}
