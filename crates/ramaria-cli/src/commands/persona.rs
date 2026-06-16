//! rust/crates/ramaria-cli/src/commands/persona.rs - 人格文件管理命令
//!
//! 设计特点:
//! - show: 从 DB 读取所有 persona，展示 uid/name/kind/config 摘要
//! - reload: 扫描 personas/ 目录下 .toml 文件，创建或更新 DB 中的 persona 记录
//! - 人格文件（.toml）是定义的权威源，DB 的 personas.config 字段是运行时缓存
//! - 用户用文本编辑器直接编辑 .toml 文件，reload 同步到 DB
//! - 文件名 = persona UID（如 rama-0001.toml），kind 从 UID 前缀自动推断
//! - 不提供 CLI 逐字段编辑命令，保持简洁

use anyhow::Context;
use ramaria_core::types::{Persona, PersonaKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// =========================================================
// 常量
// =========================================================

/// 人格文件目录（相对于 rust/ 工作目录，即 ../config/personas/）。
/// 运行期路径设计为 `%APPDATA%\Ramaria\personas\`，安装版首次释放到此目录。
const PERSONAS_DIR: &str = "../config/personas";

// =========================================================
// 公共枚举与入口
// =========================================================

/// Persona 子命令。
///
/// 职责:
/// - Show: 展示当前所有已注册人格的摘要信息。
/// - Reload: 重新扫描文件系统，同步到 DB（支持全量或按 UID 指定）。
#[derive(Debug, Clone)]
pub enum PersonaCmd {
    /// 显示所有人格摘要（uid / name / kind / config 预览）
    Show,
    /// 从 personas/ 目录重新加载人格文件到 DB
    Reload {
        /// 指定要重新加载的 persona UID（默认加载全部 .toml 文件）
        uid: Option<String>,
    },
}

/// 运行 persona 子命令分发。
///
/// 参数:
/// - `app`: App 实例引用。
/// - `cmd`: Persona 子命令。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: PersonaCmd) -> anyhow::Result<()> {
    match cmd {
        PersonaCmd::Show => run_show(app).await,
        PersonaCmd::Reload { uid } => run_reload(app, uid).await,
    }
}

// =========================================================
// show 命令
// =========================================================

/// 展示所有已注册人格的基本信息。
///
/// 说明:
/// - 从 storage.list_personas 读取全部活跃 persona。
/// - 解析 config 字段中的 TOML 内容，提取 assistant_name 和人设/规则摘要。
/// - 无 persona 时输出引导提示。
async fn run_show(app: &Arc<ramaria_app::App>) -> anyhow::Result<()> {
    let personas = app
        .storage()
        .list_personas()
        .await
        .context("查询 persona 列表失败")?;

    if personas.is_empty() {
        crate::ui::info("暂无已注册的人格");
        crate::ui::info(&format!(
            "人格文件目录: {}",
            Path::new(PERSONAS_DIR).display()
        ));
        crate::ui::info("将 .toml 文件放入该目录后运行 `ramaria persona reload` 加载");
        return Ok(());
    }

    crate::ui::separator();
    println!("  已注册人格 ({})", personas.len());
    crate::ui::separator();
    println!();

    for p in &personas {
        display_persona(p);
        println!();
    }

    // 提示文件路径
    crate::ui::info(&format!(
        "提示: 编辑 `{}` 下的 .toml 文件后运行 `ramaria persona reload` 同步",
        PERSONAS_DIR
    ));
    Ok(())
}

/// 格式化输出单个 persona。
fn display_persona(p: &Persona) {
    println!("  UID:     {}", p.uid);
    println!("  名称:    {}", p.name);
    println!("  类型:    {}", p.kind.as_str());
    println!("  状态:    {}", if p.active { "活跃" } else { "已停用" });

    // 解析 config 中的 TOML 内容提取关键信息
    if let Some(ref config) = p.config {
        if let Some(name) = crate::util::extract_toml_value(config, "assistant_name")
            && name != p.name
        {
            println!("  TOML名称: {}", name);
        }

        // 提取 A_persona 块摘要
        if let Some(block) = extract_toml_block(config, "A_persona") {
            let preview = summarize_block(&block, 150);
            println!("  人设概要: {}", preview);
        }

        // 提取 E_rules 块摘要
        if let Some(block) = extract_toml_block(config, "E_rules") {
            let preview = summarize_block(&block, 100);
            println!("  规则概要: {}", preview);
        }
    } else {
        println!("  配置:     (空 — 未加载人格文件)");
    }
}

// =========================================================
// reload 命令
// =========================================================

