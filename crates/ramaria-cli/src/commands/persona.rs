//! crates/ramaria-cli/src/commands/persona.rs - 人格文件管理命令
//!
//! 设计特点:
//! - show: 从 DB 读取所有 persona，展示 uid/name/kind/config 摘要
//! - reload: 扫描 personas/ 目录下 .toml 文件，创建或更新 DB 中的 persona 记录
//! - export: 画像（含 Manual 部分）导出为预设超集 TOML；不含证据链与会话原文
//! - 人格文件（.toml）是定义的权威源，DB 的 personas.config 字段是运行时缓存
//! - 用户用文本编辑器直接编辑 .toml 文件，reload 同步到 DB
//! - 文件名 = persona UID（如 rama-0001.toml），kind 从 UID 前缀自动推断
//! - 不提供 CLI 逐字段编辑命令，保持简洁

use anyhow::Context;
use ramaria_memory::preset::PreviewMode;
use ramaria_service::types::{DecomposeExecution, PersonaImportOutcome};
use ramaria_service::{Engine, PersonaFileAction, PersonaFullView, PersonaLoadMode};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod export;

// =========================================================
// 常量
// =========================================================

/// 人格文件目录（候选路径按优先级探测，取第一个存在的目录）。
///
/// 候选顺序:
/// 1. `RAMARIA_DATA_DIR` 环境变量指向的数据根下的 `personas/`（运行期安装布局）；
/// 2. 当前工作目录下的 `config/personas`（仓库根运行时）；
/// 3. 上级目录的 `config/personas`（旧 `rust/` 布局兼容）。
///
/// 全部不存在时回退 `config/personas`（相对 CWD）。
pub(crate) fn personas_dir() -> std::path::PathBuf {
    let candidates: Vec<std::path::PathBuf> = std::env::var("RAMARIA_DATA_DIR")
        .ok()
        .map(|d| std::path::PathBuf::from(d).join("personas"))
        .into_iter()
        .chain([
            std::path::PathBuf::from("config/personas"),
            std::path::PathBuf::from("../config/personas"),
        ])
        .collect();

    candidates
        .iter()
        .find(|c| c.is_dir())
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from("config/personas"))
}

// =========================================================
// 公共枚举与入口
// =========================================================

/// Persona 子命令。
///
/// 职责:
/// - List: 结构化列出所有人格（uid / 名称 / kind），供 agent 脚本消费。
/// - Show: 展示当前所有已注册人格的摘要信息。
/// - Reload: 重新扫描文件系统，同步到 DB（支持全量或按 UID 指定）。
/// - Import: 从预设文件导入人格（dry-run 预览 / 执行写入）。
/// - AllowInference: 允许推断接管 Manual 画像（按 label 或全部）。
/// - Prompt: 查看指定人格的 system prompt 渲染结果（静态段 / 全量两档）。
/// - Export: 导出人格画像为预设超集 TOML（不含证据链与会话原文）。
#[derive(Debug, Clone)]
pub enum PersonaCmd {
    /// 结构化列出所有人格（uid / 名称 / kind）
    List {
        /// 输出条数上限（None = 全部）
        limit: Option<usize>,
        /// 跳过前 N 条（分页）
        offset: usize,
    },
    /// 显示所有人格摘要（uid / name / kind / config 预览）
    Show,
    /// 从 personas/ 目录重新加载人格文件到 DB
    Reload {
        /// 指定要重新加载的 persona UID（默认加载全部 .toml 文件）
        uid: Option<String>,
    },
    /// 从预设文件导入人格
    Import {
        /// 预设文件路径
        file: PathBuf,
        /// 仅预览将写入项，不写库
        dry_run: bool,
        /// 合并到已有人格 uid（默认新建）
        into: Option<String>,
        /// 关闭 LLM 拆解
        no_ai: bool,
    },
    /// 允许推断接管 Manual 画像（labels 为空 = 全部）
    AllowInference {
        /// 目标人格 uid
        uid: String,
        /// 目标 trait 标签（精确匹配）
        labels: Vec<String>,
    },
    /// 查看指定人格的 system prompt 渲染结果（静态段预览；--input 走全量预览含召回）
    Prompt {
        /// 目标人格 uid
        uid: String,
        /// 给定输入文本（提供时走全量预览；缺省为静态段预览）
        input: Option<String>,
    },
    /// 导出人格画像为预设文件（超集 TOML；不含证据链与会话原文）
    Export {
        /// 目标人格 uid
        uid: String,
        /// 输出文件路径（缺省 / `-` = stdout）
        out: Option<String>,
    },
}

