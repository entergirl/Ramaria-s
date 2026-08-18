//! crates/ramaria-memory/src/fact/dedup.rs - 知识层判重
//!
//! 设计特点:
//! - 判重双条件: 同 field 语义余弦 ≥ 0.85 **且** 关键词交集 ≥ 1 → 判重复不入库
//! - 仅单条件命中（仅余弦 / 仅关键词交集）均不判定重复（避免误杀新事实）
//! - 依赖 `crate::similarity`（全 crate 唯一余弦/Jaccard 实现）
//! - 对库内已有 active 事实逐一比对，命中任一即判重复
//!
//! 参数:
//! - embedding: 可选语义向量计算器（None = 无向量语义，仅依赖关键词交集兜底）

use crate::similarity::{cosine_similarity, jaccard_similarity};
use ramaria_core::types::PersonaFact;

/// 判重语义余弦阈值（双条件之一；与关键词交集同时满足才判重复）。
pub const DEDUP_COSINE_THRESHOLD: f64 = 0.85;

/// 判重输入。
#[derive(Debug, Clone)]
pub struct DedupInput {
    /// 库内同 field 的 active 候选事实（逐一比对）。
    pub existing: Vec<PersonaFact>,
    /// 新事实关键词（逗号分隔解析成集合）。
    pub new_keywords: Vec<String>,
    /// 新事实内容语义向量（None = 无法计算语义余弦）。
    pub new_vector: Option<Vec<f32>>,
}

/// 判重结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupVerdict {
    /// 与库内某条 active 事实重复，不入库
    Duplicate,
    /// 唯一，可入库
    Unique,
}

/// 关键词交集检查：两侧关键词集合是否有 ≥1 个共同词。
///
/// 说明:
/// - 通过 Jaccard > 0 判定集合有交集（等价于交集非空）。
/// - 任一侧为空返回 false（信息不足不判重复）。
fn keywords_overlap(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    jaccard_similarity(a.iter().map(|s| s.as_str()), b.iter().map(|s| s.as_str())) > 0.0
}

/// 提取 PersonaFact 的 keyword_hint 为关键词集合（逗号分隔解析，去空）。
fn fact_keywords(fact: &PersonaFact) -> Vec<String> {
    fact.keyword_hint
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 判断新内容是否与库内已有 active 事实重复。
///
/// 参数:
/// - `input`: 判重输入（库内候选、新关键词、新向量）。
/// - `existing_text_vector`: 每条库内事实的内容向量（与 new_vector 长度一致的预计算值）。
///
/// 说明:
/// - `existing_text_vector` 用于逐条计算与新的同 field 语义余弦；若 None（embedding 不可用），
///   仅依赖关键词交集判定（单条件不判重复，故降级为不判重复——保守不误杀）。
///
/// 返回:
/// - `DedupVerdict`。
pub fn check_dedup(input: &DedupInput, existing_text_vector: &[Option<Vec<f32>>]) -> DedupVerdict {
    for (i, fact) in input.existing.iter().enumerate() {
        let kw_overlap = keywords_overlap(&fact_keywords(fact), &input.new_keywords);
        // 双条件同时满足 → 判重复；仅单条件命中不判（防误杀新事实）
        match existing_text_vector.get(i).and_then(|v| v.as_ref()) {
            Some(ext_vec) => {
                let semantic = input
                    .new_vector
                    .as_ref()
                    .map(|nv| cosine_similarity(nv, ext_vec))
                    .unwrap_or(0.0);
                // 双条件同时满足 → 判重复
                if semantic >= DEDUP_COSINE_THRESHOLD && kw_overlap {
                    return DedupVerdict::Duplicate;
                }
            }
            None => {
                // embedding 不可用或该条事实无向量：不作语义判定（单条件不重复）
                continue;
            }
        }
    }
    DedupVerdict::Unique
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{FactSource, FactStatus, FactTier, ProfileField};

    fn make_fact(content: &str, keywords: &str) -> PersonaFact {
        let mut f = PersonaFact::new(
            "char-0001".into(),
            ProfileField::Interests,
            content.into(),
            FactSource::Event,
        );
        f.status = FactStatus::Active;
        f.tier = FactTier::Stable;
        f.keyword_hint = Some(keywords.to_string());
        f
    }

    /// 双条件同时命中（语义 ≥0.85 且关键词交集 ≥1）→ 判重复。
    #[test]
    fn both_conditions_dup() {
        let existing = vec![make_fact("喜欢阅读科幻小说", "阅读,科幻")];
        let input = DedupInput {
            existing,
            new_keywords: vec!["阅读".into(), "科幻".into()],
            new_vector: Some(vec![1.0, 0.0, 0.0]),
        };
        let existing_vec = vec![Some(vec![0.99, 0.0, 0.0])]; // 语义 ≥0.85
        assert_eq!(check_dedup(&input, &existing_vec), DedupVerdict::Duplicate);
    }

    /// 仅语义命中（无关键词交集）→ 不判重复。
    #[test]
    fn only_semantic_dup_is_unique() {
        let existing = vec![make_fact("喜欢阅读科幻小说", "阅读,科幻")];
        let input = DedupInput {
            existing,
            new_keywords: vec!["电影".into()], // 与库内关键词无交集
            new_vector: Some(vec![1.0, 0.0]),
        };
        let existing_vec = vec![Some(vec![1.0, 0.0])]; // 语义 1.0 ≥0.85
        assert_eq!(check_dedup(&input, &existing_vec), DedupVerdict::Unique);
    }

    /// 仅关键词交集（无语义向量）→ 不判重复。
    #[test]
    fn only_keyword_overlap_is_unique() {
        let existing = vec![make_fact("喜欢阅读", "阅读")];
        let input = DedupInput {
            existing,
            new_keywords: vec!["阅读".into()],
            new_vector: None,
        };
        let existing_vec = vec![None]; // 无向量
        // 双条件需同时满足：语义缺失 → 不判重复（保守不误杀）
        assert_eq!(check_dedup(&input, &existing_vec), DedupVerdict::Unique);
    }

    /// 语义低于阈值即使关键词重叠 → 不判重复。
    #[test]
    fn semantic_below_threshold_is_unique() {
        let existing = vec![make_fact("喜欢阅读科幻小说", "阅读,科幻")];
        let input = DedupInput {
            existing,
            new_keywords: vec!["阅读".into()],
            new_vector: Some(vec![1.0, 0.0]),
        };
        let existing_vec = vec![Some(vec![0.0, 1.0])]; // 语义 cos = 0 < 0.85
        assert_eq!(check_dedup(&input, &existing_vec), DedupVerdict::Unique);
    }

    /// 关键词一致但语义阈值边界（=0.85）→ 判重复（边界含）。
    #[test]
    fn semantic_at_threshold_with_overlap_is_dup() {
        let existing = vec![make_fact("喜欢阅读科幻小说", "阅读,科幻")];
        let input = DedupInput {
            existing,
            new_keywords: vec!["阅读".into()],
            new_vector: Some(vec![1.0, 0.0]),
        };
        let existing_vec = vec![Some(vec![0.85, 0.0])]; // 语义恰 0.85
        assert_eq!(check_dedup(&input, &existing_vec), DedupVerdict::Duplicate);
    }

    /// 无库内候选 → 必唯一。
    #[test]
    fn no_existing_is_unique() {
        let input = DedupInput {
            existing: vec![],
            new_keywords: vec!["书".into()],
            new_vector: Some(vec![1.0]),
        };
        assert_eq!(check_dedup(&input, &[]), DedupVerdict::Unique);
    }
}
