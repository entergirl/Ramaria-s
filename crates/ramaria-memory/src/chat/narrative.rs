//! crates/ramaria-memory/src/chat/narrative.rs - 脉络素材（跨会话上下文）
//!
//! 设计特点:
//! - 闸门关闭时直接返回空素材（不查询 retriever/storage）
//! - 加权路径以当前消息为话题依据经 `RetrieverSource::search_narrative` 排序
//! - 检索无结果时回退最近 N 条；无条件路径直接取最近 N 条
//! - 安全约束：不记录完整摘要到日志；日志只记计数与是否有最后活跃时间

use ramaria_core::config::{DecayConfig as CoreDecayConfig, RetrievalConfig};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::MemoryL1;

use crate::bm25::DocId;
use crate::decay::DecayConfig;
use crate::recall::RetrieverSource;
use crate::retriever::SearchResult;

// =========================================================
// 脉络素材（跨会话上下文）
// =========================================================

/// 脉络素材（进入 prompt 近期对话脉络块的摘要行 + 最后活跃时间）。
///
/// 职责:
/// - 承载跨 session 上下文注入所需的近期 L1 摘要文本行与最后活跃时间，
///   供 System Prompt 装配（近期对话脉络块）消费。
///
/// 字段约定:
/// - `recent_summaries`: 近期 L1 摘要列表（预格式化文本行，按加载顺序排列）。
/// - `last_active_at`: 最后活跃时间字符串（`YYYY-MM-DD HH:MM`；无摘要时为 None）。
#[derive(Debug, Clone, Default)]
pub struct NarrativeMaterial {
    /// 近期 L1 摘要列表（预格式化文本行）。
    pub recent_summaries: Vec<String>,
    /// 最后活跃时间字符串（YYYY-MM-DD HH:MM 格式）。
    pub last_active_at: Option<String>,
}

/// 加载脉络素材（在线管线脉络段的单份实现，桌面与服务入口同源复用）。
///
/// 流程:
/// - 闸门关闭（`narrative_enabled=false`）→ 直接返回空素材（不查询 retriever/storage）。
/// - `narrative_weighted=true`（加权路径）：以 `query` 为话题依据经
///   [`RetrieverSource::search_narrative`] 加权排序；检索无结果时回退最近 N 条。
/// - `narrative_weighted=false`（无条件路径）：直接取最近 N 条
///   （`list_recent_l1_by_persona`）。
///
/// 参数:
/// - `storage`: 存储后端（无条件取最近 N 条路径）。
/// - `retriever`: 检索器只读视图（加权路径；未加载按空结果降级）。
/// - `retrieval`: `[retrieval]` 配置（`narrative_weighted` / `narrative_top_k`）。
/// - `decay`: `[decay]` 配置（加权路径的时间衰减）。
/// - `narrative_enabled`: 脉络注入闸门（`[injection].narrative`；false 时直接返回空素材）。
/// - `persona_uid`: 目标人格。
/// - `query`: 当前用户输入（加权路径的相关性输入）。
///
/// 返回:
/// - 脉络素材；任何降级路径都返回空字段而非错误（调用方按空值处理）。
///
/// 降级策略:
/// - 存储读取失败 → warn 日志 + 空摘要列表（不阻塞对话）。
/// - 检索无结果（无 L1 / 索引未加载）→ 回退最近 N 条；仍为空则返回空素材。
///
/// 安全约束:
/// - 不记录完整摘要到日志；日志只记计数与是否有最后活跃时间。
pub async fn load_narrative_material<R: RetrieverSource + ?Sized>(
    storage: &dyn StorageBackend,
    retriever: &R,
    retrieval: &RetrievalConfig,
    decay: &CoreDecayConfig,
    narrative_enabled: bool,
    persona_uid: &str,
    query: &str,
) -> NarrativeMaterial {
    // 脉络注入条数下限：至少 1 条（0 会导致检索与回退路径均为空素材）
    let narrative_top_k = retrieval.narrative_top_k.max(1);
    let recent_l1 = if !narrative_enabled {
        tracing::debug!(
            persona_uid = persona_uid,
            "脉络注入闸门关闭（探针消融），跳过近期 L1 摘要加载"
        );
        Vec::new()
    } else if retrieval.narrative_weighted {
        let now = ramaria_core::types::now_ms();
        let decay_config = DecayConfig::from_core(decay, "l1");
        let narrative_results = retriever.search_narrative(
            query,
            persona_uid,
            narrative_top_k as usize,
            now,
            &decay_config,
        );
        if !narrative_results.is_empty() {
            // 加权命中 → 转回 MemoryL1（脉络行格式与"最近 N 条"路径一致，
            // 缺 time_period/atmosphere 时显示纯摘要——加权优先保证话题相关性，展示次要）
            narrative_results
                .iter()
                .filter_map(search_result_to_memory_l1)
                .collect::<Vec<MemoryL1>>()
        } else {
            // 检索无结果（无 L1 或 query 无相关性命中）→ 回退最近 N 条
            storage
                .list_recent_l1_by_persona(persona_uid, narrative_top_k)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        persona_uid = persona_uid,
                        error = %e,
                        "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                    );
                    Vec::new()
                })
        }
    } else {
        storage
            .list_recent_l1_by_persona(persona_uid, narrative_top_k)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    persona_uid = persona_uid,
                    error = %e,
                    "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                );
                Vec::new()
            })
    };

    // 格式化近期摘要为可读文本行
    let recent_summaries: Vec<String> = recent_l1.iter().map(format_l1_as_context_line).collect();

    // 从最近一条 L1 的创建时间提取最后活跃时间
    let last_active_at: Option<String> = recent_l1.first().map(|l1| {
        let secs = l1.created_at / 1000;
        match chrono::DateTime::from_timestamp(secs, 0) {
            Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
            None => String::new(),
        }
    });

    tracing::debug!(
        persona_uid = persona_uid,
        l1_count = recent_l1.len(),
        has_last_active = last_active_at.is_some(),
        "近期 L1 摘要已加载"
    );

    NarrativeMaterial {
        recent_summaries,
        last_active_at,
    }
}