/// 从 personas/ 目录重新加载人格文件到 DB。
///
/// 流程:
/// 1. 验证目录存在。
/// 2. 收集 .toml 文件（可按 --uid 筛选）。
/// 3. 逐个文件读取 → 解析 assistant_name → 创建或更新 DB 记录。
/// 4. 输出加载结果摘要。
async fn run_reload(app: &Arc<ramaria_app::App>, uid: Option<String>) -> anyhow::Result<()> {
    let dir = Path::new(PERSONAS_DIR);

    if !dir.exists() {
        return Err(anyhow::anyhow!(
            "人格文件目录不存在: {}\n\
             请确认工作目录在 rust/ 下，或创建 ../config/personas/ 目录并放入 .toml 文件。",
            dir.display()
        ));
    }
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("路径不是目录: {}", dir.display()));
    }

    // 收集要处理的文件
    let files = collect_toml_files(dir, uid.as_deref())?;

    if files.is_empty() {
        if let Some(ref target_uid) = uid {
            return Err(anyhow::anyhow!(
                "未找到匹配的人格文件: {}.toml (在 {} 下)",
                target_uid,
                dir.display()
            ));
        }
        crate::ui::warn(&format!("在 {} 下未找到 .toml 文件", dir.display()));
        return Ok(());
    }

    crate::ui::separator();
    println!("  正在加载人格文件... (共 {} 个)", files.len());
    crate::ui::separator();
    println!();

    let mut success_count = 0u32;
    let mut error_count = 0u32;

    for path in &files {
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".to_string());

        match reload_single_file(app, path).await {
            Ok(uid_loaded) => {
                crate::ui::success(&format!("{filename} → {uid_loaded}"));
                success_count += 1;
            }
            Err(e) => {
                crate::ui::warn(&format!("{filename}: {e}"));
                tracing::error!(path = %path.display(), error = %e, "人格文件加载失败");
                error_count += 1;
            }
        }
    }

    println!();
    crate::ui::separator();
    println!("  加载完成: {} 成功, {} 失败", success_count, error_count);
    crate::ui::separator();

    if error_count > 0 && success_count == 0 {
        return Err(anyhow::anyhow!("所有文件加载均失败，请检查文件格式和权限"));
    }

    Ok(())
}

/// 收集目录下所有 .toml 文件（可过滤指定 UID）。
fn collect_toml_files(dir: &Path, target_uid: Option<&str>) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();

    let entries =
        std::fs::read_dir(dir).with_context(|| format!("读取目录失败: {}", dir.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| format!("读取目录条目失败: {}", dir.display()))?;
        let path = entry.path();

        // 只处理 .toml 文件
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            continue;
        }

        // 按 UID 过滤（文件名不含扩展名匹配）
        if let Some(uid) = target_uid {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            if stem.as_ref() != uid {
                continue;
            }
        }

        files.push(path);
    }

    // 确保处理顺序可预测
    files.sort();
    Ok(files)
}

/// 加载单个 .toml 文件：解析 → 读取 DB → 创建或更新。
///
/// 返回:
/// - 成功时返回加载的 persona UID。
async fn reload_single_file(app: &Arc<ramaria_app::App>, path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取文件失败: {}", path.display()))?;

    // 从文件名提取 UID
    let uid = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "无法从文件名提取 UID: {}。文件名必须为 <uid>.toml 格式（如 rama-0001.toml）",
                path.display()
            )
        })?;

    // 提取人格名称
    let name =
        crate::util::extract_toml_value(&content, "assistant_name").unwrap_or_else(|| uid.clone());

    // 推断人格类型
    let kind = PersonaKind::from_uid(&uid);

    // 检查是否已存在
    let existing = app
        .storage()
        .get_persona_by_uid(&uid)
        .await
        .context("查询已有 persona 失败")?;

    if existing.is_some() {
        // 更新已有 persona 的 name 和 config
        app.storage()
            .update_persona(&uid, &name, None, Some(&content), None)
            .await
            .with_context(|| format!("更新 persona 失败: {uid}"))?;
        tracing::info!(%uid, %name, "reload: 已更新 persona");
    } else {
        // 创建新 persona
        let mut persona = Persona::new(uid.clone(), name.clone(), kind, 1, "file".to_string());
        persona.config = Some(content.clone());
        app.storage()
            .create_persona(&persona)
            .await
            .with_context(|| format!("创建 persona 失败: {uid}"))?;
        tracing::info!(%uid, %name, kind = %kind.as_str(), "reload: 已创建新 persona");
    }

    Ok(uid)
}

// =========================================================
// TOML 解析辅助函数
// =========================================================
//
// 注: `extract_toml_value` 已提取至 `crate::util` 模块。
// `extract_toml_block` / `summarize_block` 仅被 persona 命令使用，保留于此。

