//! crates/ramaria-cli/src/commands/import_cmd.rs - 数据导入命令
//!
//! 设计特点:
//! - `ramaria import qq --file <PATH> [--deep] [--dry-run] [--persona-self-name <NAME>] [--persona-other-name <NAME>] [--gap <MINUTES>]`
//! - 快速导入（默认）：仅写入 messages 表（L0），适合快速预览历史对话
//! - 深度导入（--deep）：创建历史 session → 写入 L0 → 关闭 session（L1/L2/L3 由后台线程触发）
//! - `--dry-run`：仅解析预览（输出结构化 JSON 摘要，不写入数据库），供 agent 验证数据源
//! - 双画像支持——分别为导出者和对方创建独立 persona
//! - `--persona` 向后兼容，行为等同于 `--persona-self-name`
//! - L1 摘要 persona_uid 存 NULL，不绑定特定画像（避免记忆视图污染）
//! - Persona 自动管理：查找或创建 source="qq" 的 persona（UID 生成策略: uin > uid > seq）
//! - 解析报告输出到 stderr 提示，数据输出遵循 stdout 纯净性（--json 信封）
//! - 确认规则（M1 B 项）：`--yes` 自动确认；非 TTY 且无 `--yes` 不挂起、直接失败提示
//! - 使用 ramaria-importer crate 做格式检测、解析和写入
//! - 仅支持 qq-chat-exporter v6.x JSON 格式（语义化 type 名称）

use anyhow::Context;
use ramaria_importer::ImportSource;
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;

// =========================================================
// 导入参数
// =========================================================

/// CLI 导入命令的参数。
/// 新增双画像参数（self/other 两方独立命名和 UID 指定）。
pub struct ImportArgs {
    /// QQ 聊天记录文件路径（qq-chat-exporter v6.x JSON 格式）
    pub file: String,
    /// 导入模式：fast（默认，仅 L0）或 deep（全管线）
    pub deep: bool,
    /// 仅解析预览（不写入数据库，输出结构化 JSON 摘要）
    pub dry_run: bool,
    /// 导出者 persona 显示名称（向后兼容 `--persona`，不提供则使用文件中解析的导出者名称）
    pub persona_self_name: Option<String>,
    /// 导出者 persona UID（可选，留空则按优先级自动生成）
    pub persona_self_uid: Option<String>,
    /// 对方 persona 显示名称（不提供则使用文件中解析的对方名称）
    pub persona_other_name: Option<String>,
    /// 对方 persona UID（可选，留空则按优先级自动生成）
    pub persona_other_uid: Option<String>,
    /// session 切割时间间隔（分钟），默认 10
    pub gap: u32,
    /// 跳过确认提示
    pub yes: bool,
    /// JSON 信封输出
    pub json: bool,
}

// =========================================================
// run — 导入命令入口
// =========================================================

