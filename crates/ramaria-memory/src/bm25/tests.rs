//! crates/ramaria-memory/src/bm25/tests.rs - //! crates/ramaria-memory/src/bm25.rs — BM25 全文检索引擎单元测试
//!
//! 设计特点:
//! - 位于 bm25 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 bm25.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ---- tokenize ----

/// tokenize 各输入参数化验证：中文 bigram / 英文小写 / 混合 / 空 / 标点 / 单字符过滤。
#[test]
fn tokenize_cases() {
    // 中文 bigram
    let tokens = tokenize("机器学习");
    assert!(tokens.contains(&"机器".to_string()));
    assert!(tokens.contains(&"器学".to_string()));
    assert!(tokens.contains(&"学习".to_string()));
    // 英文小写，过滤单字母
    let tokens = tokenize("Machine Learning");
    assert!(tokens.contains(&"machine".to_string()));
    assert!(tokens.contains(&"learning".to_string()));
    assert!(!tokens.iter().any(|t| t.len() < 2));
    // 中英混合
    let tokens = tokenize("我在学Rust和Python");
    assert!(tokens.contains(&"我在".to_string()) || tokens.contains(&"在学".to_string()));
    assert!(tokens.contains(&"rust".to_string()));
    assert!(tokens.contains(&"python".to_string()));
    // 空输入
    assert!(tokenize("").is_empty());
    // 单中文字符不构成 bigram
    assert!(tokenize("我").is_empty());
    // 标点被移除
    let tokens = tokenize("你好！世界？");
    assert!(!tokens.contains(&"！世".to_string()));
    assert!(tokens.contains(&"你好".to_string()));
    assert!(tokens.contains(&"世界".to_string()));
    // 单字母英文被过滤
    assert!(tokenize("a b c").is_empty(), "单字母 token 应被过滤");
}

// ---- Bm25Index ----

#[test]
fn index_empty_search_returns_empty() {
    let index = Bm25Index::new();
    let config = Bm25Config::default();
    let results = index.search("测试", &config);
    assert!(results.is_empty());
}

#[test]
fn index_add_and_search_single_doc() {
    let mut index = Bm25Index::new();
    let config = Bm25Config::default();

    let doc_id = DocId::L1(uuid::Uuid::new_v4());
    index.add_tokenized(doc_id.clone(), &["今天天气很好适合出门"]);
    assert_eq!(index.doc_count(), 1);

    let results = index.search("天气", &config);
    assert!(!results.is_empty());
    assert_eq!(results[0].0, doc_id);
    assert!(results[0].1 > 0.0);
}

#[test]
fn index_search_ranking() {
    let mut index = Bm25Index::new();
    let config = Bm25Config::default();

    let doc_a = DocId::L1(uuid::Uuid::new_v4());
    let doc_b = DocId::L1(uuid::Uuid::new_v4());

    // doc_a 提到"天气"一次
    index.add_tokenized(doc_a.clone(), &["今天天气很好"]);
    // doc_b 提到"天气"多次
    index.add_tokenized(doc_b.clone(), &["天气天气天气很好"]);

    let results = index.search("天气", &config);
    assert_eq!(results.len(), 2);
    // doc_b 应有更高得分
    assert!(results[0].1 > results[1].1);
}

#[test]
fn index_remove_doc() {
    let mut index = Bm25Index::new();
    let config = Bm25Config::default();

    let doc_id = DocId::L1(uuid::Uuid::new_v4());
    index.add_tokenized(doc_id.clone(), &["今天天气很好"]);
    assert_eq!(index.doc_count(), 1);

    index.remove(&doc_id);
    assert_eq!(index.doc_count(), 0);

    let results = index.search("天气", &config);
    assert!(results.is_empty());
}

#[test]
fn index_remove_nonexistent() {
    let mut index = Bm25Index::new();
    let doc_id = DocId::L1(uuid::Uuid::new_v4());
    // 不应 panic
    index.remove(&doc_id);
}

#[test]
fn index_clear() {
    let mut index = Bm25Index::new();
    index.add_tokenized(DocId::L1(uuid::Uuid::new_v4()), &["测试"]);
    index.add_tokenized(DocId::L2(42), &["测试2"]);
    assert_eq!(index.doc_count(), 2);

    index.clear();
    assert_eq!(index.doc_count(), 0);
    assert!(index.df.is_empty());
    assert_eq!(index.total_tokens, 0);
}

