//! crates/ramaria-desktop/src/commands/import_cmd/tests.rs - Ramaria 导入命令隐私回归断言
//!
//! 断言导入命令各文件的日志不直接落 QQ 号/昵称明文。
//!
//! 说明:
//! - 以源码文本静态断言，拦截"把 persona_uid/persona_name/self_id 等原值
//!   直接塞回 tracing 字段"的回归；不误伤已包 `mask_id` 的脱敏写法。
//! - 通过 `env!("CARGO_MANIFEST_DIR")` 定位本模块，不依赖测试运行时目录。
//! - 扫描范围为本模块入口与全部子模块源文件，防止日志写法迁移到子模块后漏检。

/// 读取本模块全部源码（入口 + 子模块），并统一行尾为 LF。
///
/// 说明:
/// - Windows 工作区可能以 CRLF 检出；统一为 LF 可避免行尾差异造成静态匹配假失败。
fn self_source() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    let mut paths = vec![root.join("import_cmd.rs")];
    let module_dir = root.join("import_cmd");
    let entries = std::fs::read_dir(&module_dir).unwrap_or_else(|e| {
        panic!("无法读取导入命令模块目录用于隐私断言: {module_dir:?} 错误: {e}")
    });
    for entry in entries {
        let path = entry.expect("读取导入命令模块目录项失败").path();
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut source = String::new();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("无法读取源文件用于隐私断言: {path:?} 错误: {e}"));
        source.push_str(&text.replace("\r\n", "\n"));
        source.push('\n');
    }
    source
}

/// 取 `s` 在 `start` 起至多 `max_chars` 个字符的子串（保证不切开多字节 UTF-8）。
fn take_chars(s: &str, start: usize, max_chars: usize) -> String {
    s[start..].chars().take(max_chars).collect()
}

/// 禁止出现的"字段直接 = 裸原值"日志形态（含其后的分隔逗号），
/// 若被重新引入即泄露 QQ 号/昵称明文。
///
/// 字段名与值的 sigil/名称分开存储，运行时拼接，避免字面量出现在本文件
/// 源码中而干扰对"目标文件是否含原值日志"的静态判定。
const FORBIDDEN_FIELD_VALUE_PAIRS: &[(&str, char, &str)] = &[
    ("persona_uid", '%', "resolved"),
    ("persona_uid", '%', "self_uid"),
    ("persona_uid", '%', "other_uid"),
    ("persona_name", '%', "self_name"),
    ("persona_name", '%', "other_name"),
    ("self_uid", '%', "self_uid"),
    ("other_uid", '%', "other_uid"),
    ("other_uid", '%', "other_default_uid"),
    ("self_name", '%', "self_name"),
    ("other_name", '%', "other_name"),
    ("self_id", '%', "report.self_id"),
    ("other_ref_id", '?', "other_ref_id"),
    ("self_uid", '%', "uid"),
    ("other_uid", '%', "uid"),
    ("self_persona", '?', "self_persona_uid_resolved"),
    ("other_persona", '?', "other_persona_uid_resolved"),
];

/// import_qq_chat 入口 span 必须 skip 的人参（否则 instrument 在 info 级
/// 自动记录 nickname / 显式 QQ UID 参数）。
const MUST_SKIP_ARGS: &[&str] = &[
    "persona_name",
    "self_persona_uid",
    "other_persona_name",
    "other_persona_uid",
];

/// 禁止出现的"路径原值"日志形态。
///
/// 字段名前缀与值表达式后缀分开存储、运行时拼接，避免完整字面量出现在
/// 本文件源码中而干扰对"目标文件是否含路径原值日志"的静态判定。
const FORBIDDEN_PATH_LOG_FRAGMENTS: &[(&str, &str)] =
    &[("= %real_path", ".display()"), ("= %file_path", ",")];

#[test]
fn no_raw_personal_field_in_logs() {
    let src = self_source();
    for &(field, sigil, value) in FORBIDDEN_FIELD_VALUE_PAIRS {
        let forbidden = format!("{field} = {sigil}{value},");
        assert!(
            !src.contains(&forbidden),
            "检测到日志把个人标识原值直接写入 tracing 字段（禁止形态 {forbidden:?}）——QQ 号/昵称不得明文落日志，须经 mask_id"
        );
    }
}

#[test]
fn no_raw_path_in_logs() {
    let src = self_source();
    for &(prefix, suffix) in FORBIDDEN_PATH_LOG_FRAGMENTS {
        let forbidden = format!("{prefix}{suffix}");
        assert!(
            !src.contains(&forbidden),
            "检测到日志把绝对路径原值直接写入 tracing 字段（禁止形态 {forbidden:?}）——路径须经 redact_path_label 脱敏"
        );
    }
}

#[test]
fn instrument_skips_personal_args() {
    let src = self_source();
    // 以 import_qq_chat 函数签名定位其正上方的 #[tracing::instrument(skip(...))]
    // 属性块：在签名前 400 字符内回溯最近一次 "instrument(skip(" 的起点即该
    // skip 列表。与首参顺序/缩进/行尾无关，避免 rustfmt 或 CRLF 造成静态匹配假失败。
    let fn_start = src
        .find("pub async fn import_qq_chat(")
        .expect("本文件必须包含 import_qq_chat 定义（用于定位其 instrument skip 块）");
    let probe_start = fn_start.saturating_sub(400);
    let skip_start = src[probe_start..fn_start]
        .rfind("instrument(skip(")
        .map(|off| probe_start + off)
        .unwrap_or_else(|| panic!("未找到 import_qq_chat 的 instrument skip 块"));
    // 按字符取 skip 块文本（前 ~240 字符已覆盖整个 skip(...) 列表），避免字节切片切多字节
    let skip_block = take_chars(&src, skip_start, 240);
    for arg in MUST_SKIP_ARGS {
        assert!(
            skip_block.contains(arg),
            "instrument skip 块必须包含 {arg}，否则该参数会在 info 级被自动记录（泄露昵称/QQ UID）"
        );
    }
}
