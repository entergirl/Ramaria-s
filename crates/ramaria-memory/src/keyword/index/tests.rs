//! crates/ramaria-memory/src/keyword/index/tests.rs - //! crates/ramaria-memory/src/keyword/index.rs — 关键词倒排索引单元测试
//!
//! 设计特点:
//! - 位于 keyword::index 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 index.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::keyword::{KeywordQuery, MatchStrategy};

/// 测试基准时间（Unix 毫秒）。
const NOW_MS: i64 = 2_000_000_000_000;

fn l1_ref(id: uuid::Uuid, persona: &str) -> KeywordRef {
    KeywordRef::L1 {
        id,
        persona_uid: persona.to_string(),
    }
}

fn l2_ref(id: i64, persona: &str) -> KeywordRef {
    KeywordRef::L2 {
        id,
        persona_uid: persona.to_string(),
    }
}

fn q(
    keywords: &[&str],
    persona: Option<&str>,
    strategy: MatchStrategy,
    top_k: usize,
) -> KeywordQuery {
    KeywordQuery::builder(persona.map(|s| s.to_string()))
        .keywords_from(keywords.iter().filter_map(|s| KeywordToken::new(s)))
        .strategy(strategy)
        .top_k(top_k)
        .build()
}

/// 构造含 3 条文档的小型索引。
fn sample_index() -> KeywordIndex {
    let mut index = KeywordIndex::new();
    // L1 高情感近期
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "user-0001"),
        Some("工作压力, 加班, 失眠"),
        0.9,
        NOW_MS - 86_400_000, // 1 天前
    );
    // L1 中性早期
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "user-0001"),
        Some("运动, 爬山, 放松"),
        0.5,
        NOW_MS - 30 * 86_400_000, // 30 天前
    );
    // L2 其他 persona
    index.index_parsed(
        l2_ref(1, "user-0002"),
        Some("工作压力, 职场"),
        0.7,
        NOW_MS - 86_400_000,
    );
    index
}

// ---- 基础功能 ----

#[test]
fn empty_index_query_empty() {
    let index = KeywordIndex::new();
    assert_eq!(index.doc_count(), 0);
    assert!(index.is_empty());
    let results = index.query(&q(&["工作"], None, MatchStrategy::Exact, 10));
    assert!(results.is_empty());
}

/// 精确匹配 + persona 隔离 + top_k 截断
#[test]
fn exact_query_persona_filter() {
    let index = sample_index();
    let results = index.query(&q(
        &["工作压力"],
        Some("user-0001"),
        MatchStrategy::Exact,
        10,
    ));
    // 只命中 user-0001 的 L1（user-0002 的 L2 被隔离）
    assert_eq!(results.len(), 1);
    match &results[0].0 {
        KeywordRef::L1 { persona_uid, .. } => assert_eq!(persona_uid, "user-0001"),
        other => panic!("应为 L1 引用，得到 {other:?}"),
    }
}

/// 无 persona 限定（全局）时命中所有文档
#[test]
fn exact_query_global() {
    let index = sample_index();
    let results = index.query(&q(&["工作压力"], None, MatchStrategy::Exact, 10));
    assert_eq!(results.len(), 2, "两个 persona 的文档都应命中");
}

/// 子串匹配：查"工作"命中"工作压力"
#[test]
fn substring_query_hits_superstring() {
    let index = sample_index();
    let exact = index.query(&q(&["工作"], Some("user-0001"), MatchStrategy::Exact, 10));
    assert!(exact.is_empty(), "精确匹配不命中子串");
    let sub = index.query(&q(
        &["工作"],
        Some("user-0001"),
        MatchStrategy::Substring,
        10,
    ));
    assert!(!sub.is_empty(), "子串应命中工作压力");
    assert_eq!(sub.len(), 1);
}

/// 子串查询对无命中词返回空（不 panic）
#[test]
fn substring_no_match_empty() {
    let index = sample_index();
    let results = index.query(&q(&["不存在词xyz"], None, MatchStrategy::Substring, 10));
    assert!(results.is_empty());
}

/// 子串候选 bigram 倒排与"全词表 contains 扫描"逐位等价的基准实现。
fn substring_hits_full_scan(index: &KeywordIndex, query_token: &str) -> Vec<String> {
    let mut hits: Vec<String> = index
        .exact_inverted
        .keys()
        .filter(|t| t.as_str().contains(query_token))
        .map(|t| t.as_str().to_string())
        .collect();
    hits.sort();
    hits
}

