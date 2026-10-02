//! crates/ramaria-memory/src/keyword/service/tests.rs - //! crates/ramaria-memory/src/keyword/service.rs - 关键词子系统应用层服务单元测试
//!
//! 设计特点:
//! - 位于 keyword::service 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 service.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::error::RamariaResult;
use ramaria_core::keyword::{KeywordQuery, KeywordToken};

const NOW_MS: i64 = 2_000_000_000_000;

fn token(s: &str) -> KeywordToken {
    KeywordToken::new(s).unwrap()
}

fn row(
    rowid: i64,
    keyword: &str,
    status: Option<&str>,
    canonical_id: Option<i64>,
) -> KeywordPoolRow {
    KeywordPoolRow {
        rowid,
        keyword: keyword.to_string(),
        use_count: 1,
        created_at: rowid,
        alias_status: status.map(|s| s.to_string()),
        canonical_id,
        canonical_keyword: None,
    }
}

fn sample_rows() -> Vec<KeywordPoolRow> {
    vec![
        row(1, "工作压力", None, None),
        row(2, "职场焦虑", Some("pending"), Some(1)),
        row(3, "职业倦怠", Some("alias"), Some(1)),
        row(4, "爬山", Some("canonical"), None),
    ]
}

fn l1_view(id: uuid::Uuid, persona: Option<&str>, keywords: Option<&str>) -> L1DocView {
    L1DocView {
        id,
        summary: String::new(),
        keywords: keywords.map(|s| s.to_string()),
        persona_uid: persona.map(|s| s.to_string()),
        created_at: NOW_MS,
        salience: 0.8,
        last_accessed_at: None,
    }
}

fn l2_view(id: i64, persona: &str, keywords: Option<&str>) -> L2DocView {
    L2DocView {
        id,
        title: String::new(),
        summary: String::new(),
        keywords: keywords.map(|s| s.to_string()),
        attitude: None,
        paraphrase: None,
        persona_uid: persona.to_string(),
        share: 0.5,
        confidence: 0.7,
        created_at: NOW_MS,
        salience: 0.6,
    }
}

fn query_for(keywords: &[&str], persona: Option<&str>) -> KeywordQuery {
    KeywordQuery::builder(persona.map(|s| s.to_string()))
        .keywords_from(keywords.iter().filter_map(|s| KeywordToken::new(s)))
        .top_k(20)
        .build()
}

/// 已确认词表（canonical + alias，排除 pending）token 列表。
fn established(svc: &KeywordService) -> Vec<KeywordToken> {
    svc.pool()
        .established_terms()
        .into_iter()
        .cloned()
        .collect()
}

/// 测试用确定性 embedding（可注入失败）。
struct MockEmbedder {
    fail_batch: bool,
}

impl MockEmbedder {
    fn ok() -> Self {
        Self { fail_batch: false }
    }
    fn failing() -> Self {
        Self { fail_batch: true }
    }
    fn vector(text: &str) -> Vec<f32> {
        // 同词同向量（词条间保证可区分：取哈希前 4 维）
        let hash: u64 = text.bytes().fold(0x811c9dc5u64, |acc, b| {
            (acc ^ u64::from(b)).wrapping_mul(0x01000193)
        });
        vec![
            (hash & 0xFF) as f32 / 255.0,
            ((hash >> 8) & 0xFF) as f32 / 255.0,
            ((hash >> 16) & 0xFF) as f32 / 255.0,
            ((hash >> 24) & 0xFF) as f32 / 255.0,
        ]
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        Ok(Self::vector(text))
    }
    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if self.fail_batch {
            return Err(ramaria_core::RamariaError::llm("mock 批量向量化失败"));
        }
        Ok(texts.iter().map(|t| Self::vector(t)).collect())
    }
    fn model_info(&self) -> ramaria_core::EmbeddingModelInfo {
        ramaria_core::EmbeddingModelInfo {
            model_id: "mock-service".into(),
            dimension: 4,
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

// ---- 词典装载与三态映射 ----

#[test]
fn load_pool_entries_maps_three_states() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    assert_eq!(svc.pool_len(), 4);

    // canonical（NULL 与 "canonical" 形态）解析自身
    assert_eq!(
        svc.pool().resolve(&token("工作压力")).unwrap().as_str(),
        "工作压力"
    );
    assert_eq!(svc.pool().resolve(&token("爬山")).unwrap().as_str(), "爬山");
    // pending / alias 解析到规范词
    assert_eq!(
        svc.pool().resolve(&token("职场焦虑")).unwrap().as_str(),
        "工作压力"
    );
    assert_eq!(
        svc.pool().resolve(&token("职业倦怠")).unwrap().as_str(),
        "工作压力"
    );
    // 规范词列表仅 Canonical（排除 pending/alias）
    assert_eq!(svc.pool().list_canonicals().len(), 2);
}

#[test]
fn load_pool_entries_handles_invalid_and_missing_canonical() {
    let mut svc = KeywordService::new();
    let rows = vec![
        row(1, "  ", None, None),                // 无效文本 → 过滤
        row(2, "孤儿别名", Some("alias"), None), // 缺 canonical_id → 兜底 Canonical
    ];
    svc.load_pool_entries(&rows);
    assert_eq!(svc.pool_len(), 1);
    assert!(svc.pool().entry_by_id(2).unwrap().status.is_canonical());
}

#[test]
fn upsert_pool_tokens_accumulates_mirror() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    let before = svc
        .pool()
        .entry_by_token(&token("工作压力"))
        .unwrap()
        .use_count;
    let accumulated = svc.upsert_pool_tokens(&[token("工作压力"), token("新词")], NOW_MS);
    assert_eq!(accumulated, 2, "返回实际参与累积的唯一词条数");
    assert_eq!(
        svc.pool()
            .entry_by_token(&token("工作压力"))
            .unwrap()
            .use_count,
        before + 1
    );
    assert_eq!(
        svc.pool().entry_by_token(&token("新词")).unwrap().use_count,
        1
    );
}

