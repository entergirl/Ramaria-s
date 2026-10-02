//! crates/ramaria-memory/src/behavior/clustering/sample.rs - 聚类输入样本构造
//!
//! 设计特点:
//! - BehaviorSample 为情境-反应对，向量字段留空由 `vectorize` 填充。
//! - 情境关键词不含 valence，避免情绪信号污染情境判定。
//! - 关键词去重保序并小写化，空词剔除。

use ramaria_core::types::{MemoryEvent, Presentation};

// =========================================================
// 聚类输入样本
// =========================================================

/// 单条事件的聚类样本（情境-反应对，v3.1 §4.2 Step 1）。
///
/// 字段约定:
/// - `situation_keywords`: 情境侧关键词集（不含 valence，避免情绪信号污染情境判定）。
/// - `situation_vector`: 情境通道向量（embedding 不可用时为 None → 降级关键词通道）。
/// - `reaction_vector`: 反应通道向量（embedding 不可用时为 None）。
/// - `valence` / `presentation` / `salience`: 反应侧特征（簇提炼与参数化输入）。
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorSample {
    /// 事件 id（证据引用）
    pub event_id: i64,
    /// 情境侧关键词集（去重小写）
    pub situation_keywords: Vec<String>,
    /// 情境通道向量 s_i
    pub situation_vector: Option<Vec<f32>>,
    /// 反应通道向量 r_i
    pub reaction_vector: Option<Vec<f32>>,
    /// 情绪效价 -1.0..1.0
    pub valence: f64,
    /// 陈述方式
    pub presentation: Presentation,
    /// 显著性权重（salience 加权证据量）
    pub salience: f64,
    /// 情境强度 1-5（None 等效 3）
    pub situation_strength: Option<i32>,
    /// 事件开始时间（Unix 毫秒）
    pub start_ms: i64,
}

/// 从 `MemoryEvent` 构造样本（向量留空，由 `vectorize` 填充）。
///
/// 参数:
/// - `event`: L2 事件。
///
/// 说明:
/// - 关键词取 `keywords` 逗号分隔拆分（保留原词，去重去空）。
/// - 反应通道文本 = paraphrase ⊕ attitude（优先 paraphrase，缺失回退 attitude；
///   两者皆缺则该事件不参与反应通道向量化，但仍可参与情境通道与关键词聚类）。
pub fn sample_from_event(event: &MemoryEvent) -> BehaviorSample {
    let keywords: Vec<String> = event
        .keywords
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect::<Vec<_>>();

    BehaviorSample {
        event_id: event.id,
        situation_keywords: dedup_keywords(&keywords),
        situation_vector: None,
        reaction_vector: None,
        valence: event.valence.clamp(-1.0, 1.0),
        presentation: event.presentation,
        salience: event.salience.clamp(0.0, 1.0),
        situation_strength: event.situation_strength,
        start_ms: event.start,
    }
}

/// 关键词去重（保序、小写化）。
pub fn dedup_keywords(raw: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.iter()
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty())
        .filter(|k| seen.insert(k.clone()))
        .collect()
}