/// 子串候选（bigram 倒排 + contains 过滤）与全扫描命中集合逐位一致。
///
/// 覆盖：多字符命中 / 长查询词 / 单字符退化（无 bigram）/ 无命中 /
/// 含全部 bigram 但非子串的假阳性（必须经 contains 过滤剔除）。
#[test]
fn substring_bigram_candidates_match_full_scan() {
    let mut index = KeywordIndex::new();
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("工作压力, 爬山, 职场, aabbcc, 996"),
        0.5,
        NOW_MS,
    );

    let queries = [
        "工作",
        "工作压力",
        "作压",
        "压力",
        "职场",
        "工",
        "爬",
        "a",
        "aabb",
        "abc",
        "996",
        "不存在词xyz",
    ];
    for text in queries {
        let qt = KeywordToken::new(text).expect("查询词应有效");
        let mut expected = substring_hits_full_scan(&index, qt.as_str());
        let mut actual: Vec<String> = match index.substring_candidates(&qt) {
            Some(candidates) => candidates
                .into_iter()
                .filter(|t| t.as_str().contains(qt.as_str()))
                .map(|t| t.as_str().to_string())
                .collect(),
            // 单字符查询词无 bigram → 子串层退化为全词表扫描
            None => index
                .exact_inverted
                .keys()
                .filter(|t| t.as_str().contains(qt.as_str()))
                .map(|t| t.as_str().to_string())
                .collect(),
        };
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "query={text:?} 命中集合应逐位等价");
    }

    // 假阳性：aabbcc 含 "abc" 的全部 bigram 但非其子串——候选为超集，
    // 由 contains 过滤剔除（证明 bigram 倒排不是精确命中集）
    let abc = KeywordToken::new("abc").unwrap();
    let candidates = index
        .substring_candidates(&abc)
        .expect("多字符查询词应有候选");
    assert!(
        candidates.iter().any(|t| t.as_str() == "aabbcc"),
        "超集候选应含假阳性 aabbcc"
    );
    assert_eq!(
        substring_hits_full_scan(&index, "abc"),
        Vec::<String>::new()
    );
}

// ---- 评分 ----

/// recency：同为命中时，近期文档得分高于久远文档
#[test]
fn recency_boosts_newer_doc() {
    let mut index = KeywordIndex::new();
    let old_id = uuid::Uuid::new_v4();
    let new_id = uuid::Uuid::new_v4();
    index.index_parsed(
        l1_ref(old_id, "p1"),
        Some("爬山"),
        0.5,
        NOW_MS - 60 * 86_400_000,
    );
    index.index_parsed(l1_ref(new_id, "p1"), Some("爬山"), 0.5, NOW_MS - 86_400_000);
    let results =
        index.query_with_time(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 10), NOW_MS);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, l1_ref(new_id, "p1"), "近期文档应排前");
}

/// salience：同时期文档，高显著性命中得分更高
#[test]
fn salience_boosts_higher_significance() {
    let mut index = KeywordIndex::new();
    let low_id = uuid::Uuid::new_v4();
    let high_id = uuid::Uuid::new_v4();
    index.index_parsed(l1_ref(low_id, "p1"), Some("爬山"), 0.2, NOW_MS - 86_400_000);
    index.index_parsed(
        l1_ref(high_id, "p1"),
        Some("爬山"),
        0.9,
        NOW_MS - 86_400_000,
    );
    let results =
        index.query_with_time(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 10), NOW_MS);
    assert_eq!(results[0].0, l1_ref(high_id, "p1"), "高显著性命中应排前");
}

/// IDF：文档频率越高信息量越低（验证 idf 单调性 + 倒排 df 统计正确）
#[test]
fn idf_prefers_rare_term() {
    let mut index = KeywordIndex::new();
    // 文档 A 只含稀有词"量子"
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("量子"),
        0.5,
        NOW_MS - 86_400_000,
    );
    // 再加 9 篇含高频词"工作"的文档（含初始 1 篇共 9 篇命中）
    for _ in 0..9 {
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("工作"),
            0.5,
            NOW_MS - 86_400_000,
        );
    }
    let df_work = index
        .exact_inverted
        .get(&KeywordToken::new("工作").unwrap())
        .map(|v| v.len())
        .unwrap_or(0);
    assert_eq!(df_work, 9);
    let n = index.doc_count() as f64;
    let idf_rare = KeywordIndex::idf(n, 1.0);
    let idf_common = KeywordIndex::idf(n, df_work as f64);
    assert!(idf_rare > idf_common, "稀有词 IDF 应更高");
}

