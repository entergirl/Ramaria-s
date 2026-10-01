//! crates/ramaria-service/src/diagnostics.rs - 诊断信息导出用例
//!
//! 设计特点:
//! - 收集：日志(最近1000行)、配置(API key 脱敏)、数据库 schema 版本、系统信息。
//! - 打包为 .zip 文件供用户手动发送给开发者排查问题。
//! - 所有敏感信息（API key）在收集阶段即脱敏，写入 zip 前已安全。
//! - 先写同目录临时文件，成功后 `fs::rename` 原子替换目标，避免中断留下半成品覆盖旧文件。
//! - 收集阶段错误不阻塞导出：缺失项记录占位文本而非报错退出。
//!
//! 安全约束:
//! - API key 脱敏使用 `[REDACTED]` 替换，不可逆。
//! - 不收集用户对话内容、记忆数据等隐私信息。
//! - 打包前对日志与配置做**二次脱敏**：绝对路径 → 仅保留文件名；消息类字段
//!   的值（preview/content/message/...）→ 字符数占位（`<N chars>`），
//!   杜绝原文片段随诊断包离开本机。
//! - 输出路径的安全性由调用方保证（入口层的路径防护策略不属本层职责）。

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::engine::Engine;

// =========================================================
// 类型定义
// =========================================================

/// 诊断导出请求。
///
/// 字段约定:
/// - `output_path`: 输出 .zip 文件的绝对路径（由调用方通过文件对话框或 CLI 参数指定）。
/// - `schema_version`: 数据库 schema 版本号字符串（来自存储层 `schema_meta`）。
#[derive(Debug, Clone)]
pub struct DiagnosticsRequest {
    /// 输出的 .zip 文件绝对路径
    pub output_path: PathBuf,
    /// 数据库 schema 版本号字符串
    pub schema_version: String,
}

/// 诊断导出结果。
///
/// 包含导出文件的绝对路径和各组件的成功/失败状态。
#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    /// 输出的 .zip 文件绝对路径
    pub output_path: PathBuf,
    /// 各收集步骤的状态
    pub collection_status: HashMap<String, String>,
    /// 文件大小（字节）
    pub file_size_bytes: u64,
}

/// 系统信息快照。
#[derive(Debug, Clone)]
struct SystemInfo {
    /// 操作系统（如 "windows"）
    pub os: String,
    /// CPU 架构（如 "x86_64"）
    pub arch: String,
    /// 操作系统家族（如 "windows"）
    pub family: String,
    /// 应用版本
    pub app_version: String,
    /// 数据库 schema 版本
    pub schema_version: String,
    /// 当前时间 ISO 8601
    pub collected_at: String,
}

// =========================================================
// 公开 API
// =========================================================

/// 导出诊断信息，打包为 .zip 文件。
///
/// 用法:
/// - 入口层在用户手动导出诊断信息时调用（桌面设置页 / CLI 子命令）。
///
/// 参数:
/// - `engine`: 服务层引擎（配置快照提供日志目录与配置目录）。
/// - `req`: 导出请求（输出路径 + 数据库 schema 版本号）。
///
/// 返回:
/// - `DiagnosticsReport`，含输出路径、各步骤状态和文件大小。
///
/// 导出内容:
/// - `ramaria.log`: 最近最多 1000 行日志内容（从日志文件读取）。
/// - `config.toml`: 当前配置文件内容（API key 已脱敏为 `[REDACTED]`）。
/// - `system.txt`: OS / 架构 / 版本 / schema 版本 / 采集时间。
///
/// 报告字段:
/// - `collection_status` 含各收集步骤状态；其中 `index_build` 取值为
///   `ok` / `failed: <脱敏原因>` / `not_built`（检索索引构建状态，供失败诊断）。
///
/// 安全约束:
/// - API key 在收集阶段即脱敏，写入前已不可逆。
/// - 日志与配置在打包前经 [`redact_for_export`] 二次脱敏：绝对路径只留文件名，
///   消息类字段值只留字符数（不落原文）。
/// - 输出路径的安全性由调用方保证（本层不做路径白名单校验）。
///
/// 示例:
/// ```ignore
/// let report = engine.export_diagnostics(DiagnosticsRequest {
///     output_path: PathBuf::from("C:/Users/me/Desktop/ramaria-diagnostics.zip"),
///     schema_version: "1".to_string(),
/// }).await?;
/// ```
pub(crate) async fn export(
    engine: &Engine,
    req: DiagnosticsRequest,
) -> RamariaResult<DiagnosticsReport> {
    // 配置快照在锁内克隆后释放锁，收集与打包全程在锁外进行
    let config = engine.config();
    let mut status = HashMap::new();

    // 1. 收集系统信息（纯内存操作，不会失败）
    let system_info = collect_system_info(&req.schema_version);

    // 2. 收集日志（最近 1000 行）
    let logs = collect_logs(config.as_ref(), &mut status);

    // 3. 收集配置（API key 脱敏）
    let config_content = collect_config(config.as_ref(), &mut status);

    // 4. 收集检索索引构建状态（最近失败原因 / 已构建 / 未构建）
    collect_index_build_status(engine, &mut status);

    // 5. 打包为 zip
    let output_path = req.output_path;
    let file_size = build_zip(&output_path, &system_info, &logs, &config_content)
        .map_err(|e| RamariaError::io(format!("生成诊断 zip 文件失败: {e}"), None))?;

    status.insert("zip".to_string(), "ok".to_string());

    tracing::info!(
        file = %path_log_label(&output_path),
        size = file_size,
        "诊断信息导出完成"
    );

    Ok(DiagnosticsReport {
        output_path,
        collection_status: status,
        file_size_bytes: file_size,
    })
}

