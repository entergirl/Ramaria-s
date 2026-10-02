//! crates/ramaria-desktop/src/path_guard/privacy_audit_tests.rs - 桌面日志隐私审计（静态源码扫描）
//!
//! 设计特点:
//! - 断言桌面 crate 的日志不落绝对路径、裸路径变量与用户原文/密钥
//! - 扫描对象: `src/**/*.rs`（递归），以源码文本静态断言，拦截回归；
//!   不依赖运行期日志内容，故不写库、不起 Tauri 应用
//! - 规则 1: 日志宏体内不得出现 `display()`（渲染路径）或裸路径变量
//!   （`%path` / `%file_path` / `%dir` …），路径须经 `redact_path_label`
//!   折叠为"文件名 + 短哈希"
//! - 规则 2: `#[tracing::instrument(...)]` 若未 `skip_all`，其函数签名中命中的
//!   敏感参数（路径/密钥/用户原文语义）必须出现在 `skip(...)` 列表内

use std::path::{Path, PathBuf};

/// 日志宏体内禁止出现的裸路径变量（脱敏标签写法不含这些子串）。
const FORBIDDEN_LOG_VARIABLES: &[&str] = &[
    "%path",
    "%file_path",
    "%real_path",
    "%dir",
    "%output",
    "%db_path",
    "%saved_path",
    "%path_trimmed",
    "%canonical",
    "%real_parent",
    "%config_path",
    "%data_dir",
    "%log_file_path",
];

/// 命中即必须出现在 `instrument` skip 列表中的参数名。
const MUST_SKIP_PARAMS: &[&str] = &[
    "path",
    "file_path",
    "output_path",
    "api_key",
    "base_url",
    "value",
    "config_json",
    "message",
    "request",
    "reaction",
    "avoid",
    "alias",
];

#[test]
fn tracing_macros_do_not_render_paths() {
    for (name, src) in audit_sources() {
        for body in log_macro_bodies(&src) {
            assert!(
                !body.contains("display()"),
                "{name}: 日志宏内出现 display()（绝对路径会随诊断包外发）: {body}"
            );
            for var in FORBIDDEN_LOG_VARIABLES {
                assert!(
                    !contains_bare_variable(&body, var),
                    "{name}: 日志宏内出现裸路径变量 {var}（应改记 redact_path_label）: {body}"
                );
            }
        }
    }
}

#[test]
fn instrument_skips_sensitive_arguments() {
    for (name, src) in audit_sources() {
        for (skip_args, rest) in instrument_attributes(&src) {
            if skip_declares_param(&skip_args, "skip_all") {
                continue;
            }
            for param in function_params(&rest) {
                if MUST_SKIP_PARAMS.iter().any(|p| *p == param)
                    && !skip_declares_param(&skip_args, &param)
                {
                    panic!(
                        "{name}: `#[tracing::instrument]` 未 skip 敏感参数 `{param}`\
                            （会以 INFO 级自动记录入日志，可能含路径/密钥/原文）"
                    );
                }
            }
        }
    }
}

// ── 源码收集 ──

/// 收集 `src/` 下全部 `.rs` 源文件，返回（相对路径, 内容）；行尾统一为 LF。
fn audit_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);

    assert!(!files.is_empty(), "未扫描到任何源文件: {root:?}");

    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("读取源码失败 {path:?}: {e}"));
            let label = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            (label, text.replace("\r\n", "\n"))
        })
        .collect()
}

/// 递归收集目录下全部 `.rs` 文件。
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

// ── 规则 1：日志宏体提取 ──