/// 批内幂等：同批重复 token 只累加一次（跨批重放不保证，调用方需按文档幂等）。
#[test]
fn upsert_pool_tokens_dedups_within_batch() {
    let mut svc = KeywordService::new();
    let unique = svc.upsert_pool_tokens(
        &[token("工作压力"), token("新词"), token("工作压力")],
        NOW_MS,
    );
    assert_eq!(unique, 2, "批内重复 token 只计一次");
    assert_eq!(
        svc.pool()
            .entry_by_token(&token("工作压力"))
            .unwrap()
            .use_count,
        1
    );
    assert_eq!(
        svc.pool().entry_by_token(&token("新词")).unwrap().use_count,
        1
    );
}

// ---- 文档装载与增量维护 ----

#[test]
fn reset_docs_from_views_indexes_l1_and_l2() {
    let mut svc = KeywordService::new();
    let id1 = uuid::Uuid::new_v4();
    let id2 = uuid::Uuid::new_v4();
    svc.reset_docs_from_views(
        &[
            l1_view(id1, Some("p1"), Some("工作压力, 加班")),
            l1_view(id2, None, Some("爬山")), // persona None → 空串兜底
        ],
        &[l2_view(7, "p2", Some("职场, 内耗"))],
    );
    assert_eq!(svc.doc_count(), 3);

    // reset 幂等：重复装载不累积文档（同 id 覆盖 / 全量重建）
    svc.reset_docs_from_views(
        &[l1_view(id1, Some("p1"), Some("工作压力, 加班"))],
        &[l2_view(7, "p2", Some("职场, 内耗"))],
    );
    assert_eq!(svc.doc_count(), 2);
}

#[test]
fn incremental_index_and_remove_are_idempotent() {
    let mut svc = KeywordService::new();
    let id = uuid::Uuid::new_v4();
    let doc = l1_view(id, Some("p1"), Some("失眠, 焦虑"));
    svc.index_l1(&doc);
    svc.index_l1(&doc); // 同文档覆盖
    assert_eq!(svc.doc_count(), 1);

    assert!(svc.remove_l1(id));
    assert!(!svc.remove_l1(id), "重复移除返回 false");
    assert_eq!(svc.doc_count(), 0);

    // 批量移除幂等
    svc.index_l1(&doc);
    svc.index_l2(&l2_view(9, "p2", Some("加班")));
    assert_eq!(svc.remove_doc_batch(&[id], &[9]), 2);
    assert_eq!(svc.remove_doc_batch(&[id], &[9]), 0, "幂等：不存在即忽略");
}

// ---- persona 过滤 ----

#[tokio::test]
async fn persona_filter_keeps_isolation() {
    let mut svc = KeywordService::new();
    let p1_doc = uuid::Uuid::new_v4();
    let p2_doc = uuid::Uuid::new_v4();
    svc.reset_docs_from_views(
        &[
            l1_view(p1_doc, Some("p1"), Some("工作压力")),
            l1_view(p2_doc, Some("p2"), Some("工作压力")),
        ],
        &[],
    );
    // persona 限定：只命中本 persona 文档
    let r1 = svc
        .composite()
        .query(&query_for(&["工作压力"], Some("p1")), None)
        .await;
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0].0.persona_uid(), Some("p1"));
    // 全局（None）命中全部
    let rg = svc
        .composite()
        .query(&query_for(&["工作压力"], None), None)
        .await;
    assert_eq!(rg.len(), 2);
}

