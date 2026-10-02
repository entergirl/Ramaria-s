//! crates/ramaria-memory/src/keyword/alias/tests.rs - //! crates/ramaria-memory/src/keyword/alias.rs - 关键词别名管理模块单元测试
//!
//! 设计特点:
//! - 位于 keyword::alias 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 alias.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ── 别名注册与查询 ──

/// 基本别名注册和查询
#[test]
fn register_and_resolve_alias() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("职场焦虑", 1, "工作压力").unwrap();
    let result = mgr.resolve_alias("职场焦虑");
    assert!(result.is_some());
    let (token, id) = result.unwrap();
    assert_eq!(token.as_str(), "工作压力");
    assert_eq!(id, 1);
}

/// 查询不存在的别名返回 None
#[test]
fn resolve_nonexistent_alias() {
    let mgr = AliasManager::new();
    assert!(mgr.resolve_alias("不存在的").is_none());
}

/// 循环别名被拒绝
#[test]
fn cyclic_alias_rejected() {
    let mut mgr = AliasManager::new();
    let result = mgr.register_alias("工作压力", 1, "工作压力");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("指向自身"));
}

/// 无效规范词文本被拒绝
#[test]
fn invalid_canonical_rejected() {
    let mut mgr = AliasManager::new();
    let result = mgr.register_alias("别名", 1, "");
    assert!(result.is_err());
}

/// 覆盖已有别名映射
#[test]
fn overwrite_alias() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("焦虑", 1, "工作压力").unwrap();
    mgr.register_alias("焦虑", 2, "职业倦怠").unwrap();
    let result = mgr.resolve_alias_text("焦虑");
    assert_eq!(result, Some("职业倦怠"));
}

/// 注销别名
#[test]
fn unregister_alias() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("a", 1, "b").unwrap();
    assert!(mgr.is_alias("a"));
    assert!(mgr.unregister_alias("a"));
    assert!(!mgr.is_alias("a"));
}

/// 大小写 / 首尾空白不同的 alias 注册与查询命中同一映射。
#[test]
fn alias_entries_normalized_for_case_and_whitespace() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("  Work Stress ", 7, " 工作压力 ")
        .unwrap();

    // 注册与查询均按标准化文本命中
    assert_eq!(mgr.resolve_alias_text("work stress"), Some("工作压力"));
    assert_eq!(mgr.resolve_alias_text("WORK STRESS"), Some("工作压力"));
    assert_eq!(mgr.resolve_alias_text("  work stress  "), Some("工作压力"));
    assert!(mgr.is_alias("Work Stress"));
    assert!(!mgr.is_alias("   "), "非法文本不视为别名");

    let (token, id) = mgr.resolve_alias("WORK STRESS").expect("应命中");
    assert_eq!(token.as_str(), "工作压力");
    assert_eq!(id, 7);

    // 注销同样按标准化命中；再次注销返回 false
    assert!(mgr.unregister_alias(" WORK STRESS "));
    assert!(!mgr.unregister_alias("work stress"));
}

/// 别名 / 规范词文本非法时注册被拒绝；归一后自指也被拒绝。
#[test]
fn invalid_alias_inputs_rejected() {
    let mut mgr = AliasManager::new();
    assert!(mgr.register_alias("", 1, "工作压力").is_err());
    assert!(mgr.register_alias("   ", 1, "工作压力").is_err());
    assert!(
        mgr.register_alias(&"长".repeat(257), 1, "工作压力")
            .is_err()
    );
    assert!(mgr.register_alias("别名", 1, "").is_err());
    assert!(
        mgr.register_alias(" Work Stress ", 1, "work stress")
            .is_err(),
        "归一后相同视为自指"
    );
    assert_eq!(mgr.alias_count(), 0);
}

// ── 规范词 ID 查询 ──

/// 通过文本查询规范词 ID
#[test]
fn canonical_id_lookup() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("a1", 42, "规范词").unwrap();
    assert_eq!(mgr.canonical_id("规范词"), Some(42));
}

/// 不存在的规范词返回 None
#[test]
fn nonexistent_canonical_id() {
    let mgr = AliasManager::new();
    assert!(mgr.canonical_id("不存在").is_none());
}

// ── 使用量与合并建议 ──

/// 加载使用计数
#[test]
fn load_use_counts() {
    let mut mgr = AliasManager::new();
    let mut counts = HashMap::new();
    counts.insert("工作压力".into(), 15u32);
    counts.insert("职场焦虑".into(), 8u32);
    mgr.load_use_counts(counts);
    assert_eq!(mgr.use_count("工作压力"), 15);
    assert_eq!(mgr.use_count("职场焦虑"), 8);
    assert_eq!(mgr.use_count("不存在的"), 0);
}

/// use_counts 键标准化：大小写/空白归一后累加、非法键丢弃、查询同样归一。
#[test]
fn use_counts_normalized_and_accumulated() {
    let mut mgr = AliasManager::new();
    let mut counts = HashMap::new();
    counts.insert("Work Stress".into(), 3u32);
    counts.insert(" work stress ".into(), 4u32);
    counts.insert("   ".into(), 9u32); // 非法 → 丢弃
    mgr.load_use_counts(counts);

    assert_eq!(mgr.use_count("work stress"), 7, "归一后同键累加");
    assert_eq!(mgr.use_count("WORK STRESS"), 7, "查询入参同样归一");
    assert_eq!(mgr.use_count("work  stress"), 0, "不同文本不命中");
    assert_eq!(mgr.use_count(""), 0, "非法查询文本返回 0");

    // 覆盖式加载：再次加载替换旧计数（不残留）
    let mut next = HashMap::new();
    next.insert("Work Stress".into(), 2u32);
    mgr.load_use_counts(next);
    assert_eq!(mgr.use_count("work stress"), 2);
}

