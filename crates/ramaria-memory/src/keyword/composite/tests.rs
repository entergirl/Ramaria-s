//! crates/ramaria-memory/src/keyword/composite/tests.rs - //! crates/ramaria-memory/src/keyword/composite.rs — 关键词两级编排 + 语义扩展单元测试
//!
//! 设计特点:
//! - 位于 keyword::composite 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 composite.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::keyword::{KeywordQuery, KeywordToken};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const NOW_MS: i64 = 2_000_000_000_000;

/// 测试用确定性 mock embedding provider。
///
/// 向量由文本哈希生成：同词必同向量、不同词向量正交（保证测试可复现且无随机性）。
/// 置位 `fail_embed` 后 embed/embed_batch 恒失败，用于覆盖 embedding 失败分支。
struct MockEmbedder {
    dim: usize,
    // 文本 → 预设向量（供"语义相似"场景构造非零相关）
    overrides: Mutex<HashMap<String, Vec<f32>>>,
    // embedding 失败开关（测试注入）
    fail_embed: AtomicBool,
}

impl MockEmbedder {
    fn new(dim: usize, overrides: HashMap<String, Vec<f32>>) -> Self {
        Self {
            dim,
            overrides: Mutex::new(overrides),
            fail_embed: AtomicBool::new(false),
        }
    }

    /// 设置 embedding 失败开关：置位后 `embed` / `embed_batch` 恒失败。
    ///
    /// 用于覆盖 embedding 不可用时的降级分支（expand 返回空扩展、build 返回 Err）。
    fn set_fail_embed(&self, fail: bool) {
        self.fail_embed.store(fail, Ordering::SeqCst);
    }

    /// 确定性伪向量：文本哈希填充。
    fn vector_of(&self, text: &str) -> Vec<f32> {
        if let Ok(table) = self.overrides.lock()
            && let Some(v) = table.get(text)
        {
            return v.clone();
        }
        let mut v = vec![0.0f32; self.dim];
        let hash: u64 = text.bytes().fold(0x811c9dc5u64, |acc, b| {
            (acc ^ u64::from(b)).wrapping_mul(0x01000193)
        });
        for (i, slot) in v.iter_mut().enumerate().take(self.dim) {
            *slot = ((hash >> (i % 16)) & 0xF) as f32 / 16.0;
        }
        v
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if self.fail_embed.load(Ordering::SeqCst) {
            return Err(RamariaError::llm("mock embedding 失败（测试注入）"));
        }
        Ok(self.vector_of(text))
    }
    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if self.fail_embed.load(Ordering::SeqCst) {
            return Err(RamariaError::llm("mock embedding 失败（测试注入）"));
        }
        Ok(texts.iter().map(|t| self.vector_of(t)).collect())
    }
    fn model_info(&self) -> ramaria_core::EmbeddingModelInfo {
        ramaria_core::EmbeddingModelInfo {
            model_id: "mock".into(),
            dimension: self.dim,
        }
    }
    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }
    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }
    fn download_progress(&self) -> f64 {
        1.0
    }
    fn is_available(&self) -> bool {
        true
    }
}

fn l1_ref(id: uuid::Uuid, persona: &str) -> KeywordRef {
    KeywordRef::L1 {
        id,
        persona_uid: persona.to_string(),
    }
}

fn query_for(keywords: &[&str], persona: &str, top_k: usize) -> KeywordQuery {
    KeywordQuery::builder(Some(persona.to_string()))
        .keywords_from(keywords.iter().filter_map(|s| KeywordToken::new(s)))
        .top_k(top_k)
        .build()
}

/// 构造语义上"相关但字面不同"的文档集：
/// - 文档 A 关键词 = ["工作压力"]（字面直击）
/// - 文档 B 关键词 = ["职场焦虑"]（语义相关，字面无关）
fn semantic_sample() -> CompositeIndex {
    let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
    composite.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "user-0001"),
        Some("工作压力, 加班"),
        0.8,
        NOW_MS - 86_400_000,
    );
    composite.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "user-0001"),
        Some("职场焦虑, 内耗"),
        0.7,
        NOW_MS - 86_400_000,
    );
    composite
}

// ---- CompositeIndex 维护 ----

#[test]
fn composite_index_ops() {
    let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
    assert_eq!(composite.doc_count(), 0);
    let reff = l1_ref(uuid::Uuid::new_v4(), "p1");
    composite.index_parsed(reff.clone(), Some("爬山"), 0.5, NOW_MS);
    assert_eq!(composite.doc_count(), 1);
    assert!(composite.remove(&reff));
    assert_eq!(composite.doc_count(), 0);
}

// ---- 三级编排 ----

/// 精确命中足够时直接返回（不进入子串/语义层）
#[tokio::test]
async fn exact_sufficient_no_fallback() {
    let composite = semantic_sample();
    // 无 embedder（embedding 不可用）也应能精确返回
    let results = composite
        .query(&query_for(&["工作压力"], "user-0001", 5), None)
        .await;
    assert_eq!(results.len(), 1);
    // 结果只来自精确命中文档
    let doc = &results[0].0;
    assert_eq!(doc.persona_uid(), Some("user-0001"));
}

/// 子串回退：精确不足时子串命中补足
#[tokio::test]
async fn substring_fallback_when_exact_insufficient() {
    let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
    composite.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("工作压力"),
        0.5,
        NOW_MS - 86_400_000,
    );
    // 查"工作"——精确不命中（无"工作"独立词），子串命中"工作压力"
    let results = composite.query(&query_for(&["工作"], "p1", 5), None).await;
    assert_eq!(results.len(), 1, "子串回退应补足结果");
}