/// 运行 persona 子命令分发。
///
/// 参数:
/// - `engine`: 服务层引擎引用。
/// - `cmd`: Persona 子命令。
/// - `json`: JSON 信封输出。
pub async fn run(engine: &Arc<Engine>, cmd: PersonaCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        PersonaCmd::List { limit, offset } => run_list(engine, json, limit, offset).await,
        PersonaCmd::Show => run_show(engine, json).await,
        PersonaCmd::Reload { uid } => run_reload(engine, uid).await,
        PersonaCmd::Import {
            file,
            dry_run,
            into,
            no_ai,
        } => run_import(engine, file, dry_run, into, no_ai, json).await,
        PersonaCmd::AllowInference { uid, labels } => {
            run_allow_inference(engine, uid, labels, json).await
        }
        PersonaCmd::Prompt { uid, input } => run_prompt(engine, uid, input, json).await,
        PersonaCmd::Export { uid, out } => export::run(engine, uid, out, json).await,
    }
}

// =========================================================
// list 命令
// =========================================================

/// 结构化列出所有人格（uid / 名称 / kind / source / active）。
///
/// - `--json`: 输出 `{"personas":[{uid,name,kind,source,active}...]}` 信封。
/// - 文本模式：紧凑表格（uid / 名称 / kind / 来源 / 状态）。
/// - 支持 --limit/--offset 分页（列表命令统一约定）。
async fn run_list(
    engine: &Arc<Engine>,
    json: bool,
    limit: Option<usize>,
    offset: usize,
) -> anyhow::Result<()> {
    let personas = engine
        .persona_list_full()
        .await
        .context("查询 persona 列表失败")?;

    // 分页：先跳过 offset 条，再取 limit 条（limit=None 表示全部）
    let page_limit = limit.unwrap_or(usize::MAX);
    let paged: Vec<_> = personas.iter().skip(offset).take(page_limit).collect();

    if json {
        let items: Vec<serde_json::Value> = paged
            .iter()
            .map(|p| {
                serde_json::json!({
                    "uid": p.uid,
                    "name": p.name,
                    "kind": p.kind,
                    "source": p.source,
                    "active": p.is_active,
                })
            })
            .collect();
        let data = serde_json::json!({ "personas": items });
        return crate::json::emit_ok(&data);
    }

    if paged.is_empty() {
        crate::ui::info("暂无已注册的人格");
        return Ok(());
    }

    println!();
    println!(
        "  {:<16}  {:<20}  {:<6}  {:<10}  状态",
        "UID", "名称", "Kind", "来源"
    );
    println!(
        "  {:-<16}  {:-<20}  {:-<6}  {:-<10}  {:-<6}",
        "", "", "", "", ""
    );
    for p in &paged {
        println!(
            "  {:<16}  {:<20}  {:<6}  {:<10}  {}",
            p.uid,
            p.name,
            p.kind,
            p.source,
            if p.is_active { "活跃" } else { "停用" }
        );
    }
    Ok(())
}

// =========================================================
// show 命令
// =========================================================

/// 展示所有已注册人格的基本信息。
///
/// 说明:
/// - 从服务层读取人格全字段列表。
/// - 解析 config 字段中的 TOML 内容，提取 assistant_name 和人设/规则摘要。
/// - 无 persona 时输出引导提示。
async fn run_show(engine: &Arc<Engine>, json: bool) -> anyhow::Result<()> {
    let personas = engine
        .persona_list_full()
        .await
        .context("查询 persona 列表失败")?;

    if json {
        // show 的 JSON 形态与 list 一致（均为 persona 集合），供脚本统一消费
        let items: Vec<serde_json::Value> = personas
            .iter()
            .map(|p| {
                serde_json::json!({
                    "uid": p.uid,
                    "name": p.name,
                    "kind": p.kind,
                    "source": p.source,
                    "active": p.is_active,
                    "config": p.config,
                })
            })
            .collect();
        let data = serde_json::json!({ "personas": items });
        return crate::json::emit_ok(&data);
    }

    if personas.is_empty() {
        crate::ui::info("暂无已注册的人格");
        crate::ui::info(&format!("人格文件目录: {}", personas_dir().display()));
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
        personas_dir().display()
    ));
    Ok(())
}

