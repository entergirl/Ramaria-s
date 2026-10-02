//! crates/ramaria-memory/src/event/extractor/convert.rs - LLM 响应解析与事件构建
//!
//! 设计特点:
//! - `parse_event_response`: 三步递进 JSON 解析（直接 → 剥离 think 标签 → 正则提取）
//! - `build_event`: 从 JSON 构建 MemoryEvent，motives 过滤空串后逗号分隔存储
//! - `format_l1_from_cluster`: 从 TopicCluster 格式化 L1 摘要（含结构化证据线索）
//! - 仅被父模块 `extractor` 调用，方法以 `pub(super)` 对外可见
//! - 纯计算，不写库；原文不落日志（隐私红线）

use ramaria_core::{MemoryEvent, RamariaError, RamariaResult};

use super::EventExtractor;
use super::parse::{
    EventResponse, ExtractedEventJson, ParsedExtractionResult, parse_presentation,
    timestamp_to_date_str,
};

impl<'a> EventExtractor<'a> {
    /// 从 TopicCluster 格式化 L1 摘要列表。
    ///
    /// 格式（v1.4 M4：每条 L1 可附带结构化证据线索行）:
    /// ```text
    /// [1] 2025-06-01 摘要文本 (keywords: kw1, kw2)
    /// [线索] 证据文本（time: 上周三；who: 用户；cause: 需求变更频繁）
    /// ```
    ///
    /// 说明:
    /// - evidence_notes 非空时，在摘要行下方输出 `[线索]` 行，
    ///   槽位仅展示非空项（缺失槽位省略，避免空占位干扰 LLM）。
    /// - cause 槽位承载因果线索，供 L2 事件提取作为背景参考（不视为事实断言）。
    pub(super) fn format_l1_from_cluster(cluster: &crate::event::batcher::TopicCluster) -> String {
        let mut lines = Vec::with_capacity(cluster.l1_items.len());
        for (i, item) in cluster.l1_items.iter().enumerate() {
            let date = timestamp_to_date_str(item.created_at);
            let kw_str = if item.keywords.is_empty() {
                String::new()
            } else {
                let kw_list: Vec<&str> = item.keywords.iter().map(|k| k.as_str()).collect();
                format!(" (keywords: {})", kw_list.join(", "))
            };
            lines.push(format!("[{}] {} {}{}", i + 1, date, item.summary, kw_str));

            // 结构化证据线索行（v1.4 M4）：非空时输出，槽位仅列非空项
            if !item.evidence_notes.is_empty() {
                for note in &item.evidence_notes {
                    let mut slots: Vec<String> = Vec::with_capacity(3);
                    if let Some(time) = note.time.as_deref() {
                        slots.push(format!("time: {time}"));
                    }
                    if let Some(who) = note.who.as_deref() {
                        slots.push(format!("who: {who}"));
                    }
                    if let Some(cause) = note.cause.as_deref() {
                        slots.push(format!("cause: {cause}"));
                    }
                    let slot_str = if slots.is_empty() {
                        String::new()
                    } else {
                        format!("（{}）", slots.join("；"))
                    };
                    lines.push(format!("[线索] {}{}", note.text, slot_str));
                }
            }
        }
        lines.join("\n")
    }

    /// 解析 LLM 响应为事件列表和关系列表。
    ///
    /// 三步递进策略（与 summarizer 一致）:
    /// 1. 直接 `serde_json::from_str`
    /// 2. 剥离 `<think>...</think>` 标签后重试
    /// 3. 正则提取 JSON 数组/对象
    ///
    /// 返回 `ParsedExtractionResult`，包含 events 和可选的 relations。
    pub(super) fn parse_event_response(raw: &str) -> RamariaResult<ParsedExtractionResult> {
        // 步骤 1: 直接解析
        if let Ok(response) = serde_json::from_str::<EventResponse>(raw) {
            return response.into_result();
        }

        // 步骤 2: 剥离 think 标签
        let stripped = crate::utils::strip_thinking(raw);
        if stripped != raw
            && let Ok(response) = serde_json::from_str::<EventResponse>(&stripped)
        {
            return response.into_result();
        }

        // 步骤 3: 正则提取
        if let Some(extracted) = crate::utils::extract_first_json_array(raw)
            && let Ok(response) = serde_json::from_str::<EventResponse>(&extracted)
        {
            return response.into_result();
        }

        // 步骤 3b: LLM 可能返回完整的 JSON 对象（含 events/relations），
        // 而 extract_first_json_array 仅提取数组。尝试正则提取 JSON 对象 {...}。
        if let Some(obj_str) = crate::utils::extract_first_json_object(raw)
            && let Ok(response) = serde_json::from_str::<EventResponse>(&obj_str)
        {
            return response.into_result();
        }

        Err(RamariaError::validation(format!(
            "事件 JSON 解析失败，原始响应 {} 字符（不记录原文，防隐私泄漏）",
            raw.chars().count()
        )))
    }

    /// 从 ExtractedEventJson 构建 MemoryEvent。
    ///
    /// motives 从 JSON 提取，过滤空字符串后以逗号分隔存储。
    ///
    /// 参数:
    /// - `situation_strength`: 从源 L1 传播的情境强度（1-5），
    ///   None 时等效 3（中性情境， 加权 ×1.0）。
    pub(super) fn build_event(
        persona_uid: &str,
        json: ExtractedEventJson,
        start: i64,
        end: i64,
        now: i64,
        situation_strength: Option<i32>,
    ) -> MemoryEvent {
        let title = json.title.as_deref().unwrap_or("").trim().to_string();
        let title = if title.is_empty() || title.chars().count() > 20 {
            let truncated = ramaria_core::text::truncate_chars_bare(&title, 20);
            if truncated.is_empty() {
                "（无标题事件）".to_string()
            } else {
                truncated
            }
        } else {
            title
        };

        let summary = json
            .summary
            .as_deref()
            .unwrap_or("（无描述）")
            .trim()
            .to_string();
        let summary = if summary.is_empty() {
            "（无描述）".to_string()
        } else {
            summary
        };

        let keywords = json
            .keywords
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let participants = json.participants.as_ref().and_then(|v| match v {
            serde_json::Value::Array(arr) => {
                let names: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if names.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&names).unwrap_or_default())
                }
            }
            _ => None,
        });

        let confidence = json.confidence.unwrap_or(0.5).clamp(0.0, 1.0);
        let salience = crate::utils::clamp_salience(json.salience.unwrap_or(0.5));
        let valence = crate::utils::clamp_valence(json.valence.unwrap_or(0.0));
        let presentation = parse_presentation(json.presentation.as_deref());
        let share = json.share.unwrap_or(0.5).clamp(0.0, 1.0);
        let attitude = json
            .attitude
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // 提取 motives → 过滤空串 → 逗号分隔存储
        let motives = json.motives.and_then(|m| {
            let filtered: Vec<&str> = m
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if filtered.is_empty() {
                None
            } else {
                Some(filtered.join(","))
            }
        });

        MemoryEvent {
            id: 0,
            persona_uid: persona_uid.to_string(),
            title,
            summary,
            keywords,
            participants,
            start,
            end,
            confidence,
            salience,
            valence,
            presentation,
            share,
            attitude,
            paraphrase: None, // 后续异步生成
            absorbed: 0,
            situation_strength,
            motives,
            created_at: now,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        }
    }
}