// =========================================================
// 内部实现: 收集
// =========================================================

/// 收集系统信息快照。
///
/// 收集内容:
/// - OS / Arch / Family: 来自 `std::env::consts`。
/// - 应用版本: 来自 `env!("CARGO_PKG_VERSION")`。
/// - Schema 版本: 从 DB 的 `schema_meta` 表读取（由调用方传入）。
/// - 采集时间: UTC ISO 8601。
fn collect_system_info(schema_version: &str) -> SystemInfo {
    let collected_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    SystemInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        family: std::env::consts::FAMILY.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: schema_version.to_string(),
        collected_at,
    }
}

/// 收集最近 1000 行日志。
///
/// 行为:
/// - 从 `{config.paths.log_dir}/ramaria.log` 读取。
/// - 日志目录为空或文件不存在时，返回占位文本。
/// - 读取失败不阻塞导出，记录在 `status` 中。
///
/// 返回:
/// - 日志文本内容（最多 1000 行）。失败时返回说明性占位文本。
fn collect_logs(config: &RamariaConfig, status: &mut HashMap<String, String>) -> String {
    let log_dir = &config.paths.log_dir;
    if log_dir.is_empty() {
        status.insert("logs".to_string(), "skipped: 日志目录未配置".to_string());
        tracing::debug!("日志目录未配置，跳过日志收集");
        return String::from("# 日志目录未配置，无法收集日志。\n");
    }

    let log_path = PathBuf::from(log_dir).join("ramaria.log");

    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            // 截取最后 1000 行
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let start = total.saturating_sub(1000);

            let truncated: String = lines[start..].iter().map(|l| format!("{l}\n")).collect();
            // 二次脱敏：绝对路径 → 文件名；消息类字段值 → 字符数（原文不出端）
            let redacted = redact_for_export(&truncated);

            status.insert(
                "logs".to_string(),
                format!("ok: {}/{} lines", redacted.lines().count(), total),
            );
            tracing::debug!(
                total_lines = total,
                collected = redacted.lines().count(),
                "日志收集完成（已二次脱敏）"
            );

            if start > 0 {
                format!("# 最近 1000 行日志（共 {total} 行，已截断前 {start} 行）\n\n{redacted}")
            } else {
                format!("# 全部日志（共 {total} 行）\n\n{redacted}")
            }
        }
        Err(e) => {
            // 占位文本随诊断包外发：路径只写文件名（与成功内容的导出前脱敏口径一致）
            let msg = format!(
                "# 无法读取日志文件 ({}): {}\n",
                path_log_label(&log_path),
                e
            );
            status.insert("logs".to_string(), format!("error: {e}"));
            tracing::warn!(file = %path_log_label(&log_path), error = %e, "日志收集失败");
            msg
        }
    }
}

/// 收集配置文件内容（API key 已脱敏）。
///
/// 行为:
/// - 从 `{config.paths.config_dir}/config.toml` 读取。
/// - 对每一行做脱敏：匹配 `api_key` 模式的行替换值为 `[REDACTED]`。
/// - 文件不存在时返回占位文本。
///
/// 脱敏策略:
/// - 匹配包含 `api_key` 或 `apikey`（不区分大小写）的赋值行。
/// - 将 `=` 右侧的值替换为 `"[REDACTED]"`。
/// - 对于 keychain 中存储的 key，config.toml 中本不应包含，此处做防御性脱敏。
///
/// 返回:
/// - 脱敏后的配置文件文本。
fn collect_config(config: &RamariaConfig, status: &mut HashMap<String, String>) -> String {
    let config_dir = &config.paths.config_dir;
    if config_dir.is_empty() {
        status.insert("config".to_string(), "skipped: 配置目录未配置".to_string());
        tracing::debug!("配置目录未配置，跳过配置收集");
        return String::from("# 配置目录未配置，无法收集配置文件。\n");
    }

    let config_path = PathBuf::from(config_dir).join("config.toml");

    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            // 两道脱敏：API key → [REDACTED]；绝对路径 → 文件名（paths 组可能含本机路径）
            let redacted = redact_for_export(&redact_api_keys(&content));
            // 包头只写文件名，不写绝对路径（诊断包外发时不暴露本机目录结构）
            let file_label = config_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "config.toml".to_string());
            status.insert("config".to_string(), "ok".to_string());
            tracing::debug!(file = %path_log_label(&config_path), "配置收集完成（API key 已脱敏）");
            format!("# 配置文件: {file_label}\n# 注意：API key 已脱敏为 [REDACTED]\n\n{redacted}")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // config.toml 不存在是预期行为：Ramaria 将配置存储在数据库中，不使用文件配置
            status.insert(
                "config".to_string(),
                "skipped: 未使用配置文件（配置存储在数据库中）".to_string(),
            );
            tracing::debug!("配置文件不存在（预期：配置存储在数据库中），跳过收集");
            String::from(
                "# 配置文件不存在（Ramaria 将配置存储在数据库中，不使用 config.toml 文件）\n",
            )
        }
        Err(e) => {
            // 占位文本随诊断包外发：路径只写文件名（与成功内容的导出前脱敏口径一致）
            let msg = format!(
                "# 无法读取配置文件 ({}): {}\n",
                path_log_label(&config_path),
                e
            );
            status.insert("config".to_string(), format!("error: {e}"));
            tracing::warn!(file = %path_log_label(&config_path), error = %e, "配置收集失败");
            msg
        }
    }
}