/// 格式化输出单个 persona。
fn display_persona(p: &PersonaFullView) {
    println!("  UID:     {}", p.uid);
    println!("  名称:    {}", p.name);
    println!("  类型:    {}", p.kind);
    println!("  状态:    {}", if p.is_active { "活跃" } else { "已停用" });

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
/// 3. 逐个文件读取 → 解析 assistant_name → 创建或更新 DB 记录（已存在则同步文件内容）。
/// 4. 输出加载结果摘要。
async fn run_reload(engine: &Arc<Engine>, uid: Option<String>) -> anyhow::Result<()> {
    let dir = personas_dir();

    if !dir.exists() {
        return Err(anyhow::anyhow!(
            "人格文件目录不存在: {}\n\
             请确认工作目录在仓库根下，或将 .toml 文件放入 config/personas/ 目录。",
            dir.display()
        ));
    }
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("路径不是目录: {}", dir.display()));
    }

    // 收集要处理的文件（文件名用于逐文件结果展示）
    let files = collect_toml_files(&dir, uid.as_deref())?;

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

    // 导入用例：已存在记录按文件内容同步（名称 / 配置）
    let outcomes = engine
        .persona_load_from_dir(&dir, uid.as_deref(), PersonaLoadMode::CreateOrUpdate)
        .await?;

    let mut success_count = 0u32;
    let mut error_count = 0u32;

    for (path, outcome) in files.iter().zip(outcomes.iter()) {
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".to_string());

        match outcome.action {
            PersonaFileAction::Created | PersonaFileAction::Updated => {
                crate::ui::success(&format!("{filename} → {}", outcome.uid));
                success_count += 1;
            }
            PersonaFileAction::Failed => {
                crate::ui::warn(&format!("{filename}: {}", outcome.message));
                tracing::error!(
                    path = %path.display(),
                    error = %outcome.message,
                    "人格文件加载失败"
                );
                error_count += 1;
            }
            // 其余动作（如仅创建缺失模式的跳过）在本命令口径下不会出现，防御性忽略
            _ => {}
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

// =========================================================
// import 命令
// =========================================================

/// 从预设文件导入人格（dry-run 预览 / 执行写入）。
///
/// 说明:
/// - dry-run 仅解析与裁决，不调用 LLM、不写库；
/// - `--into` 指向的目标不存在时按新建处理，`--no-ai` 关闭 LLM 拆解；
/// - JSON 模式输出预览 / 导入结果信封，stdout 仅含数据；文本模式输出预览与写入摘要。
/// - 失败错误上抛，由入口统一映射退出码。
async fn run_import(
    engine: &Arc<Engine>,
    file: PathBuf,
    dry_run: bool,
    into: Option<String>,
    no_ai: bool,
    json: bool,
) -> anyhow::Result<()> {
    if dry_run {
        let preview = engine
            .persona_import_preview(&file, into.as_deref(), no_ai)
            .await
            .context("预设预览失败")?;
        if json {
            return crate::json::emit_ok(&preview);
        }
        // render_text 自带换行；预览数据走 stdout
        print!("{}", preview.render_text());
        return Ok(());
    }

    let outcome = engine
        .persona_import(&file, into.as_deref(), no_ai)
        .await
        .context("预设导入失败")?;
    if json {
        return crate::json::emit_ok(&outcome);
    }
    print!("{}", outcome.preview.render_text());
    print!("{}", render_outcome(&outcome));
    if outcome.written.failed > 0 {
        crate::ui::warn(&format!(
            "存在 {} 条写入失败（已跳过，其余条目照常写入）",
            outcome.written.failed
        ));
    }
    Ok(())
}

// =========================================================
// allow-inference 命令
// =========================================================

/// 允许推断接管：将选定 Manual 画像的来源改写为 Inferred。
///
/// 说明:
/// - `--label` 可重复（精确匹配）；缺省 = 全部 Manual 画像；
/// - `--json`: 输出 `{"uid":..., "updated":n}` 信封；
/// - 文本模式：n > 0 输出改写条数；n == 0 输出未找到提示；
/// - 失败错误上抛，由入口统一映射退出码。
async fn run_allow_inference(
    engine: &Arc<Engine>,
    uid: String,
    labels: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let updated = engine
        .persona_allow_inference(&uid, &labels)
        .await
        .context("允许推断接管失败")?;

    if json {
        return crate::json::emit_ok(&serde_json::json!({"uid": uid, "updated": updated}));
    }

    if updated > 0 {
        println!("已将 {updated} 条 Manual 画像转为可被推断覆盖（uid: {uid}）");
    } else {
        crate::ui::info("未找到匹配的 Manual 画像（无改写）");
    }
    Ok(())
}

// =========================================================
// prompt 命令
// =========================================================

/// 查看指定人格的 system prompt 渲染结果（静态段 / 全量两档）。
///
/// 说明:
/// - 缺省为静态段预览（不含检索内容）；`--input` 提供给定输入时走全量预览（含记忆召回）；
/// - `--json`: 输出单行信封（data = 预览对象）；文本模式 stdout 只含渲染结果与记忆上下文；
/// - 档位与度量走 stderr；失败错误上抛，由入口统一映射退出码。
async fn run_prompt(
    engine: &Arc<Engine>,
    uid: String,
    input: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let preview = engine
        .persona_prompt_preview(&uid, input.as_deref())
        .await
        .context("提示词预览失败")?;

    if json {
        return crate::json::emit_ok(&preview);
    }

    println!("{}", preview.system_prompt);
    if let Some(context) = preview
        .memory_context
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        println!("\n\n----- 记忆上下文（随请求单独注入）-----");
        println!("{context}");
    }

    let tier_label = match preview.tier {
        ramaria_service::PromptPreviewTier::Static => "静态段预览",
        ramaria_service::PromptPreviewTier::Full => "全量预览（含记忆召回）",
    };
    crate::ui::info(&format!(
        "{tier_label}（字符 {} / 约 {} tokens）",
        preview.chars, preview.tokens
    ));
    Ok(())
}

/// 渲染导入结果摘要（人格行 + 写入统计 + 拆解行）。
fn render_outcome(outcome: &PersonaImportOutcome) -> String {
    let reg = &outcome.preview.registration;
    let uid = reg.uid.as_deref().unwrap_or("（自动生成）");
    let mode = match reg.mode {
        PreviewMode::Create => "新建",
        PreviewMode::Merge => "合并",
    };
    let written = &outcome.written;
    let mut out = format!("导入完成：persona {uid}（{mode}）\n");
    out.push_str(&format!(
        "  已写入：traits {} / facts {} / examples {} / behavior_rules {}（失败 {}）\n",
        written.traits, written.facts, written.examples, written.behavior_rules, written.failed
    ));
    out.push_str(&format!(
        "  拆解：{}\n",
        decompose_label(&outcome.decompose)
    ));
    out
}

/// 拆解执行行文本（未启用 / 已执行 / 跳过原因）。
fn decompose_label(exec: &DecomposeExecution) -> String {
    if exec.executed {
        return format!(
            "已执行（traits {} / facts {}）",
            exec.trait_count, exec.fact_count
        );
    }
    if !exec.requested {
        return "未启用".to_string();
    }
    let reason = match exec.skipped_reason.as_deref() {
        Some("privacy_unconfirmed") => "隐私未确认",
        Some("no_input") => "无拆解输入",
        Some("llm_failed") => "LLM 调用失败",
        Some(other) => other,
        None => "未执行",
    };
    format!("已跳过（{reason}）")
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

/// 将文本截断为指定最大字符数，添加统一省略号 `…`。
///
/// 字符边界与省略号语义由 `ramaria_core::text::truncate_chars` 统一实现，不破坏 UTF-8。
fn summarize_block(text: &str, max_chars: usize) -> String {
    // 先压缩多余空白为单个空格，便于单行展示
    let compressed: String = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    ramaria_core::text::truncate_chars(&compressed, max_chars)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