/// 无使用计数时合并建议为空
#[test]
fn empty_use_counts_no_suggestions() {
    let mgr = AliasManager::new();
    let suggestions = mgr.suggest_merges(3);
    assert!(suggestions.is_empty());
}

/// 低使用量关键词不产生合并建议
#[test]
fn low_use_no_suggestion() {
    let mut mgr = AliasManager::new();
    let mut counts = HashMap::new();
    counts.insert("a".into(), 1u32);
    counts.insert("b".into(), 2u32);
    mgr.load_use_counts(counts);
    let suggestions = mgr.suggest_merges(5); // 最小阈值 5，所有词低于此
    assert!(suggestions.is_empty());
}

/// 相似关键词产生合并建议
#[test]
fn similar_keywords_get_suggestion() {
    let mut mgr = AliasManager::new();
    let mut counts = HashMap::new();
    counts.insert("工作压力".into(), 20u32);
    counts.insert("工作负担".into(), 10u32);
    mgr.load_use_counts(counts);
    let suggestions = mgr.suggest_merges(3);
    // "工作压力"和"工作负担"共享"工作"+"负"+"担"+"压"+"力"
    // 编辑距离较大但共享中文词组，应能检测相似
    // 由于中文编辑距离按字符计算，共享多个汉字 => 命中条件 1（共享 ≥2 个汉字）
    assert!(!suggestions.is_empty(), "相似关键词应产生合并建议");
    // 建议中应包含使用量高的"工作压力"作为规范词
    assert_eq!(suggestions[0].canonical_text, "工作压力");
}

/// 别名使用量高于规范词时建议反转
#[test]
fn alias_higher_usage_suggests_reverse() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("流行词", 1, "规范词").unwrap();
    let mut counts = HashMap::new();
    counts.insert("流行词".into(), 50u32); // 别名使用量高
    counts.insert("规范词".into(), 3u32); // 规范词使用量低
    mgr.load_use_counts(counts);
    let suggestions = mgr.suggest_merges(3);
    assert!(!suggestions.is_empty());
    // 建议应将"流行词"提为新的规范词
    assert!(suggestions[0].reason.contains("使用量高于当前规范词"));
}

/// 注册规范词：只写反向查询缓存，不建立别名映射；非法文本拒绝。
#[test]
fn register_canonical_registers_id_without_alias() {
    let mut mgr = AliasManager::new();
    mgr.register_canonical("工作压力", 7).unwrap();
    assert_eq!(mgr.canonical_id("工作压力"), Some(7));
    assert_eq!(mgr.canonical_count(), 1);
    assert_eq!(mgr.alias_count(), 0, "不建立别名映射");
    assert!(!mgr.is_alias("工作压力"));

    assert!(mgr.register_canonical("   ", 1).is_err(), "非法文本应拒绝");
    assert!(mgr.register_canonical(&"长".repeat(257), 1).is_err());

    // 注册与查询均按标准化文本命中；重复注册覆盖旧 ID
    mgr.register_canonical(" 工作压力 ", 9).unwrap();
    assert_eq!(mgr.canonical_id("工作压力"), Some(9));
}

/// 已注册规范词不参与合并建议（不会作为建议别名被降级）。
#[test]
fn registered_canonical_skips_suggestion() {
    let mut mgr = AliasManager::new();
    mgr.register_canonical("工作压力", 1).unwrap();
    let mut counts = HashMap::new();
    counts.insert("工作压力".into(), 20u32);
    counts.insert("职场压力".into(), 10u32);
    mgr.load_use_counts(counts);

    let suggestions = mgr.suggest_merges(3);
    assert!(
        suggestions.iter().all(|s| s.alias_text != "工作压力"),
        "已登记规范词不得作为建议别名"
    );
    assert!(suggestions.is_empty(), "规范词被跳过后无剩余配对");
}

// ── 清空缓存 ──

/// clear 重置所有状态
#[test]
fn clear_resets_state() {
    let mut mgr = AliasManager::new();
    mgr.register_alias("a", 1, "b").unwrap();
    let mut counts = HashMap::new();
    counts.insert("c".into(), 5u32);
    mgr.load_use_counts(counts);
    mgr.clear();
    assert_eq!(mgr.alias_count(), 0);
    assert_eq!(mgr.canonical_count(), 0);
    assert_eq!(mgr.use_count("c"), 0);
}

// ── 辅助函数测试 ──

/// levenshtein_distance 各输入参数化验证。
#[test]
fn test_levenshtein_cases() {
    let cases = [
        ("abc", "abc", 0),  // 相同
        ("abc", "abcd", 1), // 插入
        ("abcd", "abc", 1), // 删除
        ("abc", "abd", 1),  // 替换
    ];
    for (a, b, expected) in cases {
        assert_eq!(levenshtein_distance(a, b), expected, "{a} vs {b}");
    }
}

/// is_similar_keyword 各输入参数化验证。
#[test]
fn test_is_similar_cases() {
    let cases = [
        ("same", "same", false),            // 相同词不算相似
        ("工作压力", "工作焦虑", true),     // 共享"工""作"
        ("职场工作压力", "工作压力", true), // 包含关系
        ("abc", "xyz", false),              // 无共同字
    ];
    for (a, b, expected) in cases {
        assert_eq!(is_similar_keyword(a, b), expected, "{a} vs {b}");
    }
}
