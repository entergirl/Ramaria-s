//! crates/ramaria-memory/src/inference/stats/tests/mod.rs - Ramaria 统计特征提取全模块单元测试
//!
//! 设计特点:
//! - 由 stats/mod.rs 以 #[cfg(test)] mod tests; 统一收纳，覆盖各子模块统计逻辑。
//! - 经 use super::* 取用 stats 根 re-export 的公共 API。
//! - are_different_batches / normalize_group_weights 为 pub(super)，测试套件经显式路径直接调用。
//! - 构造事件辅助统一复用 make_event / make_event_with_situation / make_event_with_time。
//!
//! 安全约束:
//! - 测试仅用合成 MemoryEvent，不依赖真实 LLM/embedding，可离线确定性运行。

use super::admission::are_different_batches;
use super::run::normalize_group_weights;
use super::*;
use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

/// 构造测试用 MemoryEvent。
#[allow(clippy::too_many_arguments)]
fn make_event(
    title: &str,
    summary: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    valence: f64,
    share: f64,
    presentation: Presentation,
    attitude: Option<&str>,
) -> MemoryEvent {
    let now = now_ms();
    let mut ev = MemoryEvent::new(
        "user-0001".into(),
        title.into(),
        summary.into(),
        now - 1000,
        now,
    );
    ev.keywords = keywords.map(|s| s.into());
    ev.confidence = confidence;
    ev.salience = salience;
    ev.valence = valence;
    ev.share = share;
    ev.presentation = presentation;
    ev.attitude = attitude.map(|s| s.into());
    ev
}

/// 构造测试用 MemoryEvent（含 situation_strength）。
#[allow(clippy::too_many_arguments)]
fn make_event_with_situation(
    title: &str,
    summary: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    valence: f64,
    share: f64,
    presentation: Presentation,
    attitude: Option<&str>,
    situation_strength: Option<i32>,
) -> MemoryEvent {
    let mut ev = make_event(
        title,
        summary,
        keywords,
        confidence,
        salience,
        valence,
        share,
        presentation,
        attitude,
    );
    ev.situation_strength = situation_strength;
    ev
}

mod admission;
mod batch;
mod calibrate;
mod category;
mod category_stats;
mod classify;
mod config;
mod cross;
mod enrichment;
mod group;
mod metrics;
mod motive;
mod prefilter;
mod representative;
mod run;
mod situation;
mod weighted;