/// 执行 QQ 聊天记录导入。
/// 参数:
/// - `app`: 应用实例（用于触发 L1 摘要生成）。
/// - `pool`: 数据库连接池引用。
/// - `args`: 导入参数（含双画像选项）。
///   流程:
/// 1. 校验文件路径和扩展名
/// 2. 格式检测（qq-chat-exporter JSON）
/// 3. 文件解析 → 诊断报告输出（含双方标识信息）
/// 4. 用户确认（非 --yes 模式）
/// 5. 双画像 Persona 准备（self + other 各调用一次 ensure_qq_persona）
/// 6. 执行导入（fast/deep，按发送者分配 persona_uid）
/// 7. 为每个导入的 session 触发 L1 摘要生成（persona_uid=NULL，不绑定特定画像）
/// 8. 结果输出
pub async fn run(
    app: &Arc<ramaria_app::App>,
    pool: &SqlitePool,
    args: ImportArgs,
) -> anyhow::Result<()> {
    let path = Path::new(&args.file);

    // Step 1: 文件校验（业务校验失败，exit code 4）
    if !path.exists() {
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!("文件不存在: {}", args.file))
        ));
    }
    if !path.is_file() {
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!("路径不是文件: {}", args.file))
        ));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" {
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!(
                "不支持的文件类型: .{}（仅支持 qq-chat-exporter v6.x 导出的 .json 格式）",
                ext
            ))
        ));
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
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!(
                "文件 '{}' 不是 QQ 聊天记录格式。\n\
                 请确认文件来自 shuakami/qq-chat-exporter v6.x 导出的 JSON 文件。",
                args.file
            ))
        ));
    }

    // Step 3: 文件解析
    let (sessions, report) = importer.parse(path, args.gap).context("文件解析失败")?;

    // 打印解析报告（stderr 提示，不污染 stdout 数据流）
    crate::ui::info("📊 解析报告:");
    eprintln!("{}", report.summary());

    if sessions.is_empty() {
        // --json 模式：输出空数据信封（agent 可区分“成功但无数据”与异常）
        if args.json {
            let data = serde_json::json!({
                "imported": false,
                "sessions_written": 0,
                "messages_written": 0,
                "reason": "no_importable_messages",
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::warn("⚠️  文件中没有可导入的消息。导入已取消。");
        return Ok(());
    }

    // Step 3.5: --dry-run 仅解析预览，不写入数据库（agent 验证数据源）
    if args.dry_run {
        let preview = serde_json::json!({
            "dry_run": true,
            "file": args.file,
            "mode": if args.deep { "deep" } else { "fast" },
            "gap_minutes": args.gap,
            "sessions": sessions.len(),
            "messages": report.total_success() + report.total_degraded(),
            "self": {
                "name": report.self_name,
                "uin": report.self_uin,
                "id": report.self_id,
            },
            "other": {
                "name": report.other_name,
                "uin": report.other_uin,
                "uid": report.other_uid,
            },
            "skipped": {
                "total": report.total_skipped(),
                "recalled": report.skipped_recalled,
                "empty": report.skipped_empty,
                "unknown": report.skipped_unknown,
            },
        });
        if args.json {
            return crate::json::emit_ok(&preview);
        }
        // 文本预览：输出结构化摘要（数据部分走 stdout，提示走 stderr）
        println!("{}", serde_json::to_string_pretty(&preview)?);
        crate::ui::info("💡 --dry-run 模式：未写入任何数据。确认无误后去掉 --dry-run 执行导入。");
        return Ok(());
    }

    // Step 4: 确认（非 --yes 模式；非 TTY 无 --yes 直接失败不挂起）
    if !args.yes {
        let proceed = crate::ui::confirm(
            &format!(
                "确认导入 {} 个 session（共 {} 条消息）?",
                sessions.len(),
                report.total_success() + report.total_degraded()
            ),
            args.yes,
        )
        .map_err(|e| ramaria_core::error::RamariaError::validation(e.to_string()))?;
        if !proceed {
            crate::ui::info("导入已取消");
            return Ok(());
        }
    }

    // Step 5: 双画像 Persona 准备
    use ramaria_importer::qq::build_persona_uid;

    // 5a. 查询 QQ persona 当前最大 seq（用于 fallback 级别 4）
    let all_personas = ramaria_storage::repo::personas::list_all(pool)
        .await
        .context("查询已有 persona 列表失败")?;
    let max_qq_seq: u32 = all_personas
        .iter()
        .filter(|p| p.source == "qq")
        .map(|p| p.seq as u32)
        .max()
        .unwrap_or(0);

    // 5b. 导出者（self）persona
    let self_name = args
        .persona_self_name
        .clone()
        .unwrap_or_else(|| report.self_name.clone());
    let self_uid = build_persona_uid(
        args.persona_self_uid.as_deref(),
        report.self_uin.as_deref(),
        &report.self_id,
        max_qq_seq + 1,
    );
    let self_persona_uid =
        ramaria_importer::qq::ensure_qq_persona(pool, &self_uid, &self_name, Some(&report.self_id))
            .await
            .context("创建/查找导出者 persona 失败")?;

    crate::ui::info(&format!("👤 导出者: {} ({})", self_name, self_persona_uid));

    // 5c. 对方（other）persona
    let other_name = args.persona_other_name.clone().unwrap_or_else(|| {
        if report.other_name.is_empty() {
            report.chat_name.clone()
        } else {
            report.other_name.clone()
        }
    });
    let other_ref_id = if report.other_uid.is_empty() {
        None
    } else {
        Some(report.other_uid.as_str())
    };
    // 对方 seq 在 self 之后递增
    let next_seq = max_qq_seq + 2;
    let other_default_uid = build_persona_uid(
        args.persona_other_uid.as_deref(),
        report.other_uin.as_deref(),
        &report.other_uid,
        next_seq,
    );
    let other_persona_uid = ramaria_importer::qq::ensure_qq_persona(
        pool,
        &other_default_uid,
        &other_name,
        other_ref_id,
    )
    .await
    .context("创建/查找对方 persona 失败")?;

    crate::ui::info(&format!(
        "👤 对话对方: {} ({})",
        other_name, other_persona_uid
    ));

    // Step 6: 执行导入
    if args.deep {
        crate::ui::info("🔄 执行深度导入（L0 → 触发 L1 摘要生成）...");
    } else {
        crate::ui::info("⚡ 执行快速导入（L0 → 触发 L1 摘要生成）...");
    }

    let (sessions_written, messages_written, session_ids) =
        ramaria_importer::qq::QqImporter::execute_fast_import(
            pool,
            &sessions,
            &self_persona_uid,
            &other_persona_uid,
            &report.self_id,
        )
        .await
        .context("导入写入失败")?;

    // Step 6.5: 为每个导入的 session 触发 L1 摘要生成
    // (T-V11-5B-010): L1 摘要 persona_uid 存 NULL
    // —— 导入的 session 来自多人对话，摘要不应被特定画像视图独占
    let mut l1_ok = 0u32;
    let mut l1_skip = 0u32;
    let mut l1_err = 0u32;
    for sid in &session_ids {
        match app.regenerate_l1(*sid, None, None, None).await {
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

    if args.json {
        let data = serde_json::json!({
            "imported": true,
            "sessions_written": sessions_written,
            "messages_written": messages_written,
            "session_ids": session_ids.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "l1": {"ok": l1_ok, "skip": l1_skip, "err": l1_err},
            "skipped": report.total_skipped(),
        });
        return crate::json::emit_ok(&data);
    }

    Ok(())
}
