//! crates/ramaria-service/src/import/resume.rs - Ramaria 导入"继续处理"编排模块
//!
//! 设计特点:
//! - 三阶段单一编排：附件回补（重置可定位存量跳过 + 命中文件缓存回填 + 重扫 pending + md5 反查）
//!   → L1 幂等补跑（含风格统计触发）→ 深度级联补触发
//! - 幂等可重跑：理解按 md5 去重、L1 按"已有归属行"跳过；重复执行零副作用
//! - 能力门禁不可达保持 pending；附件阶段失败不阻塞 L1 补跑
//! - 进度经 ImportProgressSink 透传（phase: vision / l1 / l2 / l3），宿主只做转发
//! - 日志不落路径原文与消息正文

use std::collections::BTreeSet;
use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::engine::Engine;
use crate::vision::{AttachmentLocator, VisionRunStat, understand_attachment_rows};

use super::deep::trigger_deep;
use super::l1::{ImportL1Outcome, ImportL1Progress, ImportProgressSink, regenerate_l1_fanout};
use super::post::update_style_stats;

// =========================================================
// 请求与结果类型
// =========================================================

/// "继续处理"请求（回补中断导入的三阶段选项）。
///
/// 字段约定:
/// - `export_roots`: 附件定位与反查的搜索根；空 = 跳过附件阶段；
/// - `reset_skipped`: 是否重置"有文件可定位"的存量 skipped 行；
/// - `throttle_ms`: 连续 LLM 调用间最小间隔（毫秒，0 = 不等待）；
/// - `l1_limit`: L1 补跑会话上限（0 = 不限）；
/// - `cascade_deep`: 补跑成功后是否触发 L2 → L3 级联；
/// - `cache_dir`: 图片缓存目录（None = 不建立缓存）。
#[derive(Debug, Clone)]
pub struct ImportContinueRequest {
    /// 附件定位与反查的搜索根（空 = 跳过附件阶段）
    pub export_roots: Vec<PathBuf>,
    /// 重置"有文件可定位"的存量跳过行
    pub reset_skipped: bool,
    /// 连续 LLM 调用间最小间隔（毫秒）
    pub throttle_ms: u64,
    /// L1 补跑会话上限（0 = 不限）
    pub l1_limit: usize,
    /// 补跑成功后触发 L2 → L3 级联
    pub cascade_deep: bool,
    /// 图片缓存目录（None = 不建立缓存）
    pub cache_dir: Option<PathBuf>,
}

/// "继续处理"结果。
#[derive(Debug, Clone)]
pub struct ImportContinueOutcome {
    /// 附件回补统计（`None` = 附件阶段未执行或失败降级）
    pub vision: Option<VisionRunStat>,
    /// 重置为待处理的存量附件数
    pub reset_count: usize,
    /// 上述重置中经 md5 反查命中的数量
    pub reverse_lookup_hits: usize,
    /// L1 补跑结果
    pub l1: ImportL1Outcome,
    /// 深度处理：L2 是否已触发
    pub l2_triggered: bool,
    /// 深度处理：L3 是否已触发
    pub l3_triggered: bool,
}

// =========================================================
// 用例入口
// =========================================================

