//! crates/ramaria-cli/src/commands/import_cmd.rs - 数据导入命令
//!
//! 设计特点:
//! - `ramaria import qq --file <PATH> [--deep] [--dry-run] [--persona-self-name <NAME>] [--persona-other-name <NAME>] [--gap <MINUTES>]`
//! - 快速导入（默认）：仅写入 messages 表（L0），适合快速预览历史对话
//! - 深度导入（--deep）：L0 写入 → L1 摘要 → 触发 L2/L3 级联
//! - `--dry-run`：仅解析预览（输出结构化 JSON 摘要，不写入数据库），供 agent 验证数据源
//! - 双画像支持——分别为导出者和对方创建独立 persona（由服务层导入用例承担）
//! - `--persona` 向后兼容，行为等同于 `--persona-self-name`
//! - L1 摘要按参与者分发（私聊双侧各一行、群聊他人行），不产生无归属摘要
//! - 解析报告默认以掩码版输出到 stderr 提示（`--no-report` 可关闭），数据输出遵循 stdout 纯净性（--json 信封）
//! - 确认规则：`--yes` 自动确认；非 TTY 且无 `--yes` 不挂起、直接失败提示
//! - 仅支持 qq-chat-exporter v6.x JSON 格式（语义化 type 名称）

use anyhow::Context;
use ramaria_core::privacy::mask_id;
use ramaria_service::{
    AnalysisReport, AnalyzeRequest, Engine, ImportContinueRequest, ImportDoneSummary,
    ImportL1Progress, ImportMode, ImportPostPlan, ImportPostRequest, ImportProgressSink,
    ImportRequest,
};
use std::path::PathBuf;
use std::sync::Arc;

// =========================================================
// 导入参数
// =========================================================

/// CLI 导入命令的参数。
/// 新增双画像参数（self/other 两方独立命名和 UID 指定）+ 导入侧过滤（--side）。
pub struct ImportArgs {
    /// QQ 聊天记录文件路径（qq-chat-exporter v6.x JSON 格式）
    /// 使用 PathBuf 而非 String：Windows 下中文/非 UTF-8 路径经 clap String
    /// 解析会做 UTF-8 校验失败或乱码，PathBuf 保留原生 OsString 语义。
    pub file: PathBuf,
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
    /// 导入侧过滤：self|other|both，默认 both
    pub side: ramaria_importer::qq::ImportSide,
    /// 跳过解析报告输出（报告含导出者/对方标识，默认输出为掩码版）
    pub no_report: bool,
    /// 跳过确认提示
    pub yes: bool,
    /// JSON 信封输出
    pub json: bool,
}

// =========================================================
// 解析报告摘要
// =========================================================

/// 构造掩码版解析报告摘要（stderr 提示；不输出昵称 / 账号原值）。
///
/// 参数:
/// - `report`: 服务层解析预览报告。
///
/// 返回:
/// - 多行摘要文本；个人标识经 `mask_id` 掩码，空值显示"未提供"。
fn analysis_report_summary(report: &AnalysisReport) -> String {
    let mask_opt = |value: Option<&str>| match value.map(str::trim) {
        Some(v) if !v.is_empty() => mask_id(v),
        _ => "未提供".to_string(),
    };
    format!(
        "导出者: {} ({})\n对话: {} ({})\n时间范围: {}\n会话: {}（切割间隔 {} 分钟）\n消息: 原始 {} / 成功 {} / 降级 {} / 跳过 {}",
        mask_id(&report.self_name),
        mask_opt(report.self_uin.as_deref()),
        mask_id(&report.chat_name),
        report.chat_type,
        report.time_range,
        report.session_count,
        report.gap_minutes,
        report.total_raw,
        report.total_success,
        report.total_degraded,
        report.total_skipped,
    )
}

// =========================================================
// run — 导入命令入口
// =========================================================