/// 提取全部日志宏（trace/debug/info/warn/error）的括号体文本。
fn log_macro_bodies(src: &str) -> Vec<String> {
    const MACROS: &[&str] = &["trace!", "debug!", "info!", "warn!", "error!"];

    let mut bodies = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel) = src[cursor..].find("tracing::") {
        let after_prefix = cursor + rel + "tracing::".len();
        let rest = &src[after_prefix..];
        let Some(macro_len) = MACROS
            .iter()
            .find(|m| rest.starts_with(**m))
            .map(|m| m.len())
        else {
            cursor = after_prefix;
            continue;
        };

        let tail = &rest[macro_len..];
        let Some(open_rel) = tail.find('(') else {
            cursor = after_prefix;
            continue;
        };
        match matching_paren(tail, open_rel) {
            Some(close_rel) => {
                bodies.push(tail[open_rel + 1..close_rel].to_string());
                cursor = after_prefix + macro_len + close_rel + 1;
            }
            None => cursor = after_prefix + macro_len,
        }
    }

    bodies
}

/// 判断文本中是否出现"裸变量"引用。
///
/// 说明:
/// - `var` 形如 `%path`；命中后要求其后续字符不是标识符字符，
///   以免把 `%path_guard::redact_path_label(...)` 这类模块路径调用误判为裸变量。
fn contains_bare_variable(text: &str, var: &str) -> bool {
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find(var) {
        let end = cursor + rel + var.len();
        let next_is_ident = text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !next_is_ident {
            return true;
        }
        cursor = end;
    }
    false
}

/// 返回 `s[open..]` 中与 `s[open]` 配对的右括号下标。
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, byte) in s.as_bytes().iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

// ── 规则 2：instrument 属性提取 ──

/// 提取全部 `#[tracing::instrument(...)]` 属性，返回（参数体, 属性之后的源码）。
///
/// 说明:
/// - 无参数形式 `#[tracing::instrument]` 返回空参数体（等价"记录所有参数"，
///   由后续敏感参数检查兜底）。
fn instrument_attributes(src: &str) -> Vec<(String, String)> {
    const ATTR: &str = "#[tracing::instrument";

    let mut out = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel) = src[cursor..].find(ATTR) {
        let start = cursor + rel;
        let after = &src[start + ATTR.len()..];
        let next_paren = after.find('(');
        let next_bracket = after.find(']');

        match (next_paren, next_bracket) {
            // 无参数形式：`]` 先出现
            (Some(open), Some(close_bracket)) if open > close_bracket => {
                out.push((String::new(), after[close_bracket + 1..].to_string()));
                cursor = start + ATTR.len() + close_bracket + 1;
            }
            // 有参数形式：按括号配对取参数体
            (Some(open), _) => {
                let Some(close_rel) = matching_paren(after, open) else {
                    cursor = start + ATTR.len();
                    continue;
                };
                out.push((
                    after[open + 1..close_rel].to_string(),
                    after[close_rel + 1..].to_string(),
                ));
                cursor = start + ATTR.len() + close_rel + 1;
            }
            _ => {
                cursor = start + ATTR.len();
            }
        }
    }

    out
}

/// 判断 skip 参数体中是否声明了指定参数名（按标识符整词比较，避免子串误判）。
fn skip_declares_param(skip_args: &str, param: &str) -> bool {
    skip_args
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|token| token == param)
}

/// 从函数签名文本中提取参数名列表（`fn f(a: T, b: U)` → `["a", "b"]`）。
fn function_params(rest: &str) -> Vec<String> {
    let Some(fn_rel) = rest.find("fn ") else {
        return Vec::new();
    };
    let after_fn = &rest[fn_rel..];
    let Some(open) = after_fn.find('(') else {
        return Vec::new();
    };
    let Some(close) = matching_paren(after_fn, open) else {
        return Vec::new();
    };
    let params = &after_fn[open + 1..close];

    // 按顶层逗号切分（跳过尖括号/括号/方括号内的逗号）
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in params.chars() {
        match ch {
            '<' | '(' | '[' => {
                depth += 1;
                current.push(ch);
            }
            '>' | ')' | ']' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    parts.push(current);

    parts
        .iter()
        .filter_map(|part| {
            let (name, _) = part.split_once(':')?;
            let name = name.trim().trim_start_matches("mut ").trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}
