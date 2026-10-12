//! crates/ramaria-channel/src/runtime/outbound.rs - Ramaria 通道出站文本净化模块
//!
//! 设计特点:
//! - 多气泡契约分隔符 `||` 替换为换行（仅出站文本；库内原文不改）
//! - 单遍状态机：代码块 / 行内代码 / URL 区间内不替换
//! - 替换吞并紧邻两侧连续横向空白；无分隔符时输出与输入逐字符一致
//! - 纯函数、多字节字符安全、不 panic

// =========================================================
// 出站文本净化
// =========================================================

/// 出站文本兜底：把多气泡契约分隔符 `||` 替换为换行。
///
/// 规则:
/// - `||` → `\n`（`||||` → `\n\n` 自然分组），替换吞掉两侧连续横向空白
///   （空格 / 制表符），避免产生行首行尾空白；
/// - 代码块（``` 围栏之间；未闭合围栏其后全部视为块内）、行内代码
///   （` 成对区间）、URL（`http://` / `https://` 起始到下一个空白之间）
///   区间内不替换；
/// - 空串返回空串；无 `||` 时输出与输入逐字符一致。
///
/// 参数:
/// - `text`: 出站文本（发送前调用；库内消息正文保持原样）。
///
/// 返回:
/// - 净化后的发送文本。
pub fn sanitize_outbound_text(text: &str) -> String {
    if !text.contains("||") {
        return text.to_string();
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
    let mut out = String::with_capacity(text.len());
    let mut state = State::Normal;
    let mut i = 0usize;

    while i < text.len() {
        let rest = &text[i..];
        let Some(ch) = rest.chars().next() else {
            break;
        };
        match state {
            State::Normal => {
                if rest.starts_with("```") {
                    out.push_str("```");
                    i += 3;
                    state = State::CodeFence;
                    continue;
                }
                if rest.starts_with("||") {
                    // 左吞横向空白 → 换行 → 右吞横向空白（`||||` 再次命中同样的替换）
                    trim_trailing_horizontal(&mut out);
                    out.push('\n');
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
                    out.push('`');
                    i += 1;
                    state = State::InlineCode;
                    continue;
                }
                if let Some(len) = url_prefix_len(rest) {
                    out.push_str(&rest[..len]);
                    i += len;
                    state = State::Url;
                    continue;
                }
                out.push(ch);
                i += ch.len_utf8();
            }
            State::CodeFence => {
                if rest.starts_with("```") {
                    out.push_str("```");
                    i += 3;
                    state = State::Normal;
                } else {
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            State::InlineCode => {
                out.push(ch);
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
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// 吞掉输出缓冲尾部的连续横向空白（空格 / 制表符）。
fn trim_trailing_horizontal(out: &mut String) {
    while let Some(last) = out.chars().next_back() {
        if last == ' ' || last == '\t' {
            out.pop();
        } else {
            break;
        }
    }
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
