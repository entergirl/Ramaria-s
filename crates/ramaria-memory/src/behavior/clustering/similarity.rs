//! crates/ramaria-memory/src/behavior/clustering/similarity.rs - 三路融合相似度
//!
//! 设计特点:
//! - sim = β1·cos(r_i,r_j) + β2·cos(s_i,s_j) + (1−β1−β2)·Jaccard(K_i,K_j)。
//! - 缺通道时对应权重归零并重新归一化其余项（embedding 全缺 → 纯 Jaccard）。
//! - 余弦计算前 clip 到 [-1,1] 防御浮点误差，零向量余弦按 0 处理。
//! - Jaccard 与余弦统一收敛到 `crate::similarity`，本模块为薄包装。

use super::sample::BehaviorSample;

// =========================================================
// 三路融合相似度
// =========================================================

/// 计算两样本的三路融合相似度。
///
/// 公式: sim = β1·cos(r_i,r_j) + β2·cos(s_i,s_j) + (1−β1−β2)·Jaccard(K_i,K_j)
///
/// 降级说明:
/// - 双方都有反应向量才计入 β1 项，否则该项权重归零并重新归一化其余项；
///   情境通道同理。embedding 全部不可用 → sim = Jaccard（β 通道权重为 0）。
/// - cos 计算前 clip 到 [-1,1]（浮点误差防御），零向量余弦按 0 处理。
///
/// 参数:
/// - `beta1` + `beta2`: 双通道权重，约束 β1 + β2 ≤ 1（关键词权重 = 1 − β1 − β2）。
///
/// 返回:
/// - 相似度 0.0..1.0（Jaccard 与 clip 后余弦均非负，加权和保持非负）。
pub fn fused_similarity(a: &BehaviorSample, b: &BehaviorSample, beta1: f64, beta2: f64) -> f64 {
    let beta3 = (1.0 - beta1 - beta2).max(0.0);
    let r_ok = a.reaction_vector.is_some() && b.reaction_vector.is_some();
    let s_ok = a.situation_vector.is_some() && b.situation_vector.is_some();

    let mut weight_sum = beta3;
    let mut acc = beta3 * jaccard(&a.situation_keywords, &b.situation_keywords);

    if r_ok {
        let cos_r = cosine_clipped(
            a.reaction_vector.as_deref().unwrap_or_default(),
            b.reaction_vector.as_deref().unwrap_or_default(),
        );
        weight_sum += beta1;
        acc += beta1 * cos_r;
    }
    if s_ok {
        let cos_s = cosine_clipped(
            a.situation_vector.as_deref().unwrap_or_default(),
            b.situation_vector.as_deref().unwrap_or_default(),
        );
        weight_sum += beta2;
        acc += beta2 * cos_s;
    }

    if weight_sum <= 0.0 {
        return 0.0;
    }
    (acc / weight_sum).clamp(0.0, 1.0)
}

/// 两集合的 Jaccard 相似度（空集 → 0.0）。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`，本函数为薄包装。
pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    crate::similarity::jaccard_similarity(
        a.iter().map(String::as_str),
        b.iter().map(String::as_str),
    )
}

/// 余弦相似度（零向量 → 0.0；结果 clip 到 [-1,1] 防御浮点误差）。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::cosine_similarity`，本函数为薄包装。
/// - 统一实现同样 clamp 到 [-1,1]；调用点如需 [0,1] 语义请自行 `.max(0.0)`
///   （`routing::score_rule` 与 `incremental` 模块已如此处理）。
pub fn cosine_clipped(a: &[f32], b: &[f32]) -> f64 {
    crate::similarity::cosine_similarity(a, b)
}
