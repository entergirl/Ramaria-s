//! crates/ramaria-memory/src/similarity.rs — 相似度统一实现（v1.5 收敛）
//!
//! 背景:
//! - v1.5 审查批 2 确认跨文件存在重复的余弦相似度 / Jaccard 相似度实现，
//!   决策已批准收敛到本模块，作为全 crate 唯一实现。
//! - 收敛来源:
//!   - 余弦相似度: `vector.rs`（私有）、`inference/clustering.rs`（pub）、
//!     `event/batcher/mod.rs`（pub）、`behavior/clustering.rs`（pub `cosine_clipped`）。
//!   - Jaccard 相似度: `event/batcher/buffer.rs`、`event/batcher/mod.rs`、
//!     `behavior/clustering.rs`、`event/extractor.rs`、`inference/stats.rs`。
//! - 原模块保留薄包装（保持公开 API 与调用点签名不变），实现体统一在此。
//!
//! 语义统一决策（v1.5）:
//! - 余弦: 长度不一致 → `tracing::warn` + 返回 0.0（原 debug_assert/静默 0 统一为 warn）；
//!   空向量或零范数（< 1e-12）→ 0.0；结果 clamp 到 [-1.0, 1.0]。
//!   调用点若需要 [0, 1] 语义（如 vector 检索、routing/incremental 的 cos 项），
//!   由调用方自行 `.max(0.0)`，不在本模块内做（原 `cosine_clipped` 本就返回 [-1,1]）。
//! - Jaccard: 基于 `HashSet` 去重（重复元素不影响结果）；任一侧为空（含两侧皆空）
//!   → 0.0（信息不足不判相似）；返回 [0.0, 1.0]。

use std::collections::HashSet;

// =========================================================
// 余弦相似度
// =========================================================

