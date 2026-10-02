//! crates/ramaria-core/src/keyword/tests.rs - Ramaria 关键词模块单元测试
//!
//! 设计特点:
//! - 覆盖 KeywordToken 规范化、边界拒绝与哈希一致性
//! - 校验 KeywordSet 去重、顺序保持、迭代与 serde 往返
//! - 锁定 KeywordStatus 变体语义、默认值与序列化行为
//! - 验证 KeywordRef 各变体的类型/主键/隐私标签约定
//! - 断言 KeywordQueryBuilder 默认值、top_k 钳制与集合构造

use super::*;
use serde_json;

// ── KeywordToken 测试 ──

/// KeywordToken::new 规范化：中文保留 / 英文小写 / trim 空白
#[test]
fn keyword_token_normalization_cases() {
    let cases = [
        ("工作压力", "工作压力"),
        ("Work Stress", "work stress"),
        ("DeepSeek-API", "deepseek-api"),
        ("  职业倦怠  ", "职业倦怠"),
    ];
    for (input, expected) in cases {
        let token = KeywordToken::new(input).expect("关键词应能构造");
        assert_eq!(token.as_str(), expected, "input={input:?}");
    }
}

/// KeywordToken::new 无效输入：空/纯空白/超长 → None；边界长度 → Some
#[test]
fn keyword_token_invalid_inputs() {
    for bad in ["", "   ", "\t\n", &"x".repeat(257)] {
        assert!(KeywordToken::new(bad).is_none(), "input 应被拒绝");
    }
    let boundary = "x".repeat(256);
    let token = KeywordToken::new(&boundary);
    assert!(token.is_some());
    assert_eq!(token.unwrap().len(), 256);
}

/// Display 输出与 as_str 一致
#[test]
fn keyword_token_display() {
    let token = KeywordToken::new("人际关系").unwrap();
    assert_eq!(format!("{}", token), "人际关系");
}

/// PartialEq 比较
#[test]
fn keyword_token_partial_eq() {
    let a = KeywordToken::new("Work").unwrap();
    let b = KeywordToken::new("work").unwrap();
    assert_eq!(a, b);
}

/// Hash 一致性（相同标准化结果应 hash 相同）
#[test]
fn keyword_token_hash_consistency() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(KeywordToken::new("Work").unwrap());
    set.insert(KeywordToken::new("work").unwrap());
    assert_eq!(set.len(), 1, "相同标准化结果应去重");
}

/// Serialize + Deserialize 往返
#[test]
fn keyword_token_serde_roundtrip() {
    let token = KeywordToken::new("职业倦怠").unwrap();
    let json = serde_json::to_string(&token).unwrap();
    let deserialized: KeywordToken = serde_json::from_str(&json).unwrap();
    assert_eq!(token, deserialized);
}

/// into_inner 消费 self（From<KeywordToken> for String 委托同一实现）
#[test]
fn keyword_token_into_inner() {
    let token = KeywordToken::new("测试").unwrap();
    let s: String = token.into_inner();
    assert_eq!(s, "测试");
}

/// AsRef<str>
#[test]
fn keyword_token_as_ref_str() {
    let token = KeywordToken::new("test").unwrap();
    let s: &str = token.as_ref();
    assert_eq!(s, "test");
}

// ── KeywordSet 测试 ──

/// 空集合
#[test]
fn keyword_set_empty() {
    let set = KeywordSet::new();
    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
}

/// 插入去重
#[test]
fn keyword_set_dedup() {
    let mut set = KeywordSet::new();
    assert!(set.insert(KeywordToken::new("压力").unwrap()));
    assert!(
        !set.insert(KeywordToken::new("压力").unwrap()),
        "重复插入应返回 false"
    );
    assert_eq!(set.len(), 1);
}

/// 保留插入顺序
#[test]
fn keyword_set_order() {
    let mut set = KeywordSet::new();
    set.insert(KeywordToken::new("工作").unwrap());
    set.insert(KeywordToken::new("压力").unwrap());
    set.insert(KeywordToken::new("倦怠").unwrap());
    let tokens: Vec<&str> = set.iter().map(|t| t.as_str()).collect();
    assert_eq!(tokens, vec!["工作", "压力", "倦怠"]);
}

