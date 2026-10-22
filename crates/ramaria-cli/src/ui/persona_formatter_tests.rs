//! crates/ramaria-cli/src/ui/persona_formatter_tests.rs - Ramaria 终端输出模块单元测试
//!
//! 设计特点:
//! - 由 `ui` 以 `#[cfg(test)] mod persona_formatter_tests;` 收纳，经 `use super::*` 取用被测项
//! - 覆盖 PersonaFormatter 的跨 chunk 分隔处理与文本行渲染口径

use super::*;

// ---- 基本替换 ----

#[test]
fn basic_double_pipe_replacement() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("你好||世界"), "你好\n世界");
    assert_eq!(fmt.flush(), None);
}

#[test]
fn multiple_double_pipes() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(
        fmt.feed("那挺好的||摸鱼就摸鱼||聊天也是正事"),
        "那挺好的\n摸鱼就摸鱼\n聊天也是正事"
    );
    assert_eq!(fmt.flush(), None);
}

#[test]
fn no_pipes_passthrough() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("普通文本无分隔符"), "普通文本无分隔符");
    assert_eq!(fmt.flush(), None);
}

#[test]
fn empty_chunk() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed(""), "");
    assert_eq!(fmt.flush(), None);
}

// ---- 跨 chunk 拆分 ----

#[test]
fn cross_chunk_double_pipe() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("你好|"), "你好");
    assert_eq!(fmt.feed("|世界"), "\n世界");
    assert_eq!(fmt.flush(), None);
}

#[test]
fn cross_chunk_multiple() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("A||B|"), "A\nB");
    assert_eq!(fmt.feed("|C"), "\nC");
    assert_eq!(fmt.flush(), None);
}

// ---- 边缘情况 ----

#[test]
fn triple_pipe() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("A|||B"), "A\nB");
    assert_eq!(fmt.flush(), Some("|".to_string()));
}

#[test]
fn quad_pipe() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("A||||B"), "A\n\nB");
    assert_eq!(fmt.flush(), None);
}

#[test]
fn isolated_pipe_at_end_flushed() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("你好|"), "你好");
    assert_eq!(fmt.flush(), Some("|".to_string()));
}

// （原 isolated_pipe_at_end_no_more_chunks 与 isolated_pipe_at_end_flushed
//  行为完全重复，仅文本不同，已删除）

#[test]
fn double_pipe_at_chunk_boundary_with_trailing_text() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("前文|"), "前文");
    assert_eq!(fmt.feed("|后文还有||更多"), "\n后文还有\n更多");
    assert_eq!(fmt.flush(), None);
}

#[test]
fn single_pipe_only() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("|"), "");
    assert_eq!(fmt.flush(), Some("|".to_string()));
}

#[test]
fn pipe_followed_by_non_pipe() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("你好|"), "你好");
    assert_eq!(fmt.feed("世界"), "|世界");
    assert_eq!(fmt.flush(), None);
}

// ---- 空/极短输入 ----

#[test]
fn single_char_non_pipe() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("a"), "a");
    assert_eq!(fmt.flush(), None);
}

#[test]
fn only_double_pipe() {
    let mut fmt = PersonaFormatter::new();
    assert_eq!(fmt.feed("||"), "\n");
    assert_eq!(fmt.flush(), None);
}

// ---- 多次 feed 后正常文本 ----

#[test]
fn many_small_feeds() {
    let mut fmt = PersonaFormatter::new();
    let mut result = String::new();
    for ch in ["那", "挺", "好", "的", "|", "|", "摸", "鱼"].iter() {
        result.push_str(&fmt.feed(ch));
    }
    result.push_str(&fmt.flush().unwrap_or_default());
    assert_eq!(result, "那挺好的\n摸鱼");
}

// ---- 兼容现有 API ----

// （原 write_persona_delta_integration 与 basic_double_pipe_replacement
//  同模式且断言更弱，已删除）

// ---- 文本行渲染（分隔线与标签对齐）----

/// 分隔线宽度与标签列宽固定，供组装完整文本块的命令复用。
#[test]
fn separator_and_labeled_line_render() {
    assert_eq!(separator_line().chars().count(), 60);
    assert!(separator_line().chars().all(|c| c == '-'));

    let line = labeled_line("标题", "事件一");
    assert!(line.starts_with("  标题 "), "应保留缩进与标签: {line:?}");
    let prefix_chars = line.chars().count() - "事件一".chars().count();
    assert_eq!(
        prefix_chars,
        2 + 20 + 1,
        "标签列宽应与 labeled 的对齐口径一致: {line:?}"
    );
}