/// 执行 QQ 聊天记录导入。
///
/// 参数:
/// - `engine`: 服务层引擎（L0 写入、L1 摘要与深度触发经服务层用例）。
/// - `args`: 导入参数（含双画像选项）。
///
/// 流程:
/// 1. 校验文件路径和扩展名
/// 2. 格式检测（qq-chat-exporter JSON）
/// 3. 文件解析 → 诊断报告输出（默认掩码版，`--no-report` 关闭）
/// 4. 用户确认（非 --yes 模式）
/// 5. L0 导入（服务层用例：双画像准备 + 会话 / 消息写入）
/// 6. 导入后处理（服务层单一编排：图片理解 → L1 摘要生成 → 深度模式触发 L2→L3 级联）
/// 7. 结果输出
pub async fn run(engine: &Arc<Engine>, args: ImportArgs) -> anyhow::Result<()> {
    let path = args.file.as_path();

    // Step 1: 文件校验（业务校验失败，exit code 4）
    if !path.exists() {
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!(
                "文件不存在: {}",
                args.file.display()
            ))
        ));
    }
    if !path.is_file() {
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!(
                "路径不是文件: {}",
                args.file.display()
            ))
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
                "不支持的文件类型: .{}（仅支持 qq-chat-exporter v6.x 导出的 .json 格式；含图片的导出请保留 resources/ 目录）",
                ext
            ))
        ));
    }

    let mode = if args.deep { "深度" } else { "快速" };
    crate::ui::info(&format!(
        "🔍 正在分析文件: {} ({}导入模式, 切割间隔 {} 分钟)",
        args.file.display(),
        mode,
        args.gap
    ));

    // Step 2: 格式检测（经服务层用例，入口不直连导入器）
    let is_qq = engine
        .detect_qq_format(path)
        .await
        .context("格式检测失败")?;

    if !is_qq {
        return Err(anyhow::anyhow!(
            ramaria_core::error::RamariaError::validation(format!(
                "文件 '{}' 不是 QQ 聊天记录格式。\n\
                 请确认文件来自 shuakami/qq-chat-exporter v6.x 导出的 JSON 文件。",
                args.file.display()
            ))
        ));
    }

    // Step 3: 文件解析预览（经服务层用例；统计与掩码摘要由入口渲染）
    let report = engine
        .analyze_qq_import(AnalyzeRequest {
            file_path: args.file.clone(),
            gap_minutes: args.gap,
        })
        .await
        .context("文件解析失败")?;

    // 打印解析报告（stderr 提示，不污染 stdout 数据流；默认掩码版，避免昵称/账号标识落盘）
    if !args.no_report {
        crate::ui::info("📊 解析报告:");
        crate::ui::info(&analysis_report_summary(&report));
    }

    if report.session_count == 0 {
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
            "file": args.file.display().to_string(),
            "mode": if args.deep { "deep" } else { "fast" },
            "gap_minutes": args.gap,
            "side": match args.side {
                ramaria_importer::qq::ImportSide::Me => "self",
                ramaria_importer::qq::ImportSide::Other => "other",
                ramaria_importer::qq::ImportSide::Both => "both",
            },
            "sessions": report.session_count,
            "messages": report.total_success + report.total_degraded,
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
                "total": report.total_skipped,
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
                report.session_count,
                report.total_success + report.total_degraded
            ),
            args.yes,
        )
        .map_err(|e| ramaria_core::error::RamariaError::validation(e.to_string()))?;
        if !proceed {
            crate::ui::info("导入已取消");
            return Ok(());
        }
    }

    // Step 5: L0 导入（服务层用例：解析 → 双画像准备 → 会话 / 消息写入）
    let outcome = engine
        .import_qq_l0(ImportRequest {
            file_path: args.file.clone(),
            mode: if args.deep {
                ImportMode::Deep
            } else {
                ImportMode::Fast
            },
            gap_minutes: args.gap,
            side: args.side,
            persona_name: args.persona_self_name.clone(),
            self_persona_uid: args.persona_self_uid.clone(),
            other_persona_name: args.persona_other_name.clone(),
            other_persona_uid: args.persona_other_uid.clone(),
        })
        .await
        .context("导入写入失败")?;

    // 画像准备结果回显（按导入侧过滤：跳过侧不创建 persona；群聊展示成员分布）
    match &outcome.persona_uid {
        Some(uid) => crate::ui::info(&format!("👤 导出者: {} ({})", outcome.persona_name, uid)),
        None => crate::ui::info(&format!(
            "⏭️  跳过导出者 persona（--side {} 不处理我方）",
            "other"
        )),
    }
    match &outcome.other_persona_uid {
        Some(uid) => crate::ui::info(&format!(
            "👤 对话对方: {} ({})",
            outcome.other_persona_name, uid
        )),
        None => {
            if outcome.chat_type == "group" {
                // 群聊无单一对方：展示成员数与 Top 3 显示名（个人标识经掩码）
                let top: Vec<String> = outcome
                    .members
                    .iter()
                    .take(3)
                    .map(|m| mask_id(&m.name))
                    .collect();
                crate::ui::info(&format!(
                    "👥 群聊导入：成员 {} 人（{}）",
                    outcome.members.len(),
                    top.join("、")
                ));
            } else {
                crate::ui::info(&format!(
                    "⏭️  跳过对方 persona（--side {} 不处理对方）",
                    "self"
                ));
            }
        }
    }

    // Step 5.5: 导入后处理（服务层单一编排：图片理解 → L1 摘要生成 → 深度触发；
    // 图片理解先于 L1；能力不可达时静默跳过并保持待处理）
    // 分发口径：恒含"我"（导出者）在内、块内参与者各一行；
    // 每会话一次生成、块内参与者复制分发行（各自持有摘要）
    if args.deep {
        crate::ui::info("🔄 执行深度导入（L0 → 触发 L1 摘要生成）...");
    } else {
        crate::ui::info("⚡ 执行快速导入（L0 → 触发 L1 摘要生成）...");
    }

    let post = engine
        .run_import_post(
            ImportPostRequest {
                session_ids: outcome.session_ids.clone(),
                export_root: args.file.parent().map(|p| p.to_path_buf()),
                plan: ImportPostPlan {
                    l1_targets: Vec::new(),
                    l1_prefix: None,
                    group_fanout: true,
                    fanout_include_self: true,
                    l1_cascade: true,
                    cascade_deep: args.deep,
                    throttle_ms: 0,
                },
            },
            None,
        )
        .await
        .context("导入后处理失败")?;

    // 图片理解统计：仅扫描到附件时展示
    if let Some(stat) = post.vision.filter(|stat| stat.scanned > 0) {
        crate::ui::info(&format!(
            "🖼️  图片理解: {} 完成, {} 复用, {} 跳过, {} 失败",
            stat.done, stat.reused, stat.skipped, stat.failed
        ));
    }

    let l1_ok = post.l1.l1_success;
    let l1_skip = post.l1.l1_skipped;
    let l1_err = post.l1.l1_failed;
    if l1_ok > 0 || l1_err > 0 {
        crate::ui::info(&format!(
            "📝 L1 摘要: {} 成功, {} 跳过（空会话）, {} 失败",
            l1_ok, l1_skip, l1_err
        ));
    }

    // 深度模式触发 L2→L3 级联；快速模式跳过（留给用户稍后手动触发）
    if post.l2_triggered {
        crate::ui::info("🔍 深度导入模式：触发 L2 事件提取 → L3 人格画像...");
    }

    // Step 7: 结果输出
    crate::ui::success(&format!(
        "✅ 导入完成: {} 个 session，{} 条消息",
        outcome.sessions_written, outcome.messages_written
    ));

    if outcome.messages_dropped > 0 {
        crate::ui::warn(&format!(
            "⚠️  {} 条消息因画像缺失被丢弃",
            outcome.messages_dropped
        ));
    }

    if report.total_skipped > 0 {
        crate::ui::warn(&format!(
            "⚠️  跳过的消息: {} 条（撤回 {}，空内容 {}，未知类型 {}）",
            report.total_skipped,
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
            "sessions_written": outcome.sessions_written,
            "messages_written": outcome.messages_written,
            "messages_dropped": outcome.messages_dropped,
            "session_ids": outcome.session_ids.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "l1": {"ok": l1_ok, "skip": l1_skip, "err": l1_err},
            "skipped": report.total_skipped,
            "chat_type": outcome.chat_type,
            "members": serde_json::to_value(&outcome.members)?,
        });
        return crate::json::emit_ok(&data);
    }

    Ok(())
}