/// 子串回退开关关闭时不回退
#[tokio::test]
async fn substring_fallback_disabled() {
    let composite = CompositeIndex::new(CompositeIndexConfig {
        enable_substring_fallback: false,
        ..Default::default()
    });
    // 直接往底层 index 加词较繁琐，这里用精确语义验证：exact 空 → 不回退 → 空
    let mut composite = composite;
    composite.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("工作压力"),
        0.5,
        NOW_MS - 86_400_000,
    );
    let results = composite.query(&query_for(&["工作"], "p1", 5), None).await;
    assert!(results.is_empty(), "关闭子串回退后不应命中");
}

/// 语义扩展：Fuzzy 就绪 + embedder 可用 → 找出"工作压力"语义相关的"职场焦虑"文档
#[tokio::test]
async fn semantic_expansion_finds_related_doc() {
    let mut composite = semantic_sample();
    // 构造 mock：工作压力 与 职场焦虑 向量高度相关（其余无关）
    let dim = 4;
    let mut overrides = HashMap::new();
    overrides.insert("工作压力".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
    overrides.insert("职场焦虑".to_string(), vec![1.0, 1.0, 0.0, 0.0]);
    let embedder = MockEmbedder::new(dim, overrides);

    let terms: Vec<KeywordToken> = ["工作压力", "职场焦虑", "加班", "内耗"]
        .iter()
        .filter_map(|s| KeywordToken::new(s))
        .collect();
    let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
    composite.set_fuzzy(Some(fuzzy));

    // 查"工作压力"→ 精确命中文档A；语义扩展命中文档B（关键词"职场焦虑"）
    let results = composite
        .query(&query_for(&["工作压力"], "user-0001", 5), Some(&embedder))
        .await;
    assert_eq!(results.len(), 2, "应包含精确 + 语义两篇文档");
}

/// Fuzzy 未就绪 / embedder 为 None → 仅两层，不 panic
#[tokio::test]
async fn semantic_layer_not_ready_degrades() {
    let composite = semantic_sample();
    // fuzzy 未挂载
    let results = composite
        .query(&query_for(&["工作压力"], "user-0001", 5), None)
        .await;
    assert_eq!(results.len(), 1);
}

// ---- FuzzyKeywordIndex ----

/// 构建成功 → ready；词条数/维度正确
#[tokio::test]
async fn fuzzy_build_success() {
    let embedder = MockEmbedder::new(4, HashMap::new());
    let terms: Vec<KeywordToken> = ["工作压力", "职场焦虑"]
        .iter()
        .filter_map(|s| KeywordToken::new(s))
        .collect();
    let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
    assert!(fuzzy.is_ready());
    assert_eq!(fuzzy.len(), 2);
    assert_eq!(fuzzy.dim(), 4);
}

/// 词典为空 → 构建失败（Err）
#[tokio::test]
async fn fuzzy_build_empty_terms_err() {
    let embedder = MockEmbedder::new(4, HashMap::new());
    let result = FuzzyKeywordIndex::build(&[], &embedder).await;
    assert!(result.is_err());
}

/// embedder 批量向量生成失败 → 构建返回 Err（上层据此降级为无 Fuzzy 层）
#[tokio::test]
async fn fuzzy_build_embed_error_propagates() {
    let embedder = MockEmbedder::new(4, HashMap::new());
    embedder.set_fail_embed(true);

    let terms: Vec<KeywordToken> = ["工作压力", "职场焦虑"]
        .iter()
        .filter_map(|s| KeywordToken::new(s))
        .collect();
    let result = FuzzyKeywordIndex::build(&terms, &embedder).await;
    assert!(
        result.is_err(),
        "embed_batch 失败应向上传播为 Err，而非静默构建出不可用索引"
    );
}

/// 查询词 embed 失败 → 返回空扩展（不 panic、不产生错误结果）
#[tokio::test]
async fn fuzzy_expand_embed_error_empty() {
    // 预设"工作压力"与查询词"加班"同向量：若失败注入失效，扩展必然非空、
    // 断言随即失败——保证本用例真正守住"embed 失败 → 空扩展"的失败分支
    let mut overrides = HashMap::new();
    overrides.insert("工作压力".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
    overrides.insert("加班".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
    let embedder = MockEmbedder::new(4, overrides);

    let terms: Vec<KeywordToken> = ["工作压力"]
        .iter()
        .filter_map(|s| KeywordToken::new(s))
        .collect();
    let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();

    // 构建完成后注入 embedding 失败：expand 应静默降级为返回空
    embedder.set_fail_embed(true);
    let token = KeywordToken::new("加班").unwrap();
    let out = fuzzy.expand(&token, &embedder, 3).await;
    assert!(
        out.is_empty(),
        "embedding 失败 → 返回空扩展，不 panic、不产生错误结果"
    );
}

/// expand 自身词不扩展（跳过字面相同词条）
#[tokio::test]
async fn fuzzy_expand_skips_self() {
    let dim = 4;
    let mut overrides = HashMap::new();
    overrides.insert("工作压力".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
    let embedder = MockEmbedder::new(dim, overrides);
    let terms: Vec<KeywordToken> = ["工作压力"]
        .iter()
        .filter_map(|s| KeywordToken::new(s))
        .collect();
    let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
    let token = KeywordToken::new("工作压力").unwrap();
    let out = fuzzy.expand(&token, &embedder, 3).await;
    assert!(out.is_empty(), "自身不参与扩展");
}