// ---- 维护 ----

/// remove 单文档
#[test]
fn remove_doc() {
    let mut index = sample_index();
    let id = uuid::Uuid::new_v4();
    let reff = l1_ref(id, "user-0001");
    index.index_parsed(reff.clone(), Some("独有词"), 0.5, NOW_MS);
    assert_eq!(index.doc_count(), 4);
    assert!(index.remove(&reff));
    assert!(!index.remove(&reff), "重复移除返回 false");
    assert_eq!(index.doc_count(), 3);
    let results = index.query(&q(&["独有词"], None, MatchStrategy::Exact, 10));
    assert!(results.is_empty());
}

/// remove_doc_batch 批量移除 L1/L2
#[test]
fn remove_doc_batch() {
    let mut index = sample_index();
    let l1_a = uuid::Uuid::new_v4();
    let l1_b = uuid::Uuid::new_v4();
    index.index_parsed(l1_ref(l1_a, "p"), Some("甲"), 0.5, NOW_MS);
    index.index_parsed(l1_ref(l1_b, "p"), Some("乙"), 0.5, NOW_MS);
    let removed = index.remove_doc_batch(&[l1_a, l1_b], &[]);
    assert_eq!(removed, 2);
    assert!(
        index
            .query(&q(&["甲"], None, MatchStrategy::Exact, 5))
            .is_empty()
    );
}

