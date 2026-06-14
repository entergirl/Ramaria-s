//! rust/crates/ramaria-cli/src/commands/import_cmd.rs - 数据导入命令
//!
//! 设计特点:
//! - `ramaria import qq --file <PATH> [--deep] [--persona <NAME>] [--gap <MINUTES>]`
//! - 快速导入（默认）：仅写入 messages 表（L0），适合快速预览历史对话
//! - 深度导入（--deep）：创建历史 session → 写入 L0 → 关闭 session（L1/L2/L3 由后台线程触发）
//! - Persona 自动管理：查找或创建 source="qq" 的 persona
//! - 解析报告输出到 stdout，含成功/降级/跳过统计
//! - 支持 `--yes` 全局参数跳过确认提示
//! - 使用 ramaria-importer crate 做格式检测、解析和写入

use anyhow::Context;
use ramaria_importer::ImportSource;
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;

// =========================================================
// 导入参数
// =========================================================

/// CLI 导入命令的参数。
pub struct ImportArgs {
    /// QQ 聊天记录文件路径（JSON 或 .txt 格式）
    pub file: String,
    /// 导入模式：fast（默认，仅 L0）或 deep（全管线）
    pub deep: bool,
    /// 导入关联的 persona 显示名称（不提供则使用导出者名称）
    pub persona: Option<String>,
    /// session 切割时间间隔（分钟），默认 10
    pub gap: u32,
    /// 跳过确认提示
    pub yes: bool,
}

// =========================================================
// run — 导入命令入口
// =========================================================

/// 执行 QQ 聊天记录导入。
///
/// 参数:
/// - `app`: 应用实例（用于触发 L1 摘要生成）。
/// - `pool`: 数据库连接池引用。
/// - `args`: 导入参数。
///
/// 流程:
/// 1. 校验文件路径和扩展名
/// 2. 格式检测（JSON 或 .txt）
/// 3. 文件解析 → 诊断报告输出
/// 4. 用户确认（非 --yes 模式）
/// 5. Persona 准备
/// 6. 执行导入（fast/deep）
/// 7. 为每个导入的 session 触发 L1 摘要生成
/// 8. 结果输出
pub async fn run(
    app: &Arc<ramaria_app::App>,
    pool: &SqlitePool,
    args: ImportArgs,
) -> anyhow::Result<()> {
    let path = Path::new(&args.file);

    // Step 1: 文件校验
    if !path.exists() {
        anyhow::bail!("文件不存在: {}", args.file);
    }
    if !path.is_file() {
        anyhow::bail!("路径不是文件: {}", args.file);
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" && ext != "txt" {
        anyhow::bail!(
            "不支持的文件类型: .{}（仅支持 .json 和 .txt 格式的 QQ 聊天记录）",
            ext
        );
    }

    let mode = if args.deep { "深度" } else { "快速" };
    crate::ui::info(&format!(
        "🔍 正在分析文件: {} ({}导入模式, 切割间隔 {} 分钟)",
        args.file, mode, args.gap
    ));

    // Step 2: 格式检测
    let importer = ramaria_importer::qq::QqImporter::new();

    let is_qq = importer.detect_format(path).context("格式检测失败")?;

    if !is_qq {
        anyhow::bail!(
            "文件 '{}' 不是 QQ 聊天记录格式。\n\
             请确认文件来自: \n\
             - shuakami/qq-chat-exporter 导出的 JSON 文件\n\
             - PC QQ 消息管理器的 .txt 导出",
            args.file
        );
    }

    // Step 3: 文件解析
    let (sessions, report) = importer.parse(path, args.gap).context("文件解析失败")?;

    // 打印解析报告
    crate::ui::info("📊 解析报告:");
    println!("{}", report.summary());

    if sessions.is_empty() {
        crate::ui::warn("⚠️  文件中没有可导入的消息。导入已取消。");
        return Ok(());
    }

    // Step 4: 确认（非 --yes 模式）
    if !args.yes {
        let proceed = crate::ui::confirm(&format!(
            "确认导入 {} 个 session（共 {} 条消息）?",
            sessions.len(),
            report.total_success() + report.total_degraded()
        ))
        .context("读取用户输入失败")?;
        if !proceed {
            crate::ui::info("导入已取消");
            return Ok(());
        }
    }

    // Step 5: Persona 准备
    let persona_name = args
        .persona
        .clone()
        .unwrap_or_else(|| report.self_name.clone());

    let persona_uid = ramaria_importer::qq::ensure_qq_persona(
        pool,
        &format!("char-{}", &report.self_id),
        &persona_name,
        Some(&report.self_id),
    )
    .await
    .context("创建/查找 persona 失败")?;

    crate::ui::info(&format!("👤 Persona: {} ({})", persona_name, persona_uid));

    // Step 6: 执行导入
    if args.deep {
        crate::ui::info("🔄 执行深度导入（L0 → 触发 L1 摘要生成）...");
    } else {
        crate::ui::info("⚡ 执行快速导入（L0 → 触发 L1 摘要生成）...");
    }

    let (sessions_written, messages_written, session_ids) =
        ramaria_importer::qq::QqImporter::execute_fast_import(pool, &sessions, &persona_uid)
            .await
            .context("导入写入失败")?;

    // Step 6.5: 为每个导入的 session 触发 L1 摘要生成
    let mut l1_ok = 0u32;
    let mut l1_skip = 0u32;
    let mut l1_err = 0u32;
    for sid in &session_ids {
        match app.regenerate_l1(*sid, Some(&persona_uid)).await {
            Ok(Some(_)) => l1_ok += 1,
            Ok(None) => l1_skip += 1,
            Err(e) => {
                l1_err += 1;
                tracing::warn!(%sid, error = %e, "L1 摘要生成失败（非致命）");
            }
        }
    }
    if l1_ok > 0 || l1_err > 0 {
        crate::ui::info(&format!(
            "📝 L1 摘要: {} 成功, {} 跳过（空会话）, {} 失败",
            l1_ok, l1_skip, l1_err
        ));
    }

    // Step 6.6: 深度模式触发 L2→L3 级联；快速模式跳过（留给用户稍后手动触发）
    if args.deep && l1_ok > 0 {
        crate::ui::info("🔍 深度导入模式：触发 L2 事件提取 → L3 人格画像...");
        app.trigger_l2_check().await;
    }

    // Step 7: 结果输出
    crate::ui::success(&format!(
        "✅ 导入完成: {} 个 session，{} 条消息",
        sessions_written, messages_written
    ));

    if report.total_skipped() > 0 {
        crate::ui::warn(&format!(
            "⚠️  跳过的消息: {} 条（撤回 {}，空内容 {}，未知类型 {}）",
            report.total_skipped(),
            report.skipped_recalled,
            report.skipped_empty,
            report.skipped_unknown,
        ));
    }

    crate::ui::info(
        "💡 可使用 'ramaria memory --layer l1' 查看已生成的 L1 摘要记忆。\n\
             L2 事件和 L3 性格画像由后台线程定时处理。",
    );

    Ok(())
}
