//! crates/ramaria-service/src/index/view.rs - Ramaria 检索文档视图转换
//!
//! 设计特点:
//! - 把核心记忆类型映射为检索器文档视图，字段一一对应
//! - 纯函数无 I/O；L1 时间口径取发生时间优先、缺失回落写入时间

use ramaria_core::types::MemoryL1;
use ramaria_memory::retriever::{L1DocView, L2DocView};

// =========================================================
// 视图转换
// =========================================================

/// `MemoryL1` → 检索视图。
pub(super) fn l1_view(l1: &MemoryL1) -> L1DocView {
    L1DocView {
        id: l1.id,
        summary: l1.summary.clone(),
        keywords: l1.keywords.clone(),
        persona_uid: l1.persona_uid.clone(),
        created_at: l1.created_at,
        occurred_at: l1.occurred_at_or_created(),
        salience: l1.salience,
        last_accessed_at: l1.last_accessed_at,
    }
}

/// `MemoryEvent` → 检索视图。
pub(super) fn l2_view(event: &ramaria_core::types::MemoryEvent) -> L2DocView {
    L2DocView {
        id: event.id,
        title: event.title.clone(),
        summary: event.summary.clone(),
        keywords: event.keywords.clone(),
        attitude: event.attitude.clone(),
        paraphrase: event.paraphrase.clone(),
        persona_uid: event.persona_uid.clone(),
        share: event.share,
        confidence: event.confidence,
        created_at: event.created_at,
        start: event.start,
        salience: event.salience,
    }
}