/// 从 TOML 文本中提取 `[blocks]` 节下的多行字符串块。
///
/// 使用简单的状态机解析 TOML 的三引号多行字符串 `"""..."""`。
/// 精确匹配 `key = """` 开始，直到遇到单独的 `"""` 结束。
///
/// 参数:
/// - `content`: 完整 TOML 文本。
/// - `key`: 要查找的块名（如 "A_persona"）。
///
/// 返回:
/// - `Some(block_content)`: 找到的块内容（去除开头的空行）。
/// - `None`: 未找到或格式不符合预期。
fn extract_toml_block(content: &str, key: &str) -> Option<String> {
    let lines = content.lines();
    let mut in_block = false;
    let mut block_lines: Vec<&str> = Vec::new();

    for line in lines {
        let trimmed = line.trim();

        if in_block {
            // 遇到独立的三引号表示结束
            if trimmed == "\"\"\"" {
                break;
            }
            block_lines.push(line);
        } else if trimmed.starts_with(&format!("{key} = \"\"\"")) {
            in_block = true;
            // 如果同一行后面还有内容（罕见），直接开始收集
            let after_marker = &trimmed[key.len() + 5..]; // skip `key = """`
            if after_marker == "\"\"\"" {
                // 单行空块
                break;
            }
            // 如果 """ 在同一行后还有内容（非标准写法），跳过
        }
    }

    if block_lines.is_empty() {
        return None;
    }

    // 去除首尾空行
    while block_lines.first().is_some_and(|l| l.trim().is_empty()) {
        block_lines.remove(0);
    }
    while block_lines.last().is_some_and(|l| l.trim().is_empty()) {
        block_lines.pop();
    }

    if block_lines.is_empty() {
        return None;
    }

    Some(block_lines.join("\n"))
}

/// 将文本截断为指定最大字符数，添加 "..." 省略号。
///
/// 按字符边界截断，不破坏 UTF-8。
fn summarize_block(text: &str, max_chars: usize) -> String {
    // 先压缩多余空白为单个空格，便于单行展示
    let compressed: String = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if compressed.chars().count() <= max_chars {
        compressed
    } else {
        let truncated: String = compressed.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // 注: extract_toml_value 的单元测试已移至 crate::util 模块，
    // 此处仅保留 extract_toml_block / summarize_block / PersonaKind::from_uid 的测试。

    #[test]
    fn extract_toml_block_basic() {
        let toml = "[blocks]\nA_persona = \"\"\"\n我是测试人格。\n性格：温和。\n\"\"\"\nE_rules = \"\"\"\n规则内容\n\"\"\"";
        let result = extract_toml_block(toml, "A_persona");
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("我是测试人格"));
        assert!(content.contains("性格：温和"));
        assert!(!content.contains("\"\"\""));
    }

    #[test]
    fn extract_toml_block_not_found() {
        let toml = "[blocks]\nA_persona = \"\"\"\n内容\n\"\"\"";
        assert_eq!(extract_toml_block(toml, "E_rules"), None);
    }

    #[test]
    fn extract_toml_block_empty_block() {
        let toml = "[blocks]\nA_persona = \"\"\"\"\"\"";
        assert_eq!(extract_toml_block(toml, "A_persona"), None);
    }

    #[test]
    fn extract_toml_block_with_empty_lines() {
        let toml = "[blocks]\nA_persona = \"\"\"\n\n\n核心内容\n\n\n\"\"\"";
        let result = extract_toml_block(toml, "A_persona");
        assert_eq!(result, Some("核心内容".to_string()));
    }

    #[test]
    fn summarize_block_short() {
        assert_eq!(summarize_block("短文本", 100), "短文本");
    }

    #[test]
    fn summarize_block_truncated() {
        let long = "a".repeat(200);
        let result = summarize_block(&long, 50);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 53); // 50 chars + "..."
    }

    #[test]
    fn summarize_block_multiline() {
        let text = "第一行\n第二行\n  第三行有空格  ";
        let result = summarize_block(text, 100);
        // 多行合并为单行，以空格分隔
        assert!(result.contains("第一行"));
        assert!(result.contains("第二行"));
        assert!(result.contains("第三行有空格"));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn persona_kind_from_uid_uses_core() {
        // 验证 PersonaKind::from_uid 的行为（依赖 core 实现）
        assert_eq!(PersonaKind::from_uid("rama-0001"), PersonaKind::Rama);
        assert_eq!(PersonaKind::from_uid("user-0001"), PersonaKind::User);
        assert_eq!(PersonaKind::from_uid("char-0001"), PersonaKind::Char);
        assert_eq!(PersonaKind::from_uid("unknown-xyz"), PersonaKind::Char); // 默认回退
    }

    #[test]
    fn collect_toml_files_filters() {
        // 使用 temp dir 测试文件收集逻辑
        let tmp = std::env::temp_dir().join("ramaria_persona_test_collect");
        let _ = std::fs::create_dir_all(&tmp);

        // 创建测试文件
        std::fs::write(tmp.join("rama-0001.toml"), "name = test").unwrap();
        std::fs::write(tmp.join("user-0001.toml"), "name = user").unwrap();
        std::fs::write(tmp.join("readme.txt"), "not a toml").unwrap();

        // 无过滤
        let all = collect_toml_files(&tmp, None).unwrap();
        assert_eq!(all.len(), 2);

        // 按 UID 过滤
        let filtered = collect_toml_files(&tmp, Some("rama-0001")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].ends_with("rama-0001.toml"));

        // 不存在的 UID
        let missing = collect_toml_files(&tmp, Some("nonexistent")).unwrap();
        assert!(missing.is_empty());

        // 清理
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
