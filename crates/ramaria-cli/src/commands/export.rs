//! crates/ramaria-cli/src/commands/export.rs - 数据导出命令
//!
//! 设计特点:
//! - 载荷渲染统一由服务层提供（JSON / Markdown），本模块只做输出编排
//! - JSON: 带版本信封的会话 / 消息结构，--persona 时附 L1 摘要段
//! - Markdown: 人类可读的对话记录
//! - --redact 脱敏：消息正文与 L1 摘要替换为 <N chars>
//! - --output 指定输出文件（缺省 exports/export_{timestamp}.{ext}；`-` = stdout）
//! - 导出路径使用 canonicalize + 前缀检查防护路径穿越

use anyhow::Context;
use ramaria_service::{Engine, ExportDataRequest, render_sessions_json, render_sessions_markdown};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// export 命令参数。
pub struct ExportArgs {
    /// 导出格式: json / markdown
    pub format: String,
    /// 按 persona_uid 筛选
    pub persona: Option<String>,
    /// 输出文件路径（默认 exports/ 目录；`-` 表示 stdout）
    pub output: Option<String>,
    /// 脱敏输出：消息正文与 L1 摘要替换为 <N chars>
    pub redact: bool,
    /// 全局 --json 信封模式：stdout 输出 `{"ok":true,"data":{...}}` 信封
    pub json: bool,
}

/// 执行 export 命令。
pub async fn run(engine: &Arc<Engine>, args: ExportArgs) -> anyhow::Result<()> {
    match args.format.as_str() {
        "json" => export_json(engine, &args).await,
        "markdown" | "md" => export_markdown(engine, &args).await,
        other => anyhow::bail!("不支持的导出格式: '{other}'。支持: json / markdown"),
    }
}

// =========================================================
// JSON 导出
// =========================================================

async fn export_json(engine: &Arc<Engine>, args: &ExportArgs) -> anyhow::Result<()> {
    let data = engine
        .export_sessions(ExportDataRequest {
            persona: args.persona.clone(),
            limit: None,
            offset: None,
        })
        .await
        .context("查询会话失败")?;

    // 载荷结构由服务层统一渲染（CLI / 桌面同源）
    let json_output = render_sessions_json(&data, args.redact);

    // --json 信封模式：stdout 只输出信封（数据在 data.content 或 written_to 指向的文件）
    if args.json {
        if args.output.as_deref() == Some("-") {
            let envelope = serde_json::json!({ "format": "json", "content": json_output });
            return crate::json::emit_ok(&envelope);
        }
        let written_to = write_output(&json_output, args.output.as_deref(), "json")?;
        let envelope = serde_json::json!({
            "format": "json",
            "written_to": written_to,
            "sessions": data.total_sessions,
        });
        return crate::json::emit_ok(&envelope);
    }

    write_output(&json_output, args.output.as_deref(), "json")?;

    crate::ui::success(&format!("已导出 {} 个会话", data.total_sessions));
    Ok(())
}

// =========================================================
// Markdown 导出
// =========================================================

async fn export_markdown(engine: &Arc<Engine>, args: &ExportArgs) -> anyhow::Result<()> {
    let data = engine
        .export_sessions(ExportDataRequest {
            persona: args.persona.clone(),
            limit: None,
            offset: None,
        })
        .await
        .context("查询会话失败")?;

    // 无消息会话由渲染层跳过；全部为空时返回 None（不写文件，保持与旧行为一致的提示）
    let Some(markdown) = render_sessions_markdown(&data, args.redact) else {
        crate::ui::info("没有可导出的会话数据");
        return Ok(());
    };
    let exported_sessions = data
        .sessions
        .iter()
        .filter(|entry| !entry.messages.is_empty())
        .count();

    // --json 信封模式：stdout 只输出信封（数据在 data.content 或 written_to 指向的文件）
    if args.json {
        if args.output.as_deref() == Some("-") {
            let envelope = serde_json::json!({ "format": "markdown", "content": markdown });
            return crate::json::emit_ok(&envelope);
        }
        let written_to = write_output(&markdown, args.output.as_deref(), "md")?;
        let envelope = serde_json::json!({
            "format": "markdown",
            "written_to": written_to,
            "sessions": exported_sessions,
        });
        return crate::json::emit_ok(&envelope);
    }

    write_output(&markdown, args.output.as_deref(), "md")?;

    crate::ui::success(&format!(
        "已导出 {exported_sessions} 个会话为 Markdown 格式"
    ));
    Ok(())
}

// =========================================================
// 辅助函数
// =========================================================

/// 将内容写入输出。
///
/// - `output` 为 `None` → 自动生成默认路径 `exports/export_<timestamp>.<ext>`。
/// - `output` 为 `Some("-")` → 输出到 stdout。
/// - `output` 为 `Some(path)` → 写入指定文件。
///
/// 安全约束:
/// - 使用 canonicalize 规范化父目录，防止路径穿越攻击（符号链接、`..`、`RootDir`/`Prefix` 组件）。
/// - 自动创建父目录。
///
/// 返回:
/// - `Ok(Some(path))`: 实际写入的文件路径。
/// - `Ok(None)`: 内容已输出到 stdout。
fn write_output(
    content: &str,
    output: Option<&str>,
    format: &str,
) -> anyhow::Result<Option<String>> {
    // → 在 canonicalize 前先确保父目录存在，避免导出目录尚不存在时 canonicalize 报错。
    let path = match output {
        Some("-") => {
            println!("{content}");
            crate::ui::info("已输出到 stdout");
            return Ok(None);
        }
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(default_export_path(format)),
    };

    if let Some(parent) = path.parent() {
        // 跳过空父路径（如当前目录下的裸文件名），避免 create_dir_all("") 报错
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("无法创建输出目录: {}", parent.display()))?;
        }
    }

    let canonical = canonicalize_export_path(&path)?;

    let display_path = canonical.display().to_string();
    let mut file = std::fs::File::create(&canonical)
        .with_context(|| format!("无法创建输出文件: {display_path}"))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("写入文件失败: {display_path}"))?;
    crate::ui::info(&format!("已写入: {display_path}"));

    Ok(Some(display_path))
}

/// 规范化导出路径：通过 canonicalize 父目录防御路径穿越。
///
/// 安全措施:
/// - 对父目录调用 canonicalize，解析所有符号链接和相对路径组件。
/// - 拒绝包含 RootDir 或 Prefix 组件的路径（如 Windows `C:\` 根路径）。
/// - 文件可能尚不存在，不能 canonicalize 文件路径本身，仅对其父目录操作。
fn canonicalize_export_path(path: &Path) -> anyhow::Result<PathBuf> {
    // 拒绝裸根目录和 Windows 盘符前缀路径
    let has_root_or_prefix = path
        .components()
        .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)));
    if has_root_or_prefix && path.components().count() <= 1 {
        return Err(anyhow::anyhow!(
            "不安全的输出路径: '{}'。不能直接导出到根目录。\
             \n请使用当前目录下的路径（如 './exports/my_export.json'）。",
            path.display()
        ));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("导出目录不存在或无法访问: {}", parent.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("无效的路径: 缺少文件名 ({})", path.display()))?;

    Ok(canonical_parent.join(file_name))
}

/// 生成默认导出文件路径: `exports/export_<timestamp>.<ext>`
fn default_export_path(format: &str) -> String {
    let ext = match format {
        "json" => "json",
        "markdown" | "md" => "md",
        _ => "json",
    };
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    format!("exports/export_{ts}.{ext}")
}