/// 收集检索索引构建状态：`ok` / `failed: <脱敏原因>` / `not_built`。
///
/// 口径:
/// - 最近一次构建失败 → `failed: <原因>`（原因在记录时已脱敏，不含用户原文）；
/// - 无失败且成功构建过（构建完成时间非零）→ `ok`；
/// - 否则 → `not_built`（尚未触发过懒加载 / 自愈构建）。
fn collect_index_build_status(engine: &Engine, status: &mut HashMap<String, String>) {
    let value = match engine.index_build_failure() {
        Some(failure) => format!("failed: {}", failure.reason),
        None if engine.last_index_build_time() > 0 => "ok".to_string(),
        None => "not_built".to_string(),
    };
    status.insert("index_build".to_string(), value);
}

/// 对配置文件内容做 API key 脱敏。
///
/// 脱敏规则:
/// - 匹配模式: 行中包含 `api_key` 或 `apikey`（不区分大小写），且包含 `=`（赋值语句）。
/// - 将 `=` 右侧的内容替换为 ` "[REDACTED]"`。
/// - 不修改注释行（以 `#` 开头）。
///
/// 返回:
/// - 脱敏后的完整文本。
fn redact_api_keys(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            // 跳过纯注释行
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                return line.to_string();
            }

            // 检测 api_key 或 apikey（不区分大小写）
            let lower = trimmed.to_lowercase();
            if lower.contains("api_key") || lower.contains("apikey") {
                // 找到 `=` 的位置
                if let Some(eq_pos) = trimmed.find('=') {
                    let key_part = &trimmed[..eq_pos + 1];
                    return format!("{key_part} \"[REDACTED]\"");
                }
            }

            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// =========================================================
// 内部实现: 导出前二次脱敏
// =========================================================

/// 消息类字段名（完整名，或 `xxx_preview` 形式的后缀名）。
///
/// 说明:
/// - 命中即把字段值替换为字符数占位 `<N chars>`，避免原文随诊断包外发。
/// - 不含 `*_len` 类长度字段（如 `msg_len=12` 本身已不含原文，保持可诊断性）。
const SENSITIVE_FIELD_MARKERS: &[&str] = &[
    "preview", "content", "text", "message", "msg", "reply", "input", "summary", "notes",
    "excerpt", "snippet",
];

/// 二次脱敏原语（日志 / 配置导出与诊断摘要的统一入口）。
///
/// 规则:
/// 1. 消息类字段值 → `<N chars>`（N 为字符数，不输出原文）；
/// 2. 绝对路径（Windows 盘符 / UNC / Unix 绝对路径）→ 仅保留最后一段（文件名）。
///
/// 说明:
/// - 保留行结构与空白，便于人工阅读与定位；
/// - 该函数是"最后一道防线"，不依赖上游日志是否已做脱敏；同时供索引构建失败原因等
///   诊断摘要文本的脱敏复用（统一口径，避免两套脱敏实现）；
/// - 字段值以空白分隔且未加引号时只能取到首个词（结构化日志的内容字段通常由
///   Debug 格式化加引号，可完整覆盖）。
pub(crate) fn redact_for_export(content: &str) -> String {
    content
        .split_inclusive('\n')
        .map(redact_line)
        .collect::<String>()
}

/// 单行脱敏：按空白切分 token（保留空白片段），逐 token 处理。
fn redact_line(line: &str) -> String {
    split_words_with_separators(line)
        .into_iter()
        .map(|(is_space, part)| {
            if is_space {
                part.to_string()
            } else {
                // 先按字段脱敏（可能吞掉带引号的值），再替换其中的绝对路径
                redact_paths_in(&redact_sensitive_field(part))
            }
        })
        .collect()
}

/// 按空白切分但保留空白片段（保证脱敏后行结构与空白原样保留）。
fn split_words_with_separators(line: &str) -> Vec<(bool, &str)> {
    let mut parts: Vec<(bool, &str)> = Vec::new();
    let mut start = 0usize;
    let mut current: Option<bool> = None;

    for (i, ch) in line.char_indices() {
        let is_space = ch.is_whitespace();
        if current == Some(!is_space) {
            parts.push((!is_space, &line[start..i]));
            start = i;
        }
        current = Some(is_space);
    }
    if start < line.len() {
        parts.push((current.unwrap_or(false), &line[start..]));
    }
    parts
}

