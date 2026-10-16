//! crates/ramaria-channel/src/runtime/outbound/tests.rs - 出站文本拆条单元测试
//!
//! 设计特点:
//! - 样例驱动：基本切分 / 空白吞并 / 豁免区间 / 未切分零改动逐项覆盖
//! - 断言以逐字符等价为准（未切分时输出与输入一致；切分后逐段断言）
//! - fold 折叠：界限内原样 / 超限并入末段 / 下界与空输入
//! - 表情指令：剥离 / 关键词取首个非空 / 未闭合与非标记按原文保留

use super::*;

/// 基本切分：单组 `||` 两段；连续 `||||` 不产生空段。
#[test]
fn splits_pipe_pairs() {
    assert_eq!(split_outbound_segments("你好||在的"), vec!["你好", "在的"]);
    assert_eq!(
        split_outbound_segments("甲||乙||丙"),
        vec!["甲", "乙", "丙"]
    );
    assert_eq!(split_outbound_segments("上||||下"), vec!["上", "下"]);
}

/// 空白吞并：段边界吞掉两侧连续空格 / 制表符。
#[test]
fn swallows_surrounding_horizontal_whitespace() {
    assert_eq!(
        split_outbound_segments("你好 || 在的"),
        vec!["你好", "在的"]
    );
    assert_eq!(split_outbound_segments("甲 \t||\t 乙"), vec!["甲", "乙"]);
    assert_eq!(split_outbound_segments("  甲  || 乙  "), vec!["甲", "乙"]);
}

/// 分隔符位于文本首尾：首尾空段丢弃，只留正文段。
#[test]
fn separator_at_edges() {
    assert_eq!(split_outbound_segments("||开头"), vec!["开头"]);
    assert_eq!(split_outbound_segments("结尾 ||"), vec!["结尾"]);
}

/// 纯分隔符 / 空白：全部段为空 → 空列表（调用方跳过发送）。
#[test]
fn pure_separator_or_whitespace_is_empty() {
    assert!(split_outbound_segments("||||").is_empty());
    assert!(split_outbound_segments("  ||  ").is_empty());
}

/// 未切分零改动：内容与输入逐字符一致（不做 trim），含空串与单竖线。
#[test]
fn zero_change_without_split() {
    for text in [
        "",
        "普通文本",
        "含 `代码` 与\n换行",
        "https://example.com/a b",
        "```\n块\n```",
        "只有一个 | 竖线",
    ] {
        assert_eq!(
            split_outbound_segments(text),
            vec![text.to_string()],
            "输入: {text:?}"
        );
    }
}

/// 代码块豁免：围栏之间的 `||` 不切分，整体单段原样返回。
#[test]
fn code_fence_is_exempt() {
    let text = "看这段:\n```\na || b\n```\n结束";
    assert_eq!(split_outbound_segments(text), vec![text.to_string()]);
}

/// 未闭合围栏：围栏前的 `||` 切分，围栏内（含）其后全部不切分。
#[test]
fn unclosed_fence_exempts_rest() {
    assert_eq!(
        split_outbound_segments("开头 || 换\n```\n内部 || 不换"),
        vec!["开头", "换\n```\n内部 || 不换"]
    );
}

/// 行内代码豁免：反引号成对区间内不切分。
#[test]
fn inline_code_is_exempt() {
    assert_eq!(
        split_outbound_segments("用 `a || b` 连接||好"),
        vec!["用 `a || b` 连接", "好"]
    );
}

/// URL 豁免：链接起始到下一个空白之间不切分（含大小写不敏感 scheme）。
#[test]
fn url_is_exempt() {
    assert_eq!(
        split_outbound_segments("看 https://a.com/x||y 这里||好"),
        vec!["看 https://a.com/x||y 这里", "好"]
    );
    let text = "HTTP://a.com||b 尾";
    assert_eq!(split_outbound_segments(text), vec![text.to_string()]);
}

/// 混排中文与标点：逐段输出、多字节安全。
#[test]
fn mixed_chinese_multibyte_safe() {
    assert_eq!(
        split_outbound_segments("（笑）真的吗？||换个话题——好吧||嗯"),
        vec!["（笑）真的吗？", "换个话题——好吧", "嗯"]
    );
}

/// 单竖线与三竖线：单竖线不切分；三竖线按一对切分 + 一个保留。
#[test]
fn single_and_triple_pipe() {
    assert_eq!(split_outbound_segments("a|||b"), vec!["a", "|b"]);
    assert_eq!(split_outbound_segments("a | b"), vec!["a | b"]);
}

/// 段内换行保留：仅段首尾 trim，段内换行不动。
#[test]
fn newline_inside_segment_preserved() {
    assert_eq!(split_outbound_segments("甲\n乙||丙"), vec!["甲\n乙", "丙"]);
}

/// fold 界限内：段数不超过上限时原样返回。
#[test]
fn fold_returns_unchanged_when_within_limit() {
    let segments = vec!["甲".to_string(), "乙".to_string()];
    assert_eq!(fold_segment_overflow(segments.clone(), 5), segments);
    assert_eq!(fold_segment_overflow(segments.clone(), 2), segments);
}

