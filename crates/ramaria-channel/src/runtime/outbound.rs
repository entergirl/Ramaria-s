//! crates/ramaria-channel/src/runtime/outbound.rs - Ramaria 通道出站文本拆条模块
//!
//! 设计特点:
//! - 多气泡契约分隔符 `||` 切分为待分次发送的段列表（仅出站文本；库内原文不改）
//! - 单遍状态机：代码块 / 行内代码 / URL 区间内不切分
//! - 切段吞并紧邻两侧连续横向空白；逐段首尾 trim 后丢弃空段；未切分时输出与输入逐字符一致
//! - 段数上限折叠：超出上限的余段以换行并入末段
//! - 纯函数、多字节字符安全、不 panic

// =========================================================
// 出站文本拆条
// =========================================================

/// 出站文本拆条：把多气泡契约分隔符 `||` 切分为待分次发送的段列表。
///
/// 规则:
/// - 不含 `||`（或全部 `||` 位于豁免区间内）→ 返回单元素列表（内容与输入
///   逐字符一致，不做 trim）；
/// - 命中 `||`（豁免区间外）→ 在该处切段（吞并两侧连续横向空白：空格 /
///   制表符），逐段收集后对每段首尾 trim（含换行），丢弃 trim 后的空段；
/// - 全部段为空（正文仅分隔符与空白）→ 返回空列表（调用方跳过发送）；
/// - 豁免区间（`||` 不切分）：代码块（``` 围栏之间；未闭合围栏其后全部视作
///   块内）、行内代码（` 成对区间）、URL（`http://` / `https://` 起始到下一
///   空白之间，scheme 大小写不敏感）；
/// - 多字节字符安全、不 panic。
///
/// 参数:
/// - `text`: 出站文本（发送前调用；库内消息正文保持原样）。
///
/// 返回:
/// - 待分次发送的段列表（无切分时单元素；全空时段列表为空）。
pub fn split_outbound_segments(text: &str) -> Vec<String> {
    if !text.contains("||") {
        return vec![text.to_string()];
    }

    /// 扫描状态（Normal 之外均为豁免区间）。
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Normal,
        CodeFence,
        InlineCode,
        Url,
    }

    let bytes = text.as_bytes();
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::with_capacity(text.len());
    let mut state = State::Normal;
    let mut i = 0usize;
    let mut split_seen = false;

    while i < text.len() {
        let rest = &text[i..];
        let Some(ch) = rest.chars().next() else {
            break;
        };
        match state {
            State::Normal => {
                if rest.starts_with("```") {
                    current.push_str("```");
                    i += 3;
                    state = State::CodeFence;
                    continue;
                }
                if rest.starts_with("||") {
                    // 收尾当前段 → 右吞横向空白 → 开始下一段
                    push_segment(&mut segments, &mut current);
                    split_seen = true;
                    i += 2;
                    while i < text.len() {
                        let byte = bytes[i];
                        if byte == b' ' || byte == b'\t' {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    continue;
                }
                if ch == '`' {
                    current.push('`');
                    i += 1;
                    state = State::InlineCode;
                    continue;
                }
                if let Some(len) = url_prefix_len(rest) {
                    current.push_str(&rest[..len]);
                    i += len;
                    state = State::Url;
                    continue;
                }
                current.push(ch);
                i += ch.len_utf8();
            }
            State::CodeFence => {
                if rest.starts_with("```") {
                    current.push_str("```");
                    i += 3;
                    state = State::Normal;
                } else {
                    current.push(ch);
                    i += ch.len_utf8();
                }
            }
            State::InlineCode => {
                current.push(ch);
                i += ch.len_utf8();
                if ch == '`' {
                    state = State::Normal;
                }
            }
            State::Url => {
                if ch.is_whitespace() {
                    // 空白按 Normal 口径重新处理（不消耗该字符）
                    state = State::Normal;
                    continue;
                }
                current.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    if !split_seen {
        // 含 `||` 但全部落在豁免区间：与无分隔符等价（逐字符一致，不 trim）
        return vec![text.to_string()];
    }
    push_segment(&mut segments, &mut current);
    segments
}

/// 段数上限折叠：超过 `max_segments` 时把余段以 `\n` 连接并入末段。
///
/// 规则:
/// - `max_segments` 下界按 1 处理；
/// - `segments.len() <= max_segments` → 原样返回；
/// - 超出 → 保留前 `max_segments - 1` 段，其余（含第 max_segments 段起的全部
///   内容）以 `\n` 连接为最后一段（保证总段数 = max_segments）；
/// - 空输入 → 空输出。
///
/// 参数:
/// - `segments`: 拆条结果（按发送顺序）。
/// - `max_segments`: 单条回复最大分条数。
///
/// 返回:
/// - 折叠后的段列表（长度不超过 max_segments）。
pub fn fold_segment_overflow(segments: Vec<String>, max_segments: usize) -> Vec<String> {
    let max_segments = max_segments.max(1);
    if segments.len() <= max_segments {
        return segments;
    }
    let mut folded: Vec<String> = segments[..max_segments - 1].to_vec();
    folded.push(segments[max_segments - 1..].join("\n"));
    folded
}

/// 收尾一段：首尾 trim 后非空则入列，缓冲清空。
fn push_segment(segments: &mut Vec<String>, buffer: &mut String) {
    let segment = buffer.trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
    buffer.clear();
}

/// URL 起始检测：`http://` / `https://`（scheme 大小写不敏感）返回前缀字节长度。
fn url_prefix_len(rest: &str) -> Option<usize> {
    const HTTPS: &[u8] = b"https://";
    const HTTP: &[u8] = b"http://";
    let bytes = rest.as_bytes();
    if bytes.len() >= HTTPS.len() && bytes[..HTTPS.len()].eq_ignore_ascii_case(HTTPS) {
        return Some(HTTPS.len());
    }
    if bytes.len() >= HTTP.len() && bytes[..HTTP.len()].eq_ignore_ascii_case(HTTP) {
        return Some(HTTP.len());
    }
    None
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