/// 精准移除的倒排维护：swap_remove 下标修正、空 postings 连键清理、bigram 清理、
/// 共享 token 的 postings 重映射（被删文档与搬移文档共享 token）。
#[test]
fn remove_keeps_inverted_consistent() {
    let mut index = KeywordIndex::new();
    let keep_a = l1_ref(uuid::Uuid::new_v4(), "p1");
    let remove = l1_ref(uuid::Uuid::new_v4(), "p1");
    let keep_b = l1_ref(uuid::Uuid::new_v4(), "p1");

    index.index_parsed(keep_a.clone(), Some("工作压力, 爬山"), 0.5, NOW_MS);
    index.index_parsed(remove.clone(), Some("独有词"), 0.5, NOW_MS);
    index.index_parsed(keep_b.clone(), Some("爬山, 露营"), 0.5, NOW_MS);

    // 移除中间文档：末尾文档被 swap 到它的位置，剩余两篇仍可命中；
    // "爬山" 为跨 3 篇的共享 token，其 postings 需从 {0,1,2} 修正为 {0,1}
    assert!(index.remove(&remove));
    assert_eq!(
        index
            .query(&q(&["工作压力"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        1
    );
    assert_eq!(
        index
            .query(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        2,
        "共享 token 的两篇文档都应命中"
    );
    assert_eq!(
        index
            .query(&q(&["露营"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        1,
        "搬移文档的独有 token 应以新下标命中"
    );

    // 被删文档的独有词：精确倒排与 bigram 倒排均清理
    let unique = KeywordToken::new("独有词").unwrap();
    assert!(!index.exact_inverted.contains_key(&unique));
    assert!(
        !index
            .token_bigrams
            .values()
            .any(|set| set.contains(&unique))
    );

    // 重索引被搬移过的文档（幂等路径 + 下标修正后仍正确）
    index.index_parsed(keep_b.clone(), Some("爬山, 露营"), 0.5, NOW_MS);
    assert_eq!(index.doc_count(), 2);
    assert_eq!(
        index
            .query(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        2
    );
    assert_eq!(
        index
            .query(&q(&["露营"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        1
    );
}

/// 容量上限：默认 None 不限制；设置上限后按 created_at 最旧驱逐。
#[test]
fn max_docs_evicts_oldest() {
    let mut index = KeywordIndex::new();
    assert_eq!(index.doc_capacity(), None, "默认不限制");

    index.set_max_docs(Some(2));
    let old_id = uuid::Uuid::new_v4();
    let mid_id = uuid::Uuid::new_v4();
    index.index_parsed(
        l1_ref(old_id, "p1"),
        Some("旧词"),
        0.5,
        NOW_MS - 20 * 86_400_000,
    );
    index.index_parsed(
        l1_ref(mid_id, "p1"),
        Some("中词"),
        0.5,
        NOW_MS - 10 * 86_400_000,
    );
    assert_eq!(index.doc_count(), 2, "未超上限不驱逐");
    assert_eq!(index.doc_capacity(), Some(2));

    // 第 3 篇写入触发驱逐：最旧的"旧词"文档被移除
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("新词"),
        0.5,
        NOW_MS,
    );
    assert_eq!(index.doc_count(), 2);
    assert!(
        index
            .query(&q(&["旧词"], Some("p1"), MatchStrategy::Exact, 5))
            .is_empty(),
        "最旧文档应被驱逐"
    );
    assert_eq!(
        index
            .query(&q(&["中词"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        1
    );
    assert_eq!(
        index
            .query(&q(&["新词"], Some("p1"), MatchStrategy::Exact, 5))
            .len(),
        1
    );

    // 解除上限后恢复不限制
    index.set_max_docs(None);
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("追加词"),
        0.5,
        NOW_MS,
    );
    assert_eq!(index.doc_count(), 3);
    assert_eq!(index.doc_capacity(), None);
}

/// 幂等重建：同 reff 重复 index 覆盖而非累积
#[test]
fn reindex_is_idempotent() {
    let mut index = KeywordIndex::new();
    let reff = l1_ref(uuid::Uuid::new_v4(), "p1");
    index.index_parsed(reff.clone(), Some("旧词"), 0.5, NOW_MS);
    assert_eq!(index.doc_count(), 1);
    // 同文档更新关键词
    index.index_parsed(reff.clone(), Some("新词"), 0.9, NOW_MS);
    assert_eq!(index.doc_count(), 1, "重复 index 不累积文档");
    assert!(
        index
            .query(&q(&["旧词"], None, MatchStrategy::Exact, 5))
            .is_empty()
    );
    assert_eq!(
        index
            .query(&q(&["新词"], None, MatchStrategy::Exact, 5))
            .len(),
        1
    );
}

/// clear 清空全部
#[test]
fn clear_resets() {
    let mut index = sample_index();
    index.clear();
    assert_eq!(index.doc_count(), 0);
    assert!(
        index
            .query(&q(&["工作压力"], None, MatchStrategy::Exact, 5))
            .is_empty()
    );
}

/// top_k 截断
#[test]
fn top_k_truncates() {
    let mut index = KeywordIndex::new();
    for i in 0..5 {
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("爬山"),
            0.5,
            NOW_MS - i * 86_400_000,
        );
    }
    let results = index.query(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 3));
    assert_eq!(results.len(), 3);
}

/// 同文档命中多个查询词时不重复计分（每个文档只出现一次）
#[test]
fn one_result_per_doc() {
    let mut index = KeywordIndex::new();
    index.index_parsed(
        l1_ref(uuid::Uuid::new_v4(), "p1"),
        Some("工作压力, 加班"),
        0.5,
        NOW_MS - 86_400_000,
    );
    let results = index.query(&q(
        &["工作压力", "加班"],
        Some("p1"),
        MatchStrategy::Exact,
        5,
    ));
    assert_eq!(results.len(), 1, "一篇文档只应出现在结果中一次");
}

/// Pool 词条不入索引（debug_assert 仅在测试构建可验证，此处验证 index 忽略语义由上层保证）
#[test]
fn query_empty_keywords() {
    let index = sample_index();
    let empty = q(&[], None, MatchStrategy::Exact, 5);
    assert!(index.query(&empty).is_empty());
}

/// created_at 同分排序稳定性
#[test]
fn same_score_tiebreak_by_newest() {
    let mut index = KeywordIndex::new();
    let newer = l1_ref(uuid::Uuid::new_v4(), "p1");
    let older = l1_ref(uuid::Uuid::new_v4(), "p1");
    index.index_parsed(older.clone(), Some("爬山"), 0.5, NOW_MS - 10 * 86_400_000);
    index.index_parsed(newer.clone(), Some("爬山"), 0.5, NOW_MS - 86_400_000);
    let results = index.query_with_time(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 5), NOW_MS);
    assert_eq!(results[0].0, newer);
    assert_eq!(results[1].0, older);
}