// =========================================================
// 继续处理参数
// =========================================================

/// CLI "继续处理"命令的参数。
pub struct ContinueArgs {
    /// 导出根目录（可多次指定；空 = 跳过附件回补）
    pub export_roots: Vec<PathBuf>,
    /// 重置"有文件可定位"的存量跳过附件
    pub reset_skipped: bool,
    /// L1 补跑会话数上限（0 = 不限）
    pub limit: usize,
    /// 补跑成功后触发 L2 → L3 级联
    pub cascade_deep: bool,
    /// 跳过确认提示
    pub yes: bool,
    /// JSON 信封输出
    pub json: bool,
}

// =========================================================
// 继续处理进度回调
// =========================================================

/// CLI 进度回调：阶段进度按节流打印到 stderr（stdout 保持数据纯净）。
///
/// 说明:
/// - 图片理解的逐行进度按 10 行或末行节流；L1 阶段逐会话推送；
/// - `on_done` 为空实现（统计由命令主体汇总输出）。
struct CliProgressSink;

impl ImportProgressSink for CliProgressSink {
    fn on_l1_progress(&self, p: &ImportL1Progress) {
        if p.current.is_multiple_of(10) || p.current == p.total {
            crate::ui::info(&p.message);
        }
    }

    fn on_done(&self, _summary: &ImportDoneSummary) {}
}