// ---- 语义层构建（锁外 build + 锁内 set）与降级 ----

#[tokio::test]
async fn build_fuzzy_embedder_none_keeps_two_layers() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    let fuzzy = KeywordService::build_fuzzy(&established(&svc), None).await;
    assert!(fuzzy.is_none(), "embedding 不可用 → 不构建语义层");
    svc.set_fuzzy(fuzzy);
    assert!(svc.composite().fuzzy().is_none());
    // 两层仍可查询（不含 fuzzy）
    let id = uuid::Uuid::new_v4();
    svc.index_l1(&l1_view(id, Some("p1"), Some("工作压力")));
    let r = svc
        .composite()
        .query(&query_for(&["工作压力"], Some("p1")), None)
        .await;
    assert_eq!(r.len(), 1);
}

#[tokio::test]
async fn build_fuzzy_empty_terms_returns_none() {
    let embedder = MockEmbedder::ok();
    let fuzzy = KeywordService::build_fuzzy(&[], Some(&embedder)).await;
    assert!(fuzzy.is_none(), "空词表 → 不构建语义层");
}

#[tokio::test]
async fn build_fuzzy_success_mounts_layer() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    let embedder = MockEmbedder::ok();
    let terms = established(&svc);
    let fuzzy = KeywordService::build_fuzzy(&terms, Some(&embedder))
        .await
        .expect("构建应成功");
    assert!(fuzzy.is_ready());
    assert_eq!(fuzzy.len(), terms.len());
    // 挂载由调用方在锁内完成
    svc.set_fuzzy(Some(fuzzy));
    assert!(svc.composite().fuzzy().is_some());
}

#[tokio::test]
async fn build_fuzzy_failure_degrades_to_none() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    let embedder = MockEmbedder::failing();
    let fuzzy = KeywordService::build_fuzzy(&established(&svc), Some(&embedder)).await;
    assert!(fuzzy.is_none(), "构建失败 → None（两层降级）");
    svc.set_fuzzy(fuzzy);
    assert!(svc.composite().fuzzy().is_none());
}

/// 陈旧判据：未构建且池非空 → 陈旧；构建后词数一致 → 不陈旧；池增长后 → 陈旧。
#[tokio::test]
async fn fuzzy_stale_reports_missing_and_outdated() {
    let mut svc = KeywordService::new();
    // 空池：无词表也无语义层 → 不算陈旧
    assert!(!svc.fuzzy_stale());
    assert!(!svc.warn_if_fuzzy_stale(NOW_MS), "空池不告警");

    svc.load_pool_entries(&sample_rows());
    assert!(svc.fuzzy_stale(), "池非空且语义层未构建 → 陈旧");

    // 构建并挂载后词数一致 → 不陈旧
    let embedder = MockEmbedder::ok();
    let terms = established(&svc);
    let fuzzy = KeywordService::build_fuzzy(&terms, Some(&embedder))
        .await
        .expect("构建应成功");
    svc.set_fuzzy(Some(fuzzy));
    assert!(!svc.fuzzy_stale(), "词数一致 → 不陈旧");

    // 池增长（新词）→ 词数不一致 → 陈旧
    svc.upsert_pool_tokens(&[token("新词")], NOW_MS);
    assert!(svc.fuzzy_stale(), "词表增长后 → 陈旧");
    assert!(svc.warn_if_fuzzy_stale(NOW_MS), "陈旧时告警返回 true");
}

/// 陈旧告警节流：窗口内不重复记录，超出窗口再次记录。
#[test]
fn warn_if_fuzzy_stale_is_throttled() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    assert!(svc.warn_if_fuzzy_stale(NOW_MS));
    assert_eq!(svc.fuzzy_stale_warned_at, Some(NOW_MS), "首次告警记录时间");

    // 节流窗口内：返回值仍报告陈旧，但不刷新告警时间
    assert!(svc.warn_if_fuzzy_stale(NOW_MS + FUZZY_STALE_WARN_INTERVAL_MS - 1));
    assert_eq!(svc.fuzzy_stale_warned_at, Some(NOW_MS));

    // 超出窗口：再次告警
    assert!(svc.warn_if_fuzzy_stale(NOW_MS + FUZZY_STALE_WARN_INTERVAL_MS));
    assert_eq!(
        svc.fuzzy_stale_warned_at,
        Some(NOW_MS + FUZZY_STALE_WARN_INTERVAL_MS)
    );
}