#[test]
fn index_avg_doc_len() {
    let mut index = Bm25Index::new();

    // 空索引 avg = 1.0
    assert!((index.avg_doc_len() - 1.0).abs() < f64::EPSILON);

    // 添加 8 token 的文档（按字符拆分作为 tokens）
    let token_count = "机器学习很有意思".chars().count() as u32;
    let tokens: Vec<String> = "机器学习很有意思".chars().map(|c| c.to_string()).collect();
    index.add(DocId::L1(uuid::Uuid::new_v4()), tokens);
    assert!((index.avg_doc_len() - token_count as f64).abs() < 0.01);
}

#[test]
fn index_add_overwrite() {
    let mut index = Bm25Index::new();
    let config = Bm25Config::default();
    let doc_id = DocId::L1(uuid::Uuid::new_v4());

    index.add_tokenized(doc_id.clone(), &["天气"]);
    // 覆盖添加
    index.add_tokenized(doc_id.clone(), &["吃饭"]);

    // 只有 "吃饭" 的索引
    let results_weather = index.search("天气", &config);
    assert!(results_weather.is_empty());

    let results_eat = index.search("吃饭", &config);
    assert!(!results_eat.is_empty());
}

/// 全空字段文档（doc_len = 0）在 b = 1 时不产生 NaN，正常文档的召回不受影响。
#[test]
fn empty_doc_with_b_equals_one_keeps_recall() {
    let mut index = Bm25Index::new();
    let config = Bm25Config { k1: 1.2, b: 1.0 };

    // 全空字段文档：无任何 token，doc_len = 0
    let empty_id = DocId::L1(uuid::Uuid::new_v4());
    index.add(empty_id.clone(), Vec::new());

    let doc_id = DocId::L1(uuid::Uuid::new_v4());
    index.add_tokenized(doc_id.clone(), &["今天天气很好"]);

    let results = index.search("天气", &config);
    assert!(
        results.iter().any(|(id, _)| *id == doc_id),
        "含查询词的文档应被召回"
    );
    assert!(
        results.iter().all(|(_, score)| score.is_finite()),
        "返回分数必须有限（不得出现 NaN/Inf）"
    );
    assert!(
        !results.iter().any(|(id, _)| *id == empty_id),
        "全空文档无任何命中词，不应出现在结果中"
    );
}

/// 带上限检索：返回 top-limit 且保持降序；limit = 0 与全量检索完全一致。
#[test]
fn search_limited_returns_top_k_descending() {
    let mut index = Bm25Index::new();
    let config = Bm25Config::default();

    // 10 篇文档共享查询词「天气」，词频递增形成唯一的分数梯度
    for i in 0..10usize {
        let text = "天气".repeat(i + 1);
        index.add_tokenized(DocId::L2(i as i64), &[text.as_str()]);
    }

    let all = index.search("天气", &config);
    assert_eq!(all.len(), 10, "10 篇文档均含查询词");
    assert_eq!(
        index.search_limited("天气", &config, 0),
        all,
        "limit = 0 应与全量检索完全一致"
    );

    let top3 = index.search_limited("天气", &config, 3);
    assert_eq!(top3.len(), 3);
    assert_eq!(top3[0], all[0], "首位应与全量检索首位一致");
    assert!(
        top3.windows(2).all(|w| w[0].1 >= w[1].1),
        "结果应保持分数降序"
    );
    assert!(
        top3.iter().all(|(_, score)| *score >= all[3].1),
        "top-3 分数应不低于全量第 4 名"
    );

    // 上限大于命中数时返回全部命中
    assert_eq!(index.search_limited("天气", &config, 100), all);
}

// ---- DocId Display ----

#[test]
fn doc_id_display() {
    let l1_id = uuid::Uuid::new_v4();
    let display = DocId::L1(l1_id).to_string();
    assert!(display.starts_with("L1:"));
    assert!(display.contains(&l1_id.to_string()));

    let display = DocId::L2(42).to_string();
    assert_eq!(display, "L2:42");
}