// =========================================================
// run_continue — 继续处理命令入口
// =========================================================

/// 执行导入"继续处理"（附件回补 → L1 幂等补跑 → 可选深度级联）。
///
/// 参数:
/// - `engine`: 服务层引擎（三阶段编排经服务层用例）。
/// - `args`: 继续处理参数（导出根 / 重置开关 / 上限 / 级联开关）。
///
/// 流程:
/// 1. 导出根校验（业务校验失败，exit code 4）；
/// 2. 用户确认（非 --yes 模式；非 TTY 无 --yes 直接失败不挂起）；
/// 3. 服务层编排（附件回补 → L1 补跑 → 可选级联），进度节流打印到 stderr；
/// 4. 结果输出（统计与级联标志）。
pub async fn run_continue(engine: &Arc<Engine>, args: ContinueArgs) -> anyhow::Result<()> {
    // Step 1: 导出根校验（业务校验失败，exit code 4）
    for root in &args.export_roots {
        if !root.exists() {
            return Err(anyhow::anyhow!(
                ramaria_core::error::RamariaError::validation(format!(
                    "导出目录不存在: {}",
                    root.display()
                ))
            ));
        }
        if !root.is_dir() {
            return Err(anyhow::anyhow!(
                ramaria_core::error::RamariaError::validation(format!(
                    "路径不是目录: {}",
                    root.display()
                ))
            ));
        }
    }

    // Step 2: 确认（非 --yes 模式；非 TTY 无 --yes 直接失败不挂起）
    if !args.yes {
        let proceed =
            crate::ui::confirm("确认继续处理（附件回补 + L1 补跑 + 深度级联）?", args.yes)
                .map_err(|e| ramaria_core::error::RamariaError::validation(e.to_string()))?;
        if !proceed {
            crate::ui::info("继续处理已取消");
            return Ok(());
        }
    }

    // Step 3: 服务层编排（进度经 sink 节流打印到 stderr）
    crate::ui::info("🔄 执行导入继续处理（附件回补 → L1 补跑 → 深度级联）...");
    let outcome = engine
        .continue_import(
            ImportContinueRequest {
                export_roots: args.export_roots.clone(),
                reset_skipped: args.reset_skipped,
                throttle_ms: 0,
                l1_limit: args.limit,
                cascade_deep: args.cascade_deep,
                // 图片缓存目录（数据库同级 `image_cache`；父目录缺失时降级不建立缓存）
                cache_dir: engine
                    .db_path()
                    .parent()
                    .map(|parent| parent.join("image_cache")),
            },
            Some(&CliProgressSink),
        )
        .await
        .context("继续处理失败")?;

    // Step 4: 统计输出
    match &outcome.vision {
        Some(stat) => crate::ui::info(&format!(
            "🖼️  附件回补: 重置 {}（md5 反查 {}），扫描 {}，完成 {}，复用 {}，跳过 {}，失败 {}",
            outcome.reset_count,
            outcome.reverse_lookup_hits,
            stat.scanned,
            stat.done,
            stat.reused,
            stat.skipped,
            stat.failed
        )),
        None => crate::ui::info("🖼️  附件回补: 未执行"),
    }
    crate::ui::info(&format!(
        "📝 L1 补跑: {} 成功, {} 跳过, {} 失败（候选 {}）",
        outcome.l1.l1_success, outcome.l1.l1_skipped, outcome.l1.l1_failed, outcome.l1.l1_total
    ));
    if outcome.l2_triggered {
        crate::ui::info("🔍 已触发 L2 事件提取 → L3 人格画像");
    }
    crate::ui::success("✅ 继续处理完成");

    if args.json {
        let data = serde_json::json!({
            "reset_count": outcome.reset_count,
            "reverse_lookup_hits": outcome.reverse_lookup_hits,
            "vision": outcome.vision.map(|stat| serde_json::json!({
                "scanned": stat.scanned,
                "done": stat.done,
                "reused": stat.reused,
                "skipped": stat.skipped,
                "failed": stat.failed,
            })),
            "l1": {
                "success": outcome.l1.l1_success,
                "skipped": outcome.l1.l1_skipped,
                "failed": outcome.l1.l1_failed,
                "total": outcome.l1.l1_total,
            },
            "l2_triggered": outcome.l2_triggered,
            "l3_triggered": outcome.l3_triggered,
        });
        return crate::json::emit_ok(&data);
    }

    Ok(())
}