/// from_iter
#[test]
fn keyword_set_from_iter() {
    let tokens = vec![
        KeywordToken::new("A").unwrap(),
        KeywordToken::new("B").unwrap(),
        KeywordToken::new("A").unwrap(), // 重复
    ];
    let set: KeywordSet = tokens.into_iter().collect();
    assert_eq!(set.len(), 2);
}

/// into_iter 消费
#[test]
fn keyword_set_into_iter() {
    let mut set = KeywordSet::new();
    set.insert(KeywordToken::new("X").unwrap());
    set.insert(KeywordToken::new("Y").unwrap());
    let strings: Vec<String> = set.into_iter().map(|t| t.into_inner()).collect();
    assert_eq!(strings, vec!["x", "y"]);
}

/// contains
#[test]
fn keyword_set_contains() {
    let mut set = KeywordSet::new();
    set.insert(KeywordToken::new("测试").unwrap());
    assert!(set.contains(&KeywordToken::new("测试").unwrap()));
    assert!(!set.contains(&KeywordToken::new("不存在").unwrap()));
}

/// extend
#[test]
fn keyword_set_extend() {
    let mut set = KeywordSet::new();
    set.insert(KeywordToken::new("A").unwrap());
    let more = vec![
        KeywordToken::new("B").unwrap(),
        KeywordToken::new("C").unwrap(),
    ];
    set.extend(more);
    assert_eq!(set.len(), 3);
}

/// Serialize + Deserialize 往返
#[test]
fn keyword_set_serde_roundtrip() {
    let mut set = KeywordSet::new();
    set.insert(KeywordToken::new("a").unwrap());
    set.insert(KeywordToken::new("b").unwrap());
    let json = serde_json::to_string(&set).unwrap();
    let deserialized: KeywordSet = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.len(), 2);
}

// ── KeywordStatus 测试 ──

/// Canonical 默认值和标识
#[test]
fn keyword_status_canonical() {
    let status = KeywordStatus::Canonical;
    assert!(status.is_canonical());
    assert_eq!(status.as_str(), "canonical");
    assert!(status.canonical_id().is_none());
}

/// Alias 构造和查询
#[test]
fn keyword_status_alias() {
    let status = KeywordStatus::Alias { canonical_id: 42 };
    assert!(!status.is_canonical());
    assert_eq!(status.as_str(), "alias");
    assert_eq!(status.canonical_id(), Some(42));
}

/// Pending 构造和查询
#[test]
fn keyword_status_pending() {
    let status = KeywordStatus::Pending {
        suggested_canonical_id: 100,
    };
    assert!(!status.is_canonical());
    assert_eq!(status.as_str(), "pending");
    assert_eq!(status.canonical_id(), Some(100));
}

/// 默认值为 Canonical
#[test]
fn keyword_status_default() {
    let status: KeywordStatus = Default::default();
    assert_eq!(status, KeywordStatus::Canonical);
}

/// Serialize + Deserialize 往返（Canonical）
#[test]
fn keyword_status_serde_canonical() {
    let status = KeywordStatus::Canonical;
    let json = serde_json::to_string(&status).unwrap();
    let deserialized: KeywordStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(status, deserialized);
}

/// Serialize + Deserialize 往返（Alias）
#[test]
fn keyword_status_serde_alias() {
    let status = KeywordStatus::Alias { canonical_id: 7 };
    let json = serde_json::to_string(&status).unwrap();
    let deserialized: KeywordStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(status, deserialized);
    // 验证字段值
    match deserialized {
        KeywordStatus::Alias { canonical_id } => assert_eq!(canonical_id, 7),
        _ => panic!("应为 Alias"),
    }
}

// ── KeywordRef 测试 ──

/// KeywordRef 各变体的 doc_type/doc_id_text/persona_uid/label 查询
#[test]
fn keyword_ref_variants() {
    let l1_id = uuid::Uuid::new_v4();
    let cases = vec![
        (
            KeywordRef::L1 {
                id: l1_id,
                persona_uid: "p1".to_string(),
            },
            "l1",
            Some(l1_id.to_string()),
            Some("p1"),
        ),
        (
            KeywordRef::L2 {
                id: 456,
                persona_uid: "p2".to_string(),
            },
            "l2",
            Some("456".to_string()),
            Some("p2"),
        ),
        (
            KeywordRef::Pool {
                keyword: "测试词".to_string(),
            },
            "pool",
            None,
            None,
        ),
    ];
    for (r, dt, did, pu) in cases {
        assert_eq!(r.doc_type(), dt);
        assert_eq!(r.doc_id_text(), did);
        assert_eq!(r.persona_uid(), pu);
        // label 用于日志，前缀应与 doc_type 一致
        assert!(
            r.label().starts_with(dt),
            "label 应带 {dt} 前缀: {}",
            r.label()
        );
    }
}

