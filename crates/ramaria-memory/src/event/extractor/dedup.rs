//! crates/ramaria-memory/src/event/extractor/dedup.rs - L2 聚类去重指纹与相似度辅助
//!
//! 设计特点:
//! - 集合指纹: `sha256(按 L1 id 升序拼接的 id 列表)`，顺序无关、集合敏感、不落原文。
//! - 事件去重: 标题+摘要字符 bigram Jaccard 与关键词集合 Jaccard 取较大值判重。
//! - 全部为纯函数，无存储/LLM 依赖，便于确定性单测。
//! - 对外入口（`compute_l1_set_fingerprint` / `event_text_similarity`）以 `pub(super)` 暴露给父模块。

use ramaria_core::{MemoryEvent, MemoryL1};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

// =========================================================
// L2 聚类去重指纹辅助函数（v1.5 三层生成缓存 C）
// =========================================================

/// 计算 L1 集合指纹：`sha256(按 L1 id 升序拼接的 id 列表)` 的 hex 摘要。
///
/// 性质:
/// - **顺序无关**：先排序再拼接，同一集合无论 L1 读取顺序如何均得同指纹。
/// - **集合敏感性**：新增/移除任一 L1 → 指纹必然变化 → 自动触发重新聚类
///   （同集合跳过、集合变更重聚类的核心）。
/// - **不落原文**：仅由 L1 id（UUID）推导，不含对话原文（隐私红线）。
pub(super) fn compute_l1_set_fingerprint(l1_list: &[MemoryL1]) -> String {
    let mut ids: Vec<String> = l1_list.iter().map(|l| l.id.to_string()).collect();
    ids.sort();
    let joined = ids.join("|");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    hex_digest(&hasher.finalize())
}

/// 将 SHA-256 摘要编码为 64 字符小写 hex。
fn hex_digest(digest: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 计算两个事件的去重相似度（0.0..=1.0）。
///
/// 取「标题+摘要字符 bigram Jaccard」与「关键词集合 Jaccard」的**较大值**:
/// - 重跑/失败恢复场景: LLM 重新生成的标题/摘要措辞可能略有差异，
///   但关键词提取通常一致 → 关键词通道命中（高相似 → 判重）；
/// - 关键词缺失或两侧关键词不同的边缘场景 → 文本 bigram 通道兜底。
pub(super) fn event_text_similarity(a: &MemoryEvent, b: &MemoryEvent) -> f64 {
    let bigram = char_bigram_jaccard(
        &format!("{} {}", a.title, a.summary),
        &format!("{} {}", b.title, b.summary),
    );
    let kw = keyword_jaccard(a.keywords.as_deref(), b.keywords.as_deref());
    bigram.max(kw)
}

/// 字符 bigram Jaccard 相似度。
///
/// 归一化（小写 + 仅保留字母数字/空白）后按 Unicode 字符取相邻二元组，
/// 对中英文混合文本均有效；两文本均无 bigram 时返回 0.0（不误判为相似）。
fn char_bigram_jaccard(text_a: &str, text_b: &str) -> f64 {
    let set_a = char_bigrams(&normalize_for_dedup(text_a));
    let set_b = char_bigrams(&normalize_for_dedup(text_b));
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }
    let inter = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    inter as f64 / union as f64
}

/// 归一化去重文本：小写化并仅保留字母数字与空白（去除标点干扰）。
fn normalize_for_dedup(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 取文本的 Unicode 字符二元组集合（相邻两字符为一组）。
fn char_bigrams(normalized: &str) -> HashSet<String> {
    let chars: Vec<char> = normalized.chars().collect();
    chars
        .windows(2)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

/// 关键词集合 Jaccard 相似度（关键词为逗号分隔字符串）。
///
/// 任一侧关键词缺失/为空时返回 0.0（信息不足不判重）。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`。
fn keyword_jaccard(kw_a: Option<&str>, kw_b: Option<&str>) -> f64 {
    crate::similarity::jaccard_similarity(split_keywords(kw_a), split_keywords(kw_b))
}

/// 将逗号分隔的关键词字符串解析为去空集合。
fn split_keywords(kw: Option<&str>) -> HashSet<String> {
    kw.map(|s| {
        s.split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    })
    .unwrap_or_default()
}