// ---- 词典增强分词迁移（默认无词典=现状回归 / 有词典=整词口径）----

/// 无词典默认路径回归：索引实例分词与自由函数 `tokenize`/`tokenize_fields` 逐 token 一致。
#[test]
fn default_no_dictionary_matches_free_tokenize() {
    let index = Bm25Index::new();
    assert!(index.dictionary_is_empty(), "默认分词器应为空词典");
    assert!(
        Bm25Index::with_dictionary(&[]).dictionary_is_empty(),
        "空词典应退化"
    );

    let text = "最近工作压力很大";
    let fields = [text, "学习Rust"];
    assert_eq!(index.tokenize_with(text), tokenize(text));
    assert_eq!(
        index.tokenize_fields_with(&fields),
        tokenize_fields(&fields)
    );
}

/// 有词典时：文档整词命中、查询按整词切分、跨词噪声（如 "作压"）不再命中。
#[test]
fn dictionary_keeps_whole_word_and_removes_noise() {
    let dict = ["工作压力".to_string()];
    let mut index = Bm25Index::with_dictionary(&dict);
    let config = Bm25Config::default();
    let doc_id = DocId::L1(uuid::Uuid::new_v4());
    index.add_tokenized(doc_id.clone(), &["工作压力很大"]);

    // token 分布：词典整词 + 尾部 bigram，无跨词噪声
    let tokens = index.tokenize_fields_with(&["工作压力很大"]);
    assert!(tokens.contains(&"工作压力".to_string()));
    assert!(tokens.contains(&"很大".to_string()));
    assert!(
        !tokens.contains(&"作压".to_string()),
        "词典命中后不得产生跨词噪声 token"
    );

    // 查询 "工作压力" 整词命中；噪声 "作压" 不命中（纯 bigram 口径会命中）
    let hit = index.search("工作压力", &config);
    assert_eq!(hit.len(), 1, "整词查询应命中一篇文档");
    assert_eq!(hit[0].0, doc_id);
    assert!(
        index.search("作压", &config).is_empty(),
        "索引无噪声 token 时 '作压' 不应命中"
    );
}

/// 词典热更新：重建场景先 set_dictionary 再覆盖文档，口径切换后噪声消失。
#[test]
fn set_dictionary_switches_tokenizer_on_rebuild() {
    let mut index = Bm25Index::new();
    let config = Bm25Config::default();
    let doc_id = DocId::L1(uuid::Uuid::new_v4());

    // 旧版本（纯 bigram）口径：噪声 "作压" 可命中
    index.add_tokenized(doc_id.clone(), &["工作压力很大"]);
    assert!(
        !index.search("作压", &config).is_empty(),
        "bigram 口径下 '作压' 应能命中（旧版行为基线）"
    );

    // 迁移：注入词典 → 覆盖重建同一文档 → 口径切为词典增强
    index.set_dictionary(&["工作压力".to_string()]);
    index.add_tokenized(doc_id.clone(), &["工作压力很大"]);
    assert!(
        index.search("作压", &config).is_empty(),
        "词典口径下噪声命中应消失"
    );
    assert_eq!(index.search("工作压力", &config).len(), 1);
}

/// 词典切换推进代次：存量文档标记为陈旧、覆盖重建后同步、重复注入不换代次。
#[test]
fn set_dictionary_marks_existing_docs_stale() {
    let mut index = Bm25Index::new();
    let doc_id = DocId::L1(uuid::Uuid::new_v4());
    let dictionary = ["工作压力".to_string()];

    // 默认无词典口径写入：文档与当前代次一致
    index.add_tokenized(doc_id.clone(), &["工作压力很大"]);
    assert_eq!(index.stale_doc_count(), 0);

    // 切换词典：代次推进，存量文档标记为陈旧
    index.set_dictionary(&dictionary);
    assert_eq!(index.stale_doc_count(), 1);

    // 覆盖重建同一文档：按新代次重新盖章
    index.add_tokenized(doc_id.clone(), &["工作压力很大"]);
    assert_eq!(index.stale_doc_count(), 0);

    // 再次注入同一词典：指纹未变，不推进代次
    index.set_dictionary(&dictionary);
    assert_eq!(index.stale_doc_count(), 0);
}