/// 将脉络加权检索的 `SearchResult` 转换为 `MemoryL1`（供脉络行格式化）。
///
/// 说明:
/// - 仅接受 L1 层结果（`DocId::L1`）；其他层（L2/图谱）不是脉络注入目标。
/// - `time_period` / `atmosphere` 在 `SearchResult` 中不承载，置 None——
///   脉络行退化为纯摘要格式（加权路径优先保证话题相关性，展示次要）。
/// - `session_id` 置 nil：脉络行只用于上下文文本展示，不参与会话归属。
pub(super) fn search_result_to_memory_l1(sr: &SearchResult) -> Option<MemoryL1> {
    let id = match &sr.doc_id {
        DocId::L1(id) => *id,
        _ => return None,
    };
    Some(MemoryL1 {
        id,
        session_id: uuid::Uuid::nil(),
        summary: sr.doc_summary.clone(),
        keywords: None,
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: sr.created_at,
        last_accessed_at: sr.last_accessed_at,
        persona_uid: sr.persona_uid.clone(),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    })
}

/// 把一条 L1 摘要格式化为 prompt 脉络行（含时间/氛围标注与 120 字符截断）。
///
/// 格式:
/// - 含时间段与氛围: "上午 — 讨论了Python异步编程的线程安全问题。氛围融洽。"
/// - 仅时间段: "上午 — 讨论了Python异步编程的线程安全问题。"
/// - 仅氛围: "讨论了Python异步编程的线程安全问题。氛围融洽。"
/// - 均缺失: 纯摘要文本。
///
/// 截断规则:
/// - 单条摘要最多 120 字符，超出加省略号。
///
/// 安全约束:
/// - 仅返回展示文本，不写日志。
pub fn format_l1_as_context_line(l1: &MemoryL1) -> String {
    let time_label = l1.time_period.as_deref().unwrap_or("");
    let atmosphere = l1.atmosphere.as_deref().unwrap_or("");

    let base = if !time_label.is_empty() && !atmosphere.is_empty() {
        format!("{time_label} — {}。氛围{atmosphere}。", l1.summary)
    } else if !time_label.is_empty() {
        format!("{time_label} — {}", l1.summary)
    } else if !atmosphere.is_empty() {
        format!("{}。氛围{atmosphere}。", l1.summary)
    } else {
        l1.summary.clone()
    };

    // 截断到 120 字符（统一字符边界工具，预算内含省略号）
    ramaria_core::text::truncate_chars(&base, 120)
}
