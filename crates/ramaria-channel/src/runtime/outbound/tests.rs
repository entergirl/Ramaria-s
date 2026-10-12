//! crates/ramaria-channel/src/runtime/outbound/tests.rs - 出站文本净化单元测试
//!
//! 设计特点:
//! - 样例驱动：基本替换 / 空白吞并 / 豁免区间 / 零改动等价逐项覆盖
//! - 断言以逐字符等价为准（无分隔符输入输出一致）

use super::*;

/// 基本替换：单组 `||` 换行；`||||` 自然分组为两个换行。
#[test]
fn replaces_pipe_pairs_with_newline() {
    assert_eq!(sanitize_outbound_text("你好||在的"), "你好\n在的");
    assert_eq!(sanitize_outbound_text("甲||乙||丙"), "甲\n乙\n丙");
    assert_eq!(sanitize_outbound_text("上||||下"), "上\n\n下");
}

/// 空白吞并：替换吞掉两侧连续空格 / 制表符，不留行首行尾空白。
#[test]
fn swallows_surrounding_horizontal_whitespace() {
    assert_eq!(sanitize_outbound_text("你好 || 在的"), "你好\n在的");
    assert_eq!(sanitize_outbound_text("甲 \t||\t 乙"), "甲\n乙");
    assert_eq!(
        sanitize_outbound_text("  甲  || 乙  "),
        "  甲\n乙  ",
        "仅吞并紧邻分隔符的空白"
    );
}

/// 换行不参与吞并：`||` 紧邻的换行保留（吞并仅针对空格 / 制表符）。
#[test]
fn newlines_around_separator_untouched() {
    assert_eq!(
        sanitize_outbound_text("第一行\n||第二行"),
        "第一行\n\n第二行"
    );
    assert_eq!(sanitize_outbound_text("甲\n乙||丙"), "甲\n乙\n丙");
}

/// 代码块豁免：围栏之间的 `||` 不替换，围栏原样保留。
#[test]
fn code_fence_is_exempt() {
    let text = "看这段:\n```\na || b\n```\n结束";
    assert_eq!(sanitize_outbound_text(text), text);
}

/// 未闭合围栏：其后全部视为代码块内，不替换。
#[test]
fn unclosed_fence_exempts_rest() {
    let text = "开头 || 换\n```\n内部 || 不换";
    assert_eq!(sanitize_outbound_text(text), "开头\n换\n```\n内部 || 不换");
}

/// 行内代码豁免：反引号成对区间内不替换。
#[test]
fn inline_code_is_exempt() {
    assert_eq!(
        sanitize_outbound_text("用 `a || b` 连接||好"),
        "用 `a || b` 连接\n好"
    );
}

/// URL 豁免：链接起始到下一个空白之间不替换（含大小写不敏感 scheme）。
#[test]
fn url_is_exempt() {
    assert_eq!(
        sanitize_outbound_text("看 https://a.com/x||y 这里||好"),
        "看 https://a.com/x||y 这里\n好"
    );
    assert_eq!(
        sanitize_outbound_text("HTTP://a.com||b 尾"),
        "HTTP://a.com||b 尾"
    );
}

/// 混排中文与标点：逐字符等价，多字节安全。
#[test]
fn mixed_chinese_multibyte_safe() {
    assert_eq!(
        sanitize_outbound_text("（笑）真的吗？||换个话题——好吧||嗯"),
        "（笑）真的吗？\n换个话题——好吧\n嗯"
    );
}

/// 无分隔符零改动：与输入逐字符一致（含反引号 / URL / 换行）。
#[test]
fn zero_change_without_separator() {
    for text in [
        "",
        "普通文本",
        "含 `代码` 与\n换行",
        "https://example.com/a b",
        "```\n块\n```",
        "只有一个 | 竖线",
    ] {
        assert_eq!(sanitize_outbound_text(text), text, "输入: {text:?}");
    }
}

/// 分隔符位于文本首尾：吞并后不带多余空白，尾部只留换行。
#[test]
fn separator_at_edges() {
    assert_eq!(sanitize_outbound_text("||开头"), "\n开头");
    assert_eq!(sanitize_outbound_text("结尾 ||"), "结尾\n");
}

/// 单竖线与三竖线：单竖线不动；三竖线按一对替换 + 一个保留。
#[test]
fn single_and_triple_pipe() {
    assert_eq!(sanitize_outbound_text("a | b"), "a | b");
    assert_eq!(sanitize_outbound_text("a|||b"), "a\n|b");
}
