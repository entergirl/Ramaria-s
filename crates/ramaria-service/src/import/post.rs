//! crates/ramaria-service/src/import/post.rs - Ramaria 导入后处理编排模块
//!
//! 设计特点:
//! - 单一入口：图片理解 → L1 批量生成（含风格统计触发）→ 深度触发按固定顺序一次编排，
//!   宿主差异由计划参数表达
//! - 图片理解降级：执行失败只记 warn 且统计置 None，不阻塞 L1 与深度触发；导出根缺失时零调用
//! - 图片理解进度：逐行进度按"每 5 行或百分比变化"节流经阶段回调透传，
//!   完成 / 中止时补一条携带 done / failed / skipped 计数的收尾事件
//! - 风格统计触发：L1 完成后逐 persona 增量统计（门控 `[style].enabled`，失败不阻塞）
//! - 深度触发口径：仅当 L1 全部完成后至少一条生成成功时统一触发 L2 → L3
//! - 导入后处理互斥：进程内标志（RAII guard 保证复位）供宿主拒绝并发"继续处理"
//! - 日志：步骤级 info 只记会话数与阶段计数，不落消息正文 / 昵称 / 路径原文

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ramaria_core::error::RamariaResult;
use ramaria_core::privacy::mask_id;
use uuid::Uuid;

use crate::engine::Engine;
use crate::vision::{
    AttachmentLocator, VisionRunStat, scan_pending_attachments, understand_attachment_rows,
};

use super::deep::trigger_deep;
use super::l1::{ImportL1Outcome, ImportL1Plan, ImportL1Progress, ImportProgressSink, generate_l1};

// =========================================================
// 导入后处理互斥（进程内单实例）
// =========================================================

/// 导入后处理是否活跃（图片理解 / L1 / 深度触发在途）。
static POST_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 导入后处理活跃标记的 RAII 守卫：构造置位，Drop 复位（异常 / 提前返回路径不残留）。
pub(crate) struct PostActiveGuard(());