/// Serialize + Deserialize 往返
#[test]
fn keyword_ref_serde_roundtrip() {
    let cases = vec![
        KeywordRef::L1 {
            id: uuid::Uuid::new_v4(),
            persona_uid: "u1".into(),
        },
        KeywordRef::L2 {
            id: 2,
            persona_uid: "u2".into(),
        },
        KeywordRef::Pool {
            keyword: "kw".into(),
        },
    ];
    for r in cases {
        let json = serde_json::to_string(&r).unwrap();
        let deserialized: KeywordRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, deserialized, "JSON 往返失败: {}", json);
    }
}

/// KeywordRef::label 不含文档正文（隐私：仅类型 + 主键）
#[test]
fn keyword_ref_label_has_no_summary_text() {
    let r = KeywordRef::L2 {
        id: 7,
        persona_uid: "user-0001".into(),
    };
    assert_eq!(r.label(), "l2:7");
}

// ── MatchStrategy 测试 ──

/// MatchStrategy 仅 Exact/Substring（无 Prefix），as_str 稳定
#[test]
fn match_strategy_variants() {
    assert_eq!(MatchStrategy::Exact.as_str(), "exact");
    assert_eq!(MatchStrategy::Substring.as_str(), "substring");
    assert_ne!(MatchStrategy::Exact, MatchStrategy::Substring);
}

/// MatchStrategy serde 往返
#[test]
fn match_strategy_serde_roundtrip() {
    for s in [MatchStrategy::Exact, MatchStrategy::Substring] {
        let json = serde_json::to_string(&s).unwrap();
        let back: MatchStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}

// ── KeywordQuery 测试 ──

/// 默认构建参数：Exact / top_k=10 / persona 透传
#[test]
fn keyword_query_builder_defaults() {
    let q = KeywordQuery::builder(Some("user-0001".into())).build();
    assert_eq!(q.strategy, MatchStrategy::Exact);
    assert_eq!(q.top_k, 10);
    assert_eq!(q.persona_uid.as_deref(), Some("user-0001"));
    assert!(q.keywords.is_empty());
}

/// top_k 钳制：0 / 超大值都收敛到 1..=100
#[test]
fn keyword_query_top_k_clamped() {
    let low = KeywordQuery::builder(None).top_k(0).build();
    assert_eq!(low.top_k, 1);
    let high = KeywordQuery::builder(None).top_k(9999).build();
    assert_eq!(high.top_k, MAX_QUERY_TOP_K);
    let ok = KeywordQuery::builder(None).top_k(50).build();
    assert_eq!(ok.top_k, 50);
}

/// 关键词集合与策略可配置
#[test]
fn keyword_query_full_build() {
    let mut set = KeywordSet::new();
    set.insert(KeywordToken::new("工作压力").unwrap());
    set.insert(KeywordToken::new("加班").unwrap());
    let q = KeywordQuery::builder(Some("user-1".into()))
        .with_keywords(set)
        .strategy(MatchStrategy::Substring)
        .top_k(5)
        .build();
    assert_eq!(q.keywords.len(), 2);
    assert_eq!(q.strategy, MatchStrategy::Substring);
    assert_eq!(q.top_k, 5);
}

/// builder.keywords_from / add_keyword 便捷路径
#[test]
fn keyword_query_token_collection_helpers() {
    let q = KeywordQuery::builder(None)
        .keywords_from(vec![
            KeywordToken::new("A").unwrap(),
            KeywordToken::new("B").unwrap(),
            KeywordToken::new("A").unwrap(), // 去重
        ])
        .add_keyword(KeywordToken::new("C").unwrap())
        .build();
    assert_eq!(q.keywords.len(), 3);
}

/// KeywordQuery serde 往返（构造钳制后不变量保持）
#[test]
fn keyword_query_serde_roundtrip() {
    let q = KeywordQuery::builder(Some("p".into()))
        .keywords_from(vec![KeywordToken::new("测试").unwrap()])
        .top_k(3)
        .build();
    let json = serde_json::to_string(&q).unwrap();
    let back: KeywordQuery = serde_json::from_str(&json).unwrap();
    assert_eq!(q, back);
}