/// 执行导入"继续处理"：附件回补 → L1 幂等补跑（含风格统计触发）→ 可选深度级联。
///
/// 流程:
/// 1. 导出根校验（任一根不存在或非目录即报错，面向用户的校验错误）；
/// 2. `export_roots` 非空时执行附件回补：
///    重置可定位的存量跳过行 → 全库重扫 pending（逐行理解，逐行进度）→ 统计；
/// 3. L1 补跑：扫描"已关闭且缺摘要"的会话，逐会话按分发口径（恒含"我"与块内他人）生成；
/// 4. 补跑完成后按结果人格触发风格统计增量更新（失败不阻塞）；
/// 5. `cascade_deep` 且至少一条 L1 生成成功时统一触发 L2 → L3 级联。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `req`: 三阶段选项（见 [`ImportContinueRequest`]）；
/// - `progress`: 可选进度回调（vision / l1 / l2 / l3 阶段进度透传）。
///
/// 返回:
/// - [`ImportContinueOutcome`]：附件回补统计、重置计数与 L1 / 级联结果。
pub(crate) async fn run_continue(
    engine: &Engine,
    req: ImportContinueRequest,
    progress: Option<&dyn ImportProgressSink>,
) -> RamariaResult<ImportContinueOutcome> {
    tracing::info!(
        roots = req.export_roots.len(),
        reset_skipped = req.reset_skipped,
        l1_limit = req.l1_limit,
        cascade_deep = req.cascade_deep,
        "导入继续处理开始"
    );

    // ---- 1. 导出根校验（错误文案面向用户可含路径；日志侧不落路径原文） ----
    for root in &req.export_roots {
        if !root.is_dir() {
            return Err(RamariaError::validation(format!(
                "导出目录不存在或不是目录: {}",
                root.display()
            )));
        }
    }

    let storage = engine.storage_ref().clone();
    let mut vision_stat: Option<VisionRunStat> = None;
    let mut reset_count = 0usize;
    let mut reverse_lookup_hits = 0usize;

    // ---- 2. 附件回补（export_roots 非空时执行；失败不阻塞 L1 补跑） ----
    if !req.export_roots.is_empty() {
        let mut locator = AttachmentLocator::new(req.export_roots.clone());

        // 2a. 存量跳过行重置：仅重置"有文件可定位"的行（单行失败只记 warn 跳过）
        if req.reset_skipped {
            let skipped = storage.list_all_skipped_attachments(0).await?;
            for row in &skipped {
                let Some(located) = locator.locate(&row.source_ref, row.md5.as_deref()) else {
                    continue;
                };
                // 命中文件 best-effort 建立客户端缓存（失败不影响重置逻辑）
                if let Some(cache_dir) = req.cache_dir.as_deref() {
                    match std::fs::read(&located.path) {
                        Ok(bytes) => {
                            crate::attachment_cache::store(
                                cache_dir,
                                row.id,
                                row.md5.as_deref(),
                                &bytes,
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                attachment_id = row.id,
                                error = %error,
                                "缓存回填读取来源文件失败（跳过该行缓存）"
                            );
                        }
                    }
                }
                match storage
                    .reset_attachment_for_rescan(row.id, Some(&located.normalized_ref))
                    .await
                {
                    Ok(()) => {
                        reset_count += 1;
                        if located.via_reverse_lookup {
                            reverse_lookup_hits += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            attachment_id = row.id,
                            error = %e,
                            "存量跳过附件重置失败（跳过该行）"
                        );
                    }
                }
            }
            tracing::info!(reset_count, reverse_lookup_hits, "存量跳过附件重置完成");
        }

        // 2b. 全库重扫 pending（逐行理解；逐行进度按 10 行或末行节流推送）
        let rows = storage.list_all_pending_attachments(0).await?;
        let total = rows.len();
        vision_stat = if total == 0 {
            tracing::info!("附件回补：无待处理附件");
            Some(VisionRunStat::default())
        } else {
            if let Some(sink) = progress {
                sink.on_l1_progress(&ImportL1Progress {
                    phase: "vision",
                    current: 0,
                    total,
                    message: format!("正在回补图片理解 0/{total}"),
                    eta_seconds: None,
                    l1_total: None,
                    l2_total: None,
                    l3_total: None,
                });
            }
            let on_row = |done: usize| {
                if done.is_multiple_of(10) || done == total {
                    if let Some(sink) = progress {
                        sink.on_l1_progress(&ImportL1Progress {
                            phase: "vision",
                            current: done,
                            total,
                            message: format!("正在回补图片理解 {done}/{total}"),
                            eta_seconds: None,
                            l1_total: None,
                            l2_total: None,
                            l3_total: None,
                        });
                    }
                }
            };
            match understand_attachment_rows(engine, &rows, &mut locator, Some(&on_row)).await {
                Ok(stat) => Some(stat),
                Err(e) => {
                    // 附件理解失败降级为 None，不阻塞 L1 补跑
                    tracing::warn!(error = %e, "附件回补执行失败（不阻塞 L1 补跑）");
                    None
                }
            }
        };
    }

    // ---- 3. L1 补跑（幂等：仅补"已关闭且缺摘要"的会话） ----
    let limit = u32::try_from(req.l1_limit).unwrap_or(u32::MAX);
    let candidates = storage.list_closed_sessions_missing_l1(limit).await?;
    let total = candidates.len();
    let mut l1_success = 0usize;
    let mut l1_failed = 0usize;
    let mut l1_skipped = 0usize;
    let mut l1_processed = 0usize;
    // 本次补跑参与生成的人格归属（去重升序；供风格统计触发）
    let mut personas: BTreeSet<String> = BTreeSet::new();

    if total > 0 {
        if let Some(sink) = progress {
            sink.on_l1_progress(&ImportL1Progress {
                phase: "l1",
                current: 0,
                total,
                message: "正在补跑缺失的 L1 会话摘要...".to_string(),
                eta_seconds: None,
                l1_total: Some(total),
                l2_total: None,
                l3_total: None,
            });
        }
    }

    for session_id in &candidates {
        // 分发口径：恒含"我"（导出者）与块内他人——各 persona 一行，absorbed 独立
        match regenerate_l1_fanout(engine, *session_id, true).await {
            Ok(rows) if !rows.is_empty() => {
                l1_success += 1;
                for row in &rows {
                    if let Some(uid) = row.persona_uid.as_deref().filter(|s| !s.is_empty()) {
                        personas.insert(uid.to_string());
                    }
                }
            }
            Ok(_) => {
                l1_skipped += 1;
                tracing::debug!(session_id = %session_id, "L1 补跑无内容可生成，跳过");
            }
            Err(e) => {
                l1_failed += 1;
                tracing::warn!(session_id = %session_id, error = %e, "L1 补跑失败（非致命）");
            }
        }
        l1_processed += 1;

        if let Some(sink) = progress {
            sink.on_l1_progress(&ImportL1Progress {
                phase: "l1",
                current: l1_processed,
                total,
                message: format!("L1 补跑 {l1_processed}/{total}"),
                eta_seconds: None,
                l1_total: Some(total),
                l2_total: None,
                l3_total: None,
            });
        }
        // 请求间节流：连续 LLM 调用间保持最小间隔，避免触发远端速率限制
        ramaria_memory::llm_gate::inter_llm_delay(req.throttle_ms, "L1 补跑").await;
    }

    let l1 = ImportL1Outcome {
        l1_success,
        l1_failed,
        l1_skipped,
        l1_processed,
        l1_total: total,
        session_ids: candidates,
        personas: personas.into_iter().collect(),
    };

    // ---- 3b. 风格统计触发（L1 补跑完成后逐 persona 增量统计；失败不阻塞） ----
    update_style_stats(engine, &l1.personas).await;

    // ---- 4. 深度级联补触发（至少一条 L1 生成成功时统一触发 L2 → L3） ----
    let mut l2_triggered = false;
    let mut l3_triggered = false;
    if req.cascade_deep && l1.l1_success > 0 {
        if let Err(e) = trigger_deep(engine, Some(l1.l1_total), progress).await {
            tracing::warn!(error = %e, "深度处理触发失败（不阻塞继续处理结果）");
        }
        l2_triggered = true;
        l3_triggered = true;
    }

    tracing::info!(
        reset_count,
        reverse_lookup_hits,
        vision_scanned = vision_stat.map(|stat| stat.scanned).unwrap_or(0),
        l1_success = l1.l1_success,
        l1_failed = l1.l1_failed,
        l1_skipped = l1.l1_skipped,
        l2_triggered,
        l3_triggered,
        "导入继续处理完成"
    );

    Ok(ImportContinueOutcome {
        vision: vision_stat,
        reset_count,
        reverse_lookup_hits,
        l1,
        l2_triggered,
        l3_triggered,
    })
}