/// 计算两个向量的余弦相似度。
///
/// 公式: `cos(θ) = (A·B) / (||A|| × ||B||)`
///
/// 语义（v1.5 统一）:
/// - 长度不一致 → 记录 `tracing::warn` 并返回 0.0（不再 debug_assert panic）。
/// - 任一向量为空或范数 < 1e-12（零向量，无方向）→ 返回 0.0。
/// - 结果钳制到 [-1.0, 1.0]（防御浮点误差）。
///
/// 说明:
/// - 全 crate 唯一余弦相似度实现；各模块原实现已收敛到此，
///   公开函数（`inference::clustering::cosine_similarity`、
///   `event::batcher::cosine_similarity`、`behavior::clustering::cosine_clipped`）
///   保留为薄包装以维持调用点签名。
/// - 调用点如需 [0, 1] 语义，请自行 `.max(0.0)`。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        tracing::warn!(
            a_len = a.len(),
            b_len = b.len(),
            "余弦相似度: 向量长度不一致，返回 0.0"
        );
        return 0.0;
    }

    if a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for i in 0..a.len() {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    if norm_a < 1e-12 || norm_b < 1e-12 {
        return 0.0;
    }

    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

// =========================================================
// Jaccard 相似度
// =========================================================

/// 计算两个集合的 Jaccard 相似度。
///
/// 公式: `J(A, B) = |A ∩ B| / |A ∪ B|`
///
/// 语义（v1.5 统一）:
/// - 基于 `HashSet` 去重：重复元素不影响结果（与 graph.rs 测试
///   `build_graph_repeated_keywords_dont_affect_jaccard` 的文档意图一致）。
/// - 任一侧为空（含两侧皆空）→ 返回 0.0（信息不足不判相似）。
/// - 返回 [0.0, 1.0]。
///
/// 说明:
/// - 全 crate 唯一 Jaccard 实现；各模块原实现（`buffer::jaccard_keyword_sets`、
///   `batcher::jaccard_similarity`、`behavior::clustering::jaccard`、
///   `extractor::keyword_jaccard`、`stats::keyword_jaccard`）均已收敛到此。
pub fn jaccard_similarity<T>(a: impl IntoIterator<Item = T>, b: impl IntoIterator<Item = T>) -> f64
where
    T: Eq + std::hash::Hash,
{
    let set_a: HashSet<T> = a.into_iter().collect();
    let set_b: HashSet<T> = b.into_iter().collect();

    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    intersection as f64 / union as f64
}

// =========================================================
// 单元测试（v1.5 收敛：各模块仅测被删相似度函数的用例迁移至此）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- cosine_similarity ----

    /// 表驱动：常规 / 正交 / 反向 / 零向量 / 长度不一致 / 空集。
    #[test]
    fn cosine_cases() {
        let cases: Vec<(Vec<f32>, Vec<f32>, f64)> = vec![
            // 常规（相同向量）
            (vec![1.0, 0.0, 0.0], vec![1.0, 0.0, 0.0], 1.0),
            (vec![1.0, 2.0, 3.0], vec![1.0, 2.0, 3.0], 1.0),
            // 正交
            (vec![1.0, 0.0], vec![0.0, 1.0], 0.0),
            // 反向（保留负值，[-1,1] clamp 语义）
            (vec![1.0, 0.0], vec![-1.0, 0.0], -1.0),
            // 零向量
            (vec![0.0, 0.0], vec![1.0, 0.0], 0.0),
            (vec![0.0, 0.0], vec![1.0, 1.0], 0.0),
            // 长度不一致 → 0.0
            (vec![1.0, 0.0], vec![1.0, 0.0, 0.0], 0.0),
            // 空集
            (vec![], vec![], 0.0),
        ];
        for (a, b, expected) in cases {
            assert!(
                (cosine_similarity(&a, &b) - expected).abs() < 1e-9,
                "期望 {expected}，实际 {}",
                cosine_similarity(&a, &b)
            );
        }
    }

    /// 结果恒落在 [-1.0, 1.0]（clamp 防御浮点误差）。
    #[test]
    fn cosine_clamped_to_unit_range() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0];
        let s = cosine_similarity(&a, &b);
        assert!((-1.0..=1.0).contains(&s), "越界: {s}");
    }

    // ---- jaccard_similarity ----

    /// 表驱动：完全一致 / 部分重叠 / 无重叠 / 任一侧为空 / 两侧皆空 / 重复元素去重。
    #[test]
    fn jaccard_cases() {
        // 完全一致 → 1.0
        assert!((jaccard_similarity(vec![1, 2], vec![1, 2]) - 1.0).abs() < 1e-9);
        // 部分重叠: 交集={1}，并集={1,2,3} → 1/3
        assert!((jaccard_similarity(vec![1, 2], vec![2, 3]) - 1.0 / 3.0).abs() < 1e-9);
        // 无重叠 → 0.0
        assert!((jaccard_similarity(vec![1], vec![9]) - 0.0).abs() < 1e-9);
        // 两侧皆空 → 0.0
        assert_eq!(
            jaccard_similarity(Vec::<i32>::new(), Vec::<i32>::new()),
            0.0
        );
        // 任一侧为空 → 0.0
        assert_eq!(jaccard_similarity(vec![1], Vec::<i32>::new()), 0.0);
        assert_eq!(jaccard_similarity(Vec::<i32>::new(), vec![1]), 0.0);
        // 重复元素去重：{a,a,b} vs {a,c} → {a,b} ∩ {a,c} = {a}，并集 {a,b,c} → 1/3
        assert!((jaccard_similarity(vec!["a", "a", "b"], vec!["a", "c"]) - 1.0 / 3.0).abs() < 1e-9);
        // String 集合（behavior::clustering::jaccard 的等价场景）
        let s1 = vec!["a".to_string(), "b".to_string()];
        let s2 = vec!["b".to_string(), "c".to_string()];
        assert!((jaccard_similarity(&s1, &s2) - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(
            jaccard_similarity(&Vec::<String>::new(), &Vec::<String>::new()),
            0.0
        );
    }
}