/// fold 上限 1：全部段以换行合并为单段。
#[test]
fn fold_merges_all_when_max_one() {
    let segments = vec!["甲".to_string(), "乙".to_string(), "丙".to_string()];
    assert_eq!(fold_segment_overflow(segments, 1), vec!["甲\n乙\n丙"]);
}

/// fold 超限：保留前 max-1 段，其余并入末段。
#[test]
fn fold_merges_overflow_into_last() {
    let segments = vec!["甲".to_string(), "乙".to_string(), "丙".to_string()];
    assert_eq!(fold_segment_overflow(segments, 2), vec!["甲", "乙\n丙"]);
}

/// fold 下界：上限 0 按 1 处理。
#[test]
fn fold_treats_zero_as_one() {
    let segments = vec!["甲".to_string(), "乙".to_string()];
    assert_eq!(fold_segment_overflow(segments, 0), vec!["甲\n乙"]);
}

/// fold 空输入：任意上限下均返回空列表。
#[test]
fn fold_empty_input() {
    assert!(fold_segment_overflow(Vec::new(), 5).is_empty());
    assert!(fold_segment_overflow(Vec::new(), 0).is_empty());
}

// =========================================================
// 表情指令解析
// =========================================================

/// 无标记：文本逐字符一致，关键词为空。
#[test]
fn sticker_no_directive_is_identity() {
    let (text, keyword) = parse_sticker_directive("你好呀，今天天气不错");
    assert_eq!(text, "你好呀，今天天气不错");
    assert_eq!(keyword, None);
}

/// 半角冒号单标记：标记剥离，关键词取出。
#[test]
fn sticker_halfwidth_directive_stripped() {
    let (text, keyword) = parse_sticker_directive("送你[表情:喵喵]到家");
    assert_eq!(text, "送你到家");
    assert_eq!(keyword.as_deref(), Some("喵喵"));
}

/// 全角冒号同样识别。
#[test]
fn sticker_fullwidth_colon_recognized() {
    let (text, keyword) = parse_sticker_directive("开心[表情：开心]呀");
    assert_eq!(text, "开心呀");
    assert_eq!(keyword.as_deref(), Some("开心"));
}

/// 关键词两侧空白 trim。
#[test]
fn sticker_keyword_trimmed() {
    let (text, keyword) = parse_sticker_directive("[表情:  微笑  ]好");
    assert_eq!(text, "好");
    assert_eq!(keyword.as_deref(), Some("微笑"));
}

/// 空关键词：标记仍剥离但不产出关键词（半 / 全角冒号）。
#[test]
fn sticker_empty_keyword_stripped_only() {
    let (text, keyword) = parse_sticker_directive("哈哈[表情:]");
    assert_eq!(text, "哈哈");
    assert_eq!(keyword, None);

    let (text, keyword) = parse_sticker_directive("哈哈[表情：  ]");
    assert_eq!(text, "哈哈");
    assert_eq!(keyword, None);
}

/// 多标记：全部剥离，关键词取首个非空。
#[test]
fn sticker_multiple_takes_first_non_empty() {
    let (text, keyword) = parse_sticker_directive("[表情:][表情:猫][表情:狗]你好");
    assert_eq!(text, "你好");
    assert_eq!(keyword.as_deref(), Some("猫"));
}

/// 未闭合标记：该处按原文保留（不剥离）。
#[test]
fn sticker_unclosed_preserved() {
    let (text, keyword) = parse_sticker_directive("打[表情:喵");
    assert_eq!(text, "打[表情:喵");
    assert_eq!(keyword, None);
}

/// 前缀后无冒号：按原文保留（非标记形态）。
#[test]
fn sticker_without_colon_preserved() {
    let (text, keyword) = parse_sticker_directive("[表情包]来了");
    assert_eq!(text, "[表情包]来了");
    assert_eq!(keyword, None);
}

/// 与 `||` 共存：表情标记剥离，分隔符原样保留（交拆条模块处理）。
#[test]
fn sticker_coexists_with_pipe_separator() {
    let (text, keyword) = parse_sticker_directive("[表情:猫]||你好呀");
    assert_eq!(text, "||你好呀");
    assert_eq!(keyword.as_deref(), Some("猫"));
}

/// 中文与 emoji 混排：剥离不破坏多字节字符。
#[test]
fn sticker_mixed_multibyte_preserved() {
    let (text, keyword) = parse_sticker_directive("🎉[表情： 猫 ]好🐱");
    assert_eq!(text, "🎉好🐱");
    assert_eq!(keyword.as_deref(), Some("猫"));
}

/// 仅标记：剥离后为空文本，关键词仍产出。
#[test]
fn sticker_only_marker_yields_empty_text() {
    let (text, keyword) = parse_sticker_directive("[表情:猫]");
    assert_eq!(text, "");
    assert_eq!(keyword.as_deref(), Some("猫"));
}