impl PostActiveGuard {
    /// 置位活跃标记并返回守卫（生命周期与 `run_post` 调用一致）。
    pub(crate) fn acquire() -> Self {
        POST_ACTIVE.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for PostActiveGuard {
    fn drop(&mut self) {
        POST_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// 查询导入后处理是否正在进行（宿主在其他入口据此拒绝并发操作）。
pub fn is_post_active() -> bool {
    POST_ACTIVE.load(Ordering::SeqCst)
}

// =========================================================
// 请求与结果类型
// =========================================================

/// 导入后处理计划。
///
/// 字段约定:
/// - `l1_targets`: 每个目标一次 L1 生成（`group_fanout` 为 true 时忽略）；
/// - `l1_prefix`: L1 角色前缀覆盖（`None` = 摘要器默认前缀；`group_fanout` 为 true 时忽略）；
/// - `group_fanout`: 参与者分发（为 true 时忽略 `l1_targets` 与 `l1_prefix`）；
/// - `fanout_include_self`: 分发参与者含"我"（User 角色发言者）；
///   仅 `group_fanout` 为 true 时生效（导入分发恒开启）；
/// - `l1_cascade`: 每次 L1 生成末尾触发 L2 检查（逐次级联）；
/// - `cascade_deep`: L1 全部完成后统一触发 L2 → L3 级联（深度模式）；
/// - `throttle_ms`: 连续 LLM 调用之间的最小间隔（毫秒，0 = 不等待）。
#[derive(Debug, Clone)]
pub struct ImportPostPlan {
    /// L1 生成目标列表（每个目标一次 L1 生成；`group_fanout` 时忽略）
    pub l1_targets: Vec<Option<String>>,
    /// L1 角色前缀覆盖（`None` = 摘要器默认；`group_fanout` 时忽略）
    pub l1_prefix: Option<(String, String)>,
    /// 参与者分发（为 true 时忽略 `l1_targets` 与 `l1_prefix`）
    pub group_fanout: bool,
    /// 分发参与者含"我"（仅 `group_fanout` 为 true 时生效）
    pub fanout_include_self: bool,
    /// 逐次级联：每次 L1 生成末尾触发 L2 检查
    pub l1_cascade: bool,
    /// L1 全部完成后统一触发 L2 → L3 级联
    pub cascade_deep: bool,
    /// 连续 LLM 调用间最小间隔（毫秒）
    pub throttle_ms: u64,
}

/// 导入后处理请求。
#[derive(Debug, Clone)]
pub struct ImportPostRequest {
    /// 本次导入产出的会话 ID 列表
    pub session_ids: Vec<Uuid>,
    /// 导出目录（附件定位根）；`None` = 跳过图片理解
    pub export_root: Option<PathBuf>,
    /// 后处理计划
    pub plan: ImportPostPlan,
}

/// 导入后处理结果。
#[derive(Debug, Clone)]
pub struct ImportPostOutcome {
    /// 图片理解统计（`None` = 未执行或失败降级）
    pub vision: Option<VisionRunStat>,
    /// L1 批量生成结果
    pub l1: ImportL1Outcome,
    /// 深度处理：L2 是否已触发
    pub l2_triggered: bool,
    /// 深度处理：L3 是否已触发
    pub l3_triggered: bool,
}

// =========================================================
// 用例入口
// =========================================================

/// 执行导入后处理编排：图片理解 → L1 批量生成（含风格统计触发）→ 可选深度级联。
///
/// 流程:
/// 1. `export_root` 为 Some 时先执行图片理解（失败只记日志并降级为 `None`，不阻塞后续）；
/// 2. L1 批量生成（计划与进度回调透传，口径见 [`ImportL1Plan`]）；
/// 3. L1 完成后按结果人格触发风格统计增量更新（失败不阻塞）；
/// 4. `cascade_deep` 且至少一条 L1 生成成功时统一触发 L2 → L3 级联。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `req`: 会话列表、导出根与后处理计划；
/// - `progress`: 可选进度回调（vision / L1 / L2 / L3 阶段进度透传）。
///
/// 返回:
/// - `ImportPostOutcome`：图片理解统计、L1 计数与深度触发标志。
pub(crate) async fn run_post(
    engine: &Engine,
    req: ImportPostRequest,
    progress: Option<&dyn ImportProgressSink>,
) -> RamariaResult<ImportPostOutcome> {
    // 活跃标记随本次编排置位 / 复位：宿主在其他入口（如"继续处理"）据此拒绝并发
    let _active = PostActiveGuard::acquire();

    let session_count = req.session_ids.len();
    tracing::info!(session_count, "导入后处理开始");

    // ---- 图片理解（先理解后 L1；失败降级不阻塞） ----
    let vision = match &req.export_root {
        Some(root) => run_vision_stage(engine, &req.session_ids, root, progress).await,
        None => None,
    };

    // ---- L1 批量生成 ----
    let l1 = generate_l1(
        engine,
        &req.session_ids,
        ImportL1Plan {
            targets: req.plan.l1_targets,
            l1_prefix: req.plan.l1_prefix,
            group_fanout: req.plan.group_fanout,
            fanout_include_self: req.plan.fanout_include_self,
            cascade: req.plan.l1_cascade,
            throttle_ms: req.plan.throttle_ms,
        },
        progress,
    )
    .await?;

    // ---- 风格统计触发（L1 完成后逐 persona 增量统计；失败不阻塞） ----
    update_style_stats(engine, &l1.personas).await;

    // ---- 深度触发（L1 全部完成后统一触发 L2 → L3） ----
    let mut l2_triggered = false;
    let mut l3_triggered = false;
    if req.plan.cascade_deep && l1.l1_success > 0 {
        if let Err(e) = trigger_deep(engine, Some(l1.l1_total), progress).await {
            tracing::warn!(error = %e, "深度处理触发失败（不阻塞导入结果）");
        }
        l2_triggered = true;
        l3_triggered = true;
    }

    tracing::info!(
        session_count,
        l1_success = l1.l1_success,
        l1_failed = l1.l1_failed,
        l1_skipped = l1.l1_skipped,
        l2_triggered,
        l3_triggered,
        "导入后处理完成"
    );

    Ok(ImportPostOutcome {
        vision,
        l1,
        l2_triggered,
        l3_triggered,
    })
}

// =========================================================
// 图片理解阶段（扫描 → 节流进度 → 收尾事件）
// =========================================================

/// 执行图片理解阶段：扫描待处理附件 → 起始与节流进度 → 完成 / 中止收尾事件。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `session_ids`: 本次导入产出的会话列表；
/// - `export_root`: 导出目录（附件相对引用的定位根）；
/// - `progress`: 可选进度回调（phase = "vision"）。
///
/// 返回:
/// - `Some(stat)`: 阶段执行完成（含零待处理与门禁未过口径）；
/// - `None`: 扫描或逐行理解失败（降级，不阻塞后续阶段）。
async fn run_vision_stage(
    engine: &Engine,
    session_ids: &[Uuid],
    export_root: &Path,
    progress: Option<&dyn ImportProgressSink>,
) -> Option<VisionRunStat> {
    let rows = match scan_pending_attachments(engine, session_ids).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "图片理解扫描失败（不阻塞导入）");
            return None;
        }
    };
    let total = rows.len();
    if total == 0 {
        tracing::info!("导入图片理解：无待处理附件");
        return Some(VisionRunStat::default());
    }

    // 起始进度：总量已知，先给 0/N 起点（宿主只做转发）
    emit_vision_progress(progress, 0, total, format!("正在理解图片 0/{total}"), None);

    let mut locator = AttachmentLocator::new(vec![export_root.to_path_buf()]);
    // 节流状态：上次推送时的百分比（usize::MAX = 尚未推送）
    let last_pct = AtomicUsize::new(usize::MAX);
    let on_row = |done: usize| {
        let pct = done.saturating_mul(100) / total;
        if should_push_vision_progress(done, total, last_pct.load(Ordering::Relaxed)) {
            last_pct.store(pct, Ordering::Relaxed);
            emit_vision_progress(
                progress,
                done,
                total,
                format!("正在理解图片 {done}/{total}"),
                None,
            );
        }
    };

    match understand_attachment_rows(engine, &rows, &mut locator, Some(&on_row)).await {
        Ok(stat) => {
            tracing::info!(
                scanned = stat.scanned,
                done = stat.done,
                reused = stat.reused,
                skipped = stat.skipped,
                failed = stat.failed,
                "导入图片理解完成"
            );
            // 完成收尾事件：done / failed / skipped 计数随可选字段透出
            emit_vision_progress(
                progress,
                stat.scanned,
                stat.scanned,
                format!(
                    "图片理解完成：成功 {}，失败 {}，跳过 {}",
                    stat.done, stat.failed, stat.skipped
                ),
                Some(&stat),
            );
            Some(stat)
        }
        Err(e) => {
            tracing::warn!(error = %e, "图片理解执行失败（不阻塞导入）");
            emit_vision_progress(
                progress,
                0,
                0,
                "图片理解中止（不阻塞后续处理）".to_string(),
                None,
            );
            None
        }
    }
}

/// 推送一条图片理解阶段进度（`stat` 非空时携带完成统计字段）。
///
/// 参数:
/// - `progress`: 进度回调（None = 不推送）；
/// - `current` / `total`: 当前进度与总量；
/// - `message`: 人类可读描述；
/// - `stat`: 完成统计（仅完成收尾事件传入）。
fn emit_vision_progress(
    progress: Option<&dyn ImportProgressSink>,
    current: usize,
    total: usize,
    message: String,
    stat: Option<&VisionRunStat>,
) {
    let Some(sink) = progress else {
        return;
    };
    sink.on_l1_progress(&ImportL1Progress {
        phase: "vision",
        current,
        total,
        message,
        eta_seconds: None,
        l1_total: None,
        l2_total: None,
        l3_total: None,
        vision_done: stat.map(|s| s.done),
        vision_failed: stat.map(|s| s.failed),
        vision_skipped: stat.map(|s| s.skipped),
    });
}

/// 图片理解进度节流：每 5 行或百分比变化时推送（末行必推）。
///
/// 参数:
/// - `done`: 已完成行数（从 1 起）；
/// - `total`: 总行数；
/// - `prev_pct`: 上次推送时的百分比（`usize::MAX` = 尚未推送）。
pub(super) fn should_push_vision_progress(done: usize, total: usize, prev_pct: usize) -> bool {
    if done == total || done.is_multiple_of(5) {
        return true;
    }
    total > 0 && done.saturating_mul(100) / total != prev_pct
}

// =========================================================
// 风格统计触发
// =========================================================

/// 对本次参与 L1 生成的人格逐个触发风格统计增量更新。
///
/// 语义:
/// - `[style].enabled=false` 时整体跳过（debug 日志）；
/// - 空列表直接返回；
/// - 单个 persona 失败只记 warn，不阻塞编排后续阶段。
///
/// 安全约束:
/// - 日志只记 persona（经 `mask_id` 脱敏）与计数，不记统计内容。
pub(super) async fn update_style_stats(engine: &Engine, personas: &[String]) {
    if personas.is_empty() {
        return;
    }
    if !engine.config().style.enabled {
        tracing::debug!(
            personas = personas.len(),
            "风格统计关闭，跳过导入后风格更新"
        );
        return;
    }
    let mut updated = 0usize;
    for uid in personas {
        match crate::style::incremental_update(engine, uid).await {
            Ok(()) => updated += 1,
            Err(e) => {
                tracing::warn!(
                    persona_uid = %mask_id(uid),
                    error = %e,
                    "导入后风格统计更新失败（非致命）"
                );
            }
        }
    }
    tracing::info!(personas = personas.len(), updated, "导入后风格统计更新完成");
}