/// 单个 token 的字段脱敏：`字段=值` 中字段命中敏感名单时，值替换为 `<N chars>`。
fn redact_sensitive_field(token: &str) -> String {
    let Some(eq) = token.find('=') else {
        return token.to_string();
    };
    let name = token[..eq]
        .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .to_ascii_lowercase();
    if !is_sensitive_field_name(&name) {
        return token.to_string();
    }

    let value = &token[eq + 1..];
    if value.is_empty() {
        return token.to_string();
    }

    // 值可能是 Debug `?` 格式化的引号包裹，也可能是 Display `%` 的裸文本
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    let inner = if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    };
    let count = inner.chars().count();
    let prefix = &token[..eq];
    if quoted {
        format!("{prefix}=\"<{count} chars>\"")
    } else {
        format!("{prefix}=<{count} chars>")
    }
}

/// 字段名是否命中敏感名单（完整名，或 `xxx_preview` 形式的后缀名）。
fn is_sensitive_field_name(name: &str) -> bool {
    SENSITIVE_FIELD_MARKERS
        .iter()
        .any(|marker| name == *marker || name.ends_with(&format!("_{marker}")))
}

/// 替换 token 内的绝对路径为文件名（Windows 盘符 / UNC / Unix 绝对路径）。
fn redact_paths_in(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut i = 0usize;

    while i < token.len() {
        if let Some(len) = absolute_path_len_at(token, i) {
            out.push_str(&path_file_name(&token[i..i + len]));
            i += len;
            continue;
        }
        // 不是路径起点：按 UTF-8 字符整体推进（不切开多字节字符）
        let ch_len = token[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&token[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// 判断 `s[i..]` 是否以绝对路径开头；是则返回该路径片段长度（字节）。
///
/// 判定:
/// - 路径必须起始于边界（token 起点或空白/引号/等号/括号等之后），
///   避免把 URL（`https://...`）中的路径段误判为本地路径；
/// - Windows：`X:\` / `X:/`；UNC：`\\server\share`；
/// - Unix：`/xxx`（排除 `//` 与 `/` 后紧跟空白）。
fn absolute_path_len_at(s: &str, i: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if i != 0 && !is_path_boundary(bytes[i - 1]) {
        return None;
    }

    let windows_drive = i + 2 < bytes.len()
        && bytes[i].is_ascii_alphabetic()
        && bytes[i + 1] == b':'
        && (bytes[i + 2] == b'\\' || bytes[i + 2] == b'/');
    let unc = bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\';
    let unix = bytes[i] == b'/'
        && i + 1 < bytes.len()
        && bytes[i + 1] != b'/'
        && !bytes[i + 1].is_ascii_whitespace();

    if !(windows_drive || unc || unix) {
        return None;
    }

    let mut end = i;
    while end < bytes.len() && !is_path_terminator(bytes[end]) {
        end += 1;
    }
    // 仅取到前缀/单个分隔符（如裸 "C:\"）时视为非路径，保持原文
    if end <= i + 1 {
        return None;
    }
    Some(end - i)
}

/// 路径起点允许的前导字节（排除词内斜杠与 URL 协议段）。
fn is_path_boundary(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'"'
            | b'\''
            | b'='
            | b'('
            | b'['
            | b'{'
            | b','
            | b':'
            | b'<'
            | b'`'
            | b'|'
    )
}

/// 路径终止字节（路径片段到此为止，后续字符原样保留）。
fn is_path_terminator(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'"' | b'\''
                | b','
                | b')'
                | b']'
                | b'}'
                | b'>'
                | b'<'
                | b'|'
                | b'*'
                | b'?'
                | b';'
                | b'`'
        )
}

/// 取路径最后一段（文件名）；空路径（如 `/`）返回 `<path>` 占位。
fn path_file_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    if last.is_empty() {
        "<path>".to_string()
    } else {
        last.to_string()
    }
}

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}

// =========================================================
// 内部实现: zip 打包
// =========================================================

/// 临时文件名后缀：写入完成后通过原子重命名替换正式文件。
const TEMP_SUFFIX: &str = ".part";

