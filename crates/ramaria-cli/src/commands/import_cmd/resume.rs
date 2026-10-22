//! crates/ramaria-cli/src/commands/import_cmd/resume.rs - 数据导入继续处理命令
//!
//! 设计特点:
//! - 导出根校验 / 确认 / 服务层编排 / 统计输出。
//! - 进度经 sink 节流打印到 stderr，stdout 保持数据纯净。

use anyhow::Context;
use ramaria_service::{
    Engine, ImportContinueRequest, ImportDoneSummary, ImportL1Progress, ImportProgressSink,
};
use std::path::PathBuf;
use std::sync::Arc;

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