#[tokio::test]
async fn set_fuzzy_replaces_layer() {
    let mut svc = KeywordService::new();
    let embedder = MockEmbedder::ok();
    let terms = vec![token("工作压力"), token("职场焦虑")];
    let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
    svc.set_fuzzy(Some(fuzzy));
    assert!(svc.composite().fuzzy().is_some());
    svc.set_fuzzy(None);
    assert!(svc.composite().fuzzy().is_none());
}

// ---- 词典快照 / 自由文本查询（检索融合接线） ----

#[test]
fn pool_snapshot_contains_dict_and_alias_resolve() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    let snapshot = svc.pool_snapshot();
    // 词典仅含已确认词表（canonical + alias），pending 排除
    assert_eq!(snapshot.dictionary.len(), 3);
    assert!(snapshot.dictionary.iter().any(|d| d == "工作压力"));
    assert!(snapshot.dictionary.iter().any(|d| d == "职业倦怠"));
    assert!(
        !snapshot.dictionary.iter().any(|d| d == "职场焦虑"),
        "pending 词条不入词典"
    );
    // 解析表仅含已确认别名 → canonical；pending / canonical 自身不收录
    assert_eq!(
        snapshot.resolve().get("职业倦怠").map(String::as_str),
        Some("工作压力")
    );
    assert!(
        snapshot.resolve().get("职场焦虑").is_none(),
        "pending 不参与归一"
    );
    assert!(
        snapshot.resolve().get("工作压力").is_none(),
        "canonical 不收录自反映射"
    );
}

#[test]
fn composite_arc_shares_same_mirror() {
    let mut svc = KeywordService::new();
    let id = uuid::Uuid::new_v4();
    svc.index_l1(&l1_view(id, Some("p1"), Some("爬山")));
    let arc = svc.composite_arc();
    assert_eq!(arc.doc_count(), 1);
    // Arc 与 service 指向同一份镜像（写后旧 Arc 仍指向旧快照）
    svc.clear_docs();
    assert_eq!(arc.doc_count(), 1, "旧 Arc 快照不受后续写影响");
    assert_eq!(svc.doc_count(), 0);
}

/// 别名短语查询：用户文本含已确认别名词 → 命中规范词关键词文档（label 与 retriever 兼容）。
#[tokio::test]
async fn query_text_labels_aliases_to_canonical_doc() {
    let mut svc = KeywordService::new();
    // 词典：canonical 工作压力 + 已确认 alias 职业倦怠 → 工作压力
    svc.load_pool_entries(&sample_rows());
    let doc_id = uuid::Uuid::new_v4();
    svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));

    let hits = query_text_labels(
        svc.composite(),
        &svc.pool_snapshot(),
        "最近职业倦怠",
        Some("p1"),
        None,
        10,
    )
    .await;
    assert_eq!(hits.len(), 1, "别名查询应命中规范词文档");
    assert_eq!(hits[0].0, format!("L1:{doc_id}"));
}

/// 空词典（无池词条）→ 纯 bigram + 子串回退仍可命中。
#[tokio::test]
async fn query_text_labels_empty_pool_falls_back_bigram() {
    let mut svc = KeywordService::new();
    let doc_id = uuid::Uuid::new_v4();
    // 无 pool 词条；镜像文档关键词含 "工作压力"
    svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));

    let hits = query_text_labels(
        svc.composite(),
        &svc.pool_snapshot(),
        "工作压力很大",
        Some("p1"),
        None,
        10,
    )
    .await;
    assert!(
        hits.iter().any(|(l, _)| l == &format!("L1:{doc_id}")),
        "空词典时 bigram + 子串应命中（label 兼容）"
    );
}

/// 无镜像文档 / 空文本 / persona 隔离下自由文本查询的降级语义。
#[tokio::test]
async fn query_text_labels_degrade_cases() {
    let mut svc = KeywordService::new();
    svc.load_pool_entries(&sample_rows());
    // 无镜像文档 → 空
    let hits = query_text_labels(
        svc.composite(),
        &svc.pool_snapshot(),
        "工作压力",
        Some("p1"),
        None,
        10,
    )
    .await;
    assert!(hits.is_empty());

    // 空文本 → 空
    let doc_id = uuid::Uuid::new_v4();
    svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));
    let hits = query_text_labels(
        svc.composite(),
        &svc.pool_snapshot(),
        "   ",
        Some("p1"),
        None,
        10,
    )
    .await;
    assert!(hits.is_empty());

    // persona 隔离：查 p2 查不到 p1 文档
    let hits = query_text_labels(
        svc.composite(),
        &svc.pool_snapshot(),
        "工作压力",
        Some("p2"),
        None,
        10,
    )
    .await;
    assert!(hits.is_empty(), "跨 persona 隔离不命中");
}