/// 将收集到的诊断数据打包为 .zip 文件。
///
/// 打包策略:
/// - 先将内容写入与目标同目录的临时文件（`{文件名}.part`），全部写入并 `finish`
///   成功后再用 `std::fs::rename` 原子替换目标路径（Windows 下可原子覆盖已存在文件）。
/// - 任一步失败返回 Err，并清理残留临时文件，不留半成品覆盖旧文件。
/// - 使用 Deflated 压缩（平衡速度与体积）。
/// - 每个文件一行写入，不在内存中构建完整 zip。
///
/// 返回:
/// - 写入的字节数（文件大小）。
fn build_zip(
    output_path: &Path,
    system_info: &SystemInfo,
    logs: &str,
    config_content: &str,
) -> Result<u64, String> {
    // 确保父目录存在（仅当父目录为非空路径）
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建输出目录 '{}': {e}", parent.display()))?;
    }

    // 临时文件与目标同目录，保证 rename 在同一文件系统内、可原子覆盖旧文件
    let file_name = output_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("输出路径缺少文件名: '{}'", output_path.display()))?;
    let temp_path = output_path.with_file_name(format!("{file_name}{TEMP_SUFFIX}"));

    // 主体闭包：先写临时文件，成功后再原子替换目标；任何 Err 由外层清理临时文件
    let result = (|| -> Result<u64, String> {
        let bytes = write_zip(&temp_path, system_info, logs, config_content)?;
        std::fs::rename(&temp_path, output_path).map_err(|e| {
            format!(
                "原子替换 zip 失败 '{}' → '{}': {e}",
                temp_path.display(),
                output_path.display()
            )
        })?;
        Ok(bytes)
    })();

    if result.is_err() {
        // 写入或 rename 中途失败：清理可能残留的临时文件，不留盘
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// 将诊断数据写入指定路径的 .zip 文件。
///
/// 参数:
/// - `zip_path`: 目标 .zip 文件路径（由调用方决定为临时或正式路径）。
///
/// 返回:
/// - 写入的字节数（文件大小）。
fn write_zip(
    zip_path: &Path,
    system_info: &SystemInfo,
    logs: &str,
    config_content: &str,
) -> Result<u64, String> {
    let file = std::fs::File::create(zip_path)
        .map_err(|e| format!("无法创建临时 zip 文件 '{}': {e}", zip_path.display()))?;

    let mut zip_writer = zip::ZipWriter::new(file);

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // 1. 写入 system.txt
    let system_content = build_system_txt(system_info);
    zip_writer
        .start_file("system.txt", options)
        .map_err(|e| format!("zip 写入 system.txt 失败: {e}"))?;
    zip_writer
        .write_all(system_content.as_bytes())
        .map_err(|e| format!("zip 写入 system.txt 内容失败: {e}"))?;

    // 2. 写入 ramaria.log
    zip_writer
        .start_file("ramaria.log", options)
        .map_err(|e| format!("zip 写入 ramaria.log 失败: {e}"))?;
    zip_writer
        .write_all(logs.as_bytes())
        .map_err(|e| format!("zip 写入 ramaria.log 内容失败: {e}"))?;

    // 3. 写入 config.toml
    zip_writer
        .start_file("config.toml", options)
        .map_err(|e| format!("zip 写入 config.toml 失败: {e}"))?;
    zip_writer
        .write_all(config_content.as_bytes())
        .map_err(|e| format!("zip 写入 config.toml 内容失败: {e}"))?;

    // 完成写入，获取文件大小
    let finished = zip_writer
        .finish()
        .map_err(|e| format!("zip 完成写入失败: {e}"))?;

    let file_size = finished.metadata().map(|m| m.len()).unwrap_or(0);

    Ok(file_size)
}

/// 构建 system.txt 内容。
///
/// 格式: 键值对，每行一个属性，便于机器解析和人类阅读。
fn build_system_txt(info: &SystemInfo) -> String {
    format!(
        "# Ramaria 诊断报告 - 系统信息\n\
         # 采集时间: {collected_at}\n\
         \n\
         os = {os}\n\
         arch = {arch}\n\
         family = {family}\n\
         app_version = {app_version}\n\
         schema_version = {schema_version}\n",
        collected_at = info.collected_at,
        os = info.os,
        arch = info.arch,
        family = info.family,
        app_version = info.app_version,
        schema_version = info.schema_version,
    )
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── API key 脱敏 ──

    #[test]
    fn test_redact_api_key_cases() {
        // 单行脱敏、大小写不敏感、注释与无关键保持原样
        let cases = [
            ("api_key = \"sk-abc123def456\"", "api_key = \"[REDACTED]\""),
            ("API_KEY = \"secret\"", "API_KEY = \"[REDACTED]\""),
            (
                "# api_key = \"this is a comment\"",
                "# api_key = \"this is a comment\"",
            ),
            (
                "base_url = \"https://api.example.com\"",
                "base_url = \"https://api.example.com\"",
            ),
            ("// api_key = \"value\"", "// api_key = \"value\""),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_api_keys(input), expected, "input={input:?}");
        }
        // 多行: 保留 base_url/model_id、api_key 脱敏、不含原密文
        let input =
            "base_url = \"https://api.example.com\"\napi_key = \"my-secret\"\nmodel_id = \"gpt-4\"";
        let result = redact_api_keys(input);
        assert!(result.contains("base_url"));
        assert!(result.contains("[REDACTED]"));
        assert!(result.contains("model_id"));
        assert!(!result.contains("my-secret"));
    }

    // ── 导出前二次脱敏 ──

    /// 消息类字段 → 字符数占位；长度类字段不受影响。
    #[test]
    fn redact_for_export_replaces_message_fields() {
        let cases = [
            (r#"preview="我想吃火锅""#, r#"preview="<5 chars>""#),
            ("msg=hello", "msg=<5 chars>"),
            ("user_input=\"hi\"", "user_input=\"<2 chars>\""),
            // 长度字段本身不含原文，保持可诊断性
            ("msg_len=12", "msg_len=12"),
            ("content_len=3", "content_len=3"),
            // 非敏感字段保持原样
            ("dropped=Some(2)", "dropped=Some(2)"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_for_export(input), expected, "input={input}");
        }
    }

    /// 绝对路径 → 文件名；URL 与非路径文本不受影响；多语言内容不 panic。
    #[test]
    fn redact_for_export_replaces_absolute_paths() {
        let cases = [
            (
                r"path=C:\Users\someone\Documents\ramaria.log",
                "path=ramaria.log",
            ),
            (
                "无法读取 /home/someone/private/ramaria.log",
                "无法读取 ramaria.log",
            ),
            (r#""C:\Users\someone\AppData\Roaming""#, r#""Roaming""#),
            ("dir=/tmp/", "dir=tmp"),
            // URL 中的路径段不是本地路径，保持原样
            (
                "url=https://api.github.com/repos/entergirl/Ramaria-s",
                "url=https://api.github.com/repos/entergirl/Ramaria-s",
            ),
            // 中文与路径混排不 panic、不切坏多字节字符
            (
                "导入到 C:\\用户\\文档\\导出.zip 完成",
                "导入到 导出.zip 完成",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_for_export(input), expected, "input={input}");
        }
    }

    /// 行结构与空白保留（脱敏不破坏日志可读性）。
    #[test]
    fn redact_for_export_keeps_line_structure() {
        let input = "INFO x: a  preview=\"秘密内容\"\n\nWARN y: b\n";
        let out = redact_for_export(input);
        assert_eq!(out.lines().count(), 3, "行数应保持");
        assert!(out.contains("INFO x: a  preview=\"<4 chars>\""));
        assert!(out.ends_with('\n'), "末尾换行应保留");
    }

    // ── system.txt 构建 ──

    #[test]
    fn test_build_system_txt_contains_all_fields() {
        let info = SystemInfo {
            os: "windows".into(),
            arch: "x86_64".into(),
            family: "windows".into(),
            app_version: "2.0.0".into(),
            schema_version: "1".into(),
            collected_at: "2026-06-15T12:00:00Z".into(),
        };

        let content = build_system_txt(&info);

        assert!(content.contains("os = windows"));
        assert!(content.contains("arch = x86_64"));
        assert!(content.contains("app_version = 2.0.0"));
        assert!(content.contains("schema_version = 1"));
        assert!(content.contains("2026-06-15T12:00:00Z"));
    }

    // ── 系统信息收集 ──

    #[test]
    fn test_collect_system_info() {
        let info = collect_system_info("42");

        assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.schema_version, "42");
        assert!(!info.collected_at.is_empty());
        // 验证 OS 字段非空
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
    }

    // ── 日志收集: 空目录 ──

    #[test]
    fn test_collect_logs_empty_dir() {
        let mut config = RamariaConfig::default();
        config.paths.log_dir = String::new();
        let mut status = HashMap::new();

        let result = collect_logs(&config, &mut status);

        assert!(result.contains("未配置"));
        assert!(status.contains_key("logs"));
    }

    // ── 配置收集: 空目录 ──

    #[test]
    fn test_collect_config_empty_dir() {
        let mut config = RamariaConfig::default();
        config.paths.config_dir = String::new();
        let mut status = HashMap::new();

        let result = collect_config(&config, &mut status);

        assert!(result.contains("未配置"));
        assert!(status.contains_key("config"));
    }

    // ── 收集失败分支: 占位文本只写文件名 ──

    /// 读取失败（目标同名目录占位）→ 占位文本只含文件名，不把本机绝对路径写进诊断包。
    #[test]
    fn collect_failure_placeholder_keeps_file_name_only() {
        let dir = unique_dir("collect-failure");
        let log_dir = dir.join("logs");
        let config_dir = dir.join("cfg");
        // 同名目录占位 → read_to_string 失败（非 NotFound 分支）
        std::fs::create_dir_all(log_dir.join("ramaria.log")).expect("创建同名目录应成功");
        std::fs::create_dir_all(config_dir.join("config.toml")).expect("创建同名目录应成功");

        let mut config = RamariaConfig::default();
        config.paths.log_dir = log_dir.to_string_lossy().into_owned();
        config.paths.config_dir = config_dir.to_string_lossy().into_owned();
        let mut status = HashMap::new();

        let logs = collect_logs(&config, &mut status);
        let config_text = collect_config(&config, &mut status);

        let dir_label = dir.to_string_lossy().into_owned();
        assert!(
            !logs.contains(&dir_label) && !config_text.contains(&dir_label),
            "占位文本不应含绝对路径: logs={logs} config={config_text}"
        );
        assert!(logs.contains("ramaria.log"), "应保留文件名便于定位: {logs}");
        assert!(
            config_text.contains("config.toml"),
            "应保留文件名便于定位: {config_text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── zip 打包: 原子写入 ──

    fn sample_info() -> SystemInfo {
        SystemInfo {
            os: "windows".into(),
            arch: "x86_64".into(),
            family: "windows".into(),
            app_version: "test".into(),
            schema_version: "1".into(),
            collected_at: "2026-06-15T12:00:00Z".into(),
        }
    }

    /// 在系统临时目录下创建唯一的测试子目录，返回其绝对路径。
    fn unique_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("ramaria_diag_{tag}_{}_{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建测试临时目录失败");
        dir
    }

    /// 装配诊断用例所需的引擎（临时库 + mock LLM；配置快照承载导出路径字段）。
    async fn engine_for_diagnostics(dir: &Path, config: RamariaConfig) -> crate::engine::Engine {
        let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
            .await
            .expect("测试库初始化应成功");
        let storage: std::sync::Arc<dyn ramaria_core::traits::StorageBackend> =
            std::sync::Arc::new(ramaria_storage::SqliteStorage::new(pool));
        crate::engine::Engine::from_parts(
            storage,
            std::sync::Arc::new(crate::test_support::MockLlm::local()),
            None,
            config,
        )
    }

    /// 读取 zip 中指定条目的文本内容（测试辅助）。
    fn read_zip_entry(zip_path: &Path, name: &str) -> String {
        use std::io::Read;

        let file = std::fs::File::open(zip_path).expect("无法打开生成的 zip");
        let mut archive = zip::ZipArchive::new(file).expect("生成的文件不是合法 zip");
        let mut entry = archive.by_name(name).expect("归档缺少目标条目");
        let mut text = String::new();
        entry.read_to_string(&mut text).expect("读取归档条目失败");
        text
    }

    /// 端到端：导出包不含绝对路径与消息原文（二次脱敏后落盘）。
    #[tokio::test]
    async fn export_redacts_absolute_paths_and_message_previews() {
        let dir = unique_dir("redact");
        let log_dir = dir.join("logs");
        let config_dir = dir.join("cfg");
        std::fs::create_dir_all(&log_dir).expect("创建日志目录失败");
        std::fs::create_dir_all(&config_dir).expect("创建配置目录失败");

        // 日志：本机绝对路径（Windows + Unix）与结构化消息字段（含原文）
        std::fs::write(
            log_dir.join("ramaria.log"),
            "INFO ramaria_service::chat: 收到消息 preview=\"我想吃火锅\" path=C:\\Users\\someone\\Documents\\ramaria.log\n\
             WARN ramaria_service::io: 无法读取 /home/someone/private/ramaria.log\n",
        )
        .expect("写入日志失败");
        // 配置：本机路径 + API key（TOML 中以 \\ 转义反斜杠）
        std::fs::write(
            config_dir.join("config.toml"),
            "[paths]\ndata_dir = \"C:\\\\Users\\\\someone\\\\AppData\\\\Roaming\\\\Ramaria\"\napi_key = \"sk-secret\"\n",
        )
        .expect("写入配置失败");

        let mut config = RamariaConfig::default();
        config.paths.log_dir = log_dir.to_string_lossy().into_owned();
        config.paths.config_dir = config_dir.to_string_lossy().into_owned();

        let zip_path = dir.join("diag.zip");
        // 经服务层引擎调用导出用例（配置快照承载日志 / 配置目录字段）
        let engine = engine_for_diagnostics(&dir, config).await;
        let report = engine
            .export_diagnostics(DiagnosticsRequest {
                output_path: zip_path.clone(),
                schema_version: "1".to_string(),
            })
            .await
            .expect("导出诊断包应成功");
        assert!(report.file_size_bytes > 0);

        let logs = read_zip_entry(&zip_path, "ramaria.log");
        assert!(
            !logs.contains("C:\\Users"),
            "日志不得含 Windows 绝对路径: {logs}"
        );
        assert!(
            !logs.contains("/home/someone"),
            "日志不得含 Unix 绝对路径: {logs}"
        );
        assert!(logs.contains("ramaria.log"), "应保留文件名便于定位: {logs}");
        assert!(
            logs.contains("preview=\"<5 chars>\""),
            "消息预览应替换为字符数: {logs}"
        );
        assert!(!logs.contains("我想吃火锅"), "日志不得含消息原文");

        let cfg = read_zip_entry(&zip_path, "config.toml");
        assert!(!cfg.contains("Users"), "配置不得含绝对路径: {cfg}");
        assert!(
            cfg.contains("data_dir = \"Ramaria\""),
            "路径应只保留最后一段: {cfg}"
        );
        assert!(cfg.contains("[REDACTED]"), "API key 仍应脱敏: {cfg}");
        assert!(
            cfg.contains("# 配置文件: config.toml"),
            "包头只写文件名: {cfg}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 检索索引构建状态进入诊断收集：未构建 / 失败（脱敏原因）/ 已构建三态。
    #[tokio::test]
    async fn export_reports_index_build_status() {
        let dir = unique_dir("index-status");
        let mut config = RamariaConfig::default();
        config.paths.log_dir = dir.join("logs").to_string_lossy().into_owned();
        config.paths.config_dir = dir.join("cfg").to_string_lossy().into_owned();

        // 组装带失败注入的引擎（真实 SQLite + 可失败存储包装）
        let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
            .await
            .expect("测试库初始化应成功");
        let inner = std::sync::Arc::new(ramaria_storage::SqliteStorage::new(pool));
        let failable = std::sync::Arc::new(crate::test_support::FailableStorage::new(inner));
        let engine = crate::engine::Engine::from_parts(
            std::sync::Arc::clone(&failable)
                as std::sync::Arc<dyn ramaria_core::traits::StorageBackend>,
            std::sync::Arc::new(crate::test_support::MockLlm::local()),
            None,
            config,
        );

        // 1) 未构建 → not_built
        let report = engine
            .export_diagnostics(DiagnosticsRequest {
                output_path: dir.join("diag-1.zip"),
                schema_version: "1".to_string(),
            })
            .await
            .expect("导出诊断包应成功");
        assert_eq!(
            report
                .collection_status
                .get("index_build")
                .map(String::as_str),
            Some("not_built"),
            "未构建时收集状态应为 not_built: {:?}",
            report.collection_status
        );

        // 2) 构建失败 → failed: <脱敏原因>（保留可诊断信息、折叠为单行）
        failable.set_fail_list_personas(true);
        engine
            .rebuild_index()
            .await
            .expect_err("失败注入下重建应报错");
        let report = engine
            .export_diagnostics(DiagnosticsRequest {
                output_path: dir.join("diag-2.zip"),
                schema_version: "1".to_string(),
            })
            .await
            .expect("导出诊断包应成功");
        let failed = report
            .collection_status
            .get("index_build")
            .cloned()
            .unwrap_or_default();
        assert!(failed.starts_with("failed: "), "失败态前缀: {failed}");
        assert!(
            failed.contains("list_personas"),
            "应保留可诊断信息: {failed}"
        );
        assert!(!failed.contains('\n'), "失败原因应折叠为单行: {failed}");

        // 3) 恢复后成功构建 → ok（失败记录清除）
        failable.set_fail_list_personas(false);
        engine.rebuild_index().await.expect("恢复后重建应成功");
        assert!(
            engine.index_build_failure().is_none(),
            "构建成功后失败记录应清除"
        );
        let report = engine
            .export_diagnostics(DiagnosticsRequest {
                output_path: dir.join("diag-3.zip"),
                schema_version: "1".to_string(),
            })
            .await
            .expect("导出诊断包应成功");
        assert_eq!(
            report
                .collection_status
                .get("index_build")
                .map(String::as_str),
            Some("ok"),
            "成功构建后收集状态应为 ok: {:?}",
            report.collection_status
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 用 zip crate 读取并断言归档包含三份诊断文件。
    fn assert_archive_has_three_files(zip_path: &Path) {
        let file = std::fs::File::open(zip_path).expect("无法打开生成的 zip");
        let mut archive = zip::ZipArchive::new(file).expect("生成的文件不是合法 zip");
        let names: Vec<String> = (0..archive.len())
            .map(|i| {
                archive
                    .by_index(i)
                    .expect("读取归档条目失败")
                    .name()
                    .to_string()
            })
            .collect();
        for expected in ["system.txt", "ramaria.log", "config.toml"] {
            assert!(
                names.iter().any(|n| n == expected),
                "归档缺少条目 {expected}，实际: {names:?}"
            );
        }
    }

    // build_zip 生成有效归档：返回大小 > 0、目标存在、同目录无 .part 残留。
    #[test]
    fn build_zip_writes_valid_archive_and_cleans_temp() {
        let dir = unique_dir("valid");
        let target = dir.join("report.zip");
        let info = sample_info();

        let bytes =
            build_zip(&target, &info, "log line 1\n", "[config]\nkey=1").expect("build_zip 应成功");

        assert!(bytes > 0, "返回字节数应为正，得到 {bytes}");
        assert_eq!(
            std::fs::metadata(&target).expect("目标 zip 应存在").len(),
            bytes,
            "目标文件大小应与返回字节数一致"
        );
        assert!(
            !dir.join("report.zip.part").exists(),
            "成功后不应残留 .part 临时文件"
        );
        assert_archive_has_three_files(&target);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 目标已存在旧内容时，build_zip 用完整 zip 原子替换（不残留半成品或 temp）。
    #[test]
    fn build_zip_atomically_replaces_existing_target() {
        let dir = unique_dir("replace");
        let target = dir.join("report.zip");
        // 预写一个旧内容文件（非 zip）
        std::fs::write(&target, b"old stale content").expect("预写旧内容失败");
        let info = sample_info();

        let bytes = build_zip(&target, &info, "fresh log\n", "[config]\nfresh=1")
            .expect("build_zip 应成功");

        assert!(bytes > 0);
        let size = std::fs::metadata(&target).expect("替换后目标应存在").len();
        assert!(
            size != b"old stale content".len() as u64,
            "目标应被新 zip 替换而非保留旧内容长度"
        );
        assert!(
            !dir.join("report.zip.part").exists(),
            "替换后不应残留 .part 临时文件"
        );
        // 目标是合法 zip 且含三文件，证明旧内容已被完整覆盖
        assert_archive_has_three_files(&target);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 失败路径（目标为已存在目录 → rename 失败）：返回 Err、无 .part 残留、
    // 原目标（目录）保持不被半文件污染。锁定的是"失败不留半文件/temp"分支。
    #[test]
    fn build_zip_failure_leaves_no_partial_target() {
        let dir = unique_dir("failure");
        // 目标位置放一个目录：write 阶段写 .part 成功，rename 到该目录失败
        let target = dir.join("report.zip");
        std::fs::create_dir(&target).expect("创建目标目录失败");
        let info = sample_info();

        let err = build_zip(&target, &info, "log\n", "cfg\n").expect_err("目标为目录时应失败");

        assert!(!err.is_empty(), "错误信息不应为空");
        assert!(
            !dir.join("report.zip.part").exists(),
            "失败后不应残留 .part 临时文件"
        );
        assert!(
            target.is_dir(),
            "原目标目录应保持不变（未被半成品 zip 覆盖）"
        );
        // 目录内不应被写入任何文件
        let entries: Vec<_> = std::fs::read_dir(&target)
            .expect("读取目标目录失败")
            .collect();
        assert!(entries.is_empty(), "失败不应在目录内留下写入内容");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
