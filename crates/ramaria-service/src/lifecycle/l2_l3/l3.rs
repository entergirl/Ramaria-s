//! crates/ramaria-service/src/lifecycle/l2_l3/l3.rs - Ramaria L3 性格推断触发与执行
//!
//! 设计特点:
//! - `check_l3_trigger`：计数线（未吸收事件数 ≥ 阈值）或时间线（最早事件超龄）任一满足 → 推断
//! - 全流程：Phase A 统计 + 分层收缩 → Phase B LLM 推断 → Phase C 置信度更新 + 漂移检测
//! - 快照语义：Phase C 后、事件吸收前写入本轮分布，作为下一轮漂移检测的旧分布基准
//! - 首轮判定：以"本轮是否产出活跃 trait"为准（Keep-only 稳定轮必须继续漂移检测）
//! - 降级纪律：Phase B / C 失败不阻塞事件吸收标记，仅记日志后继续；日志只记 persona 与计数

use std::sync::atomic::AtomicBool;

use ramaria_core::types::now_ms;
use ramaria_memory::job::{JobManager, JobType};
use tracing::{debug, error, info, warn};

use crate::engine::Engine;

// =========================================================
// L3 性格推断触发检查
// =========================================================

/// 检查 L3 性格推断触发条件。
///
/// 触发条件（任一满足）:
/// - 计数线：未吸收事件数 ≥ `[thresholds].l3_trigger_count`（默认 10 条；`0` = 有事件即触发）；
/// - 时间线：`[thresholds].l3_trigger_days` > 0 且最早事件超过该天数
///   （默认 30 天；`0` = 不按时间触发，与定时路径同约定）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `shutdown`: 宿主停止位；本函数不做停止位中断，参数保留以对齐调度链调用口径。
/// - `persona_uid`: 目标人格。
///
/// 返回:
/// - `true`: 本次启动了性格推断；
/// - `false`: 未触发（无未吸收事件或条件不满足）。
pub(crate) async fn check_l3_trigger(
    engine: &Engine,
    shutdown: Option<&AtomicBool>,
    persona_uid: &str,
) -> bool {
    // 停止位中断由调用方轮次循环承担；本函数不额外检查。
    let _ = shutdown;

    let storage = engine.storage_ref().as_ref();
    let events = match storage.list_unabsorbed_events(persona_uid).await {
        Ok(e) => e,
        Err(e) => {
            warn!(persona_uid, %e, "L3 触发检查：查询未吸收事件失败");
            return false;
        }
    };

    if events.is_empty() {
        return false;
    }

    let now = now_ms();
    let oldest_event_age_days = events
        .iter()
        .map(|e| e.start)
        .min()
        .map(|min_time| (now - min_time) as f64 / (1000.0 * 86400.0))
        .unwrap_or(0.0);

    // L3 触发条件来自配置：计数线 `0` = 有未吸收事件即触发；
    // 时间线 `0` = 不按时间触发（与定时路径同一约定）
    let trigger_count = engine.config().thresholds.l3_trigger_count as usize;
    let trigger_days = engine.config().thresholds.l3_trigger_days as f64;

    let count_fired = events.len() >= trigger_count;
    let time_fired = trigger_days > 0.0 && oldest_event_age_days >= trigger_days;

    if count_fired || time_fired {
        info!(
            persona_uid,
            event_count = events.len(),
            oldest_days = %format!("{:.1}", oldest_event_age_days),
            "L3 触发条件满足，启动性格推断"
        );
        run_l3_inference(engine, persona_uid).await;
        true
    } else {
        info!(
            persona_uid,
            event_count = events.len(),
            trigger_count,
            oldest_days = %format!("{:.1}", oldest_event_age_days),
            trigger_days,
            "L3 触发条件未满足（需要 {} 条未吸收事件或最早事件 > {} 天，当前 {} 条 {:.1} 天）",
            trigger_count, trigger_days, events.len(), oldest_event_age_days
        );
        false
    }
}

/// 执行 L3 性格推断（Phase A 统计+分层收缩 → Phase B LLM 推断 → Phase C 置信度更新）。
///
/// 可观测性:
/// - 通过 JobManager 创建 `PersonalityInference` 任务记录，
///   记录开始/完成/failed 时间，便于运维排查"何时对谁做了推断"。
///
/// 流程:
/// - Phase A: 校准权重链 + 三轨准入 + 分层收缩（真实执行）+ 动机统计。
///   收缩发生在统计后、写快照与 Phase B 前，收缩后分布成为当轮快照与
///   Phase B prompt 的输入（跨用户冷启动先验由
///   `cold_start_cross_user_prior` 开关控制，见函数内接线）。
/// - Phase B: LLM 三步结构化推断（注入因果链特征 + 动机维度）
/// - Phase C: 校准化置信度更新 + 四维度漂移检测
/// - Phase C 后、事件吸收前：把本轮分布写入 cluster_snapshots（存档上一期），
///   作为下一轮漂移检测对比的旧分布基准（避免同轮自比）。
pub(super) async fn run_l3_inference(engine: &Engine, persona_uid: &str) {
    let storage = engine.storage_ref().as_ref();
    let persona_owned = persona_uid.to_string();

    // 取未吸收事件列表
    let events = match storage.list_unabsorbed_events(&persona_owned).await {
        Ok(e) => e,
        Err(e) => {
            error!(persona_uid = %persona_owned, %e, "L3 推断：查询事件失败");
            return;
        }
    };

    if events.is_empty() {
        debug!(persona_uid = %persona_owned, "L3 推断：无未吸收事件，跳过");
        return;
    }

    // ---- 创建 JobManager 任务记录（可观测性） ----
    let job_manager = JobManager::with_defaults(storage);
    let payload = serde_json::json!({
        "persona_uid": &persona_owned,
        "event_count": events.len(),
        "phase": "A"
    })
    .to_string();

    let job_id = match job_manager
        .create(JobType::PersonalityInference, Some(&payload))
        .await
    {
        Ok(id) => id,
        Err(e) => {
            error!(persona_uid = %persona_owned, %e, "创建 L3 推断任务记录失败，继续执行");
            0 // 哨兵值：表示无有效 job_id
        }
    };

    if job_id > 0
        && let Err(e) = job_manager.mark_running(job_id).await
    {
        // 状态标记失败只影响可观测性，推断流程继续（不返回、不覆盖业务结果）
        warn!(
            job_id,
            error = %e,
            "标记 L3 推断任务 running 失败（继续执行，仅状态可观测性受影响）"
        );
    }

    // ---- 统计特征提取（纯数值，不调 LLM） ----
    use ramaria_memory::inference::{
        ShrinkConfig, StatsConfig, apply_layered_shrinkage, run_phase_a_stats,
    };

    let stats_config = StatsConfig::default();
    let mut stats_summary = run_phase_a_stats(&events, &stats_config);

    info!(
        persona_uid = %persona_owned,
        event_count = events.len(),
        category_count = stats_summary.categories.len(),
        job_id,
        "L3 Phase A 统计完成"
    );

    // ---- Phase A 内分层先验收缩（含跨用户冷启动先验） ----
    // 语义：收缩修正后的分类分布是当轮画像推断的输入——后续写入的
    // persona_cluster_snapshots 快照与 Phase B prompt 均使用收缩后数值
    //（顺序：统计 → 分层收缩 → 写快照 / Phase B）。
    // γ 三元参数此处使用 ShrinkConfig::default()。
    let cross_user_prior_enabled = engine
        .config()
        .inference
        .upgrade
        .cold_start_cross_user_prior;
    let shrink_gamma = apply_layered_shrinkage(
        storage,
        &mut stats_summary,
        &persona_owned,
        &ShrinkConfig::default(),
        cross_user_prior_enabled,
    )
    .await;

    info!(
        persona_uid = %persona_owned,
        gamma = shrink_gamma,
        cross_user_prior_enabled,
        job_id,
        "L3 Phase A 分层收缩完成，开始 Phase B"
    );

    // ---- LLM 三步结构化推断 ----
    use ramaria_memory::inference::confidence::ConfidenceConfig;
    use ramaria_memory::inference::drift::DriftConfig;
    use ramaria_memory::inference::inferrer::InferrerConfig;
    use ramaria_memory::inference::run_phase_b_inference;

    let llm = engine.llm_ref();
    let inferrer_config = InferrerConfig::from(engine.config().inference.inferrer.clone());
    // 扩展特征（时延分布 + 情绪沿链走势）独立开关，默认开启
    let causal_extended_enabled = engine
        .config()
        .inference
        .upgrade
        .causal_latency_emotion_trend;
    let phase_b_result = match run_phase_b_inference(
        llm.as_ref(),
        storage,
        &stats_summary,
        &persona_owned,
        &inferrer_config,
        causal_extended_enabled,
    )
    .await
    {
        Ok(result) => {
            info!(
                persona_uid = %persona_owned,
                saved = result.traits_saved,
                updated = result.traits_updated,
                deprecated = result.traits_deprecated,
                source = ?result.source,
                "L3 Phase B 推断完成"
            );
            result
        }
        Err(e) => {
            error!(persona_uid = %persona_owned, error = %e, "L3 Phase B 推断失败");
            if job_id > 0
                && let Err(mark_err) = job_manager
                    .mark_failed(job_id, &format!("Phase B 推断失败: {e}"))
                    .await
            {
                // 标记失败不覆盖原始错误（原始错误已由上面的 error! 记录）
                warn!(
                    job_id,
                    error = %mark_err,
                    "标记 L3 推断任务失败状态失败（不覆盖原始错误）"
                );
            }
            return;
        }
    };

    // ---- 置信度更新 + 漂移检测 ----
    use ramaria_memory::inference::run_phase_c_update;

    // 判断是否为首轮推断：以"本轮是否产出活跃 trait"为准
    // （稳定轮只产 Keep，traits_updated/deprecated 均为 0 但不是首轮）
    let is_first_round = is_first_inference_round(&phase_b_result);

    let confidence_config = ConfidenceConfig::from(engine.config().inference.confidence.clone());
    let mut drift_config = DriftConfig::from(engine.config().inference.drift.clone());
    // 漂移检测是否从快照恢复真实旧分布（配置开关）
    drift_config.restore_real_distribution = engine
        .config()
        .inference
        .upgrade
        .drift_restore_real_distribution;
    match run_phase_c_update(
        &confidence_config,
        &drift_config,
        storage,
        &persona_owned,
        &phase_b_result.traits,
        &events,
        is_first_round,
    )
    .await
    {
        Ok(phase_c_result) => {
            info!(
                persona_uid = %persona_owned,
                traits_updated = phase_c_result.traits_updated,
                evidence_saved = phase_c_result.evidence_saved,
                has_drift = phase_c_result.has_significant_drift,
                drift_categories = ?phase_c_result.drift_categories,
                "L3 Phase C 更新完成"
            );
        }
        Err(e) => {
            error!(persona_uid = %persona_owned, error = %e, "L3 Phase C 更新失败");
            // 失败不阻塞事件吸收标记——traits 已写入，confidence 保持初始值
        }
    };

    // ---- 持久化本轮画像分布快照（作为下一轮漂移检测的旧分布基准） ----
    // 语义：快照 = "上一轮已吸收画像分布"，供下一轮 Phase C 漂移检测对比。
    // 因此写入必须在 Phase C 之后、事件吸收标记之前；本轮事件无论 Phase C 成败
    // 都会在下方被标记吸收，故快照随本轮吸收一并持久化，避免下一轮读到的仍是
    // 更早轮次的分布（同轮自比 / 跨轮累积均值）。
    // 注意：stats_summary.categories 已在 Phase A 后经分层收缩（含跨用户先验），
    // 写入快照的是收缩后分布——与"统计 → 分层收缩 → 写快照 / Phase B"的顺序一致。
    let mut snapshot_count = 0usize;
    for cat_stats in &stats_summary.categories {
        let snapshot_json = serde_json::json!({
            "category": cat_stats.category,
            "event_count": cat_stats.event_count,
            "n_effective": cat_stats.n_eff,
            "valence_mean": cat_stats.valence_mean,
            "valence_std": cat_stats.valence_std,
            "share_mean": cat_stats.share_mean,
        });

        let snapshot = ramaria_core::types::ClusterSnapshot {
            id: 0,
            persona_uid: persona_owned.clone(),
            category: cat_stats.category.clone(),
            cluster_label: format!("cluster_{}", cat_stats.category),
            samples: Some(snapshot_json.to_string()),
            count: cat_stats.event_count as i32,
            is_current: true,
            created_at: now_ms(),
            semantic_label: None,
            semantic_label_embedding: None,
        };

        match storage.save_cluster_snapshot(&snapshot).await {
            Ok(_) => snapshot_count += 1,
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    category = %cat_stats.category,
                    error = %e,
                    "写入聚类快照失败（单条跳过，不影响其他分类）"
                );
            }
        }
    }

    info!(
        persona_uid = %persona_owned,
        job_id,
        snapshot_count,
        total_categories = stats_summary.categories.len(),
        "L3 画像分布快照已写入（作为下一轮漂移检测的旧分布基准）"
    );

    // ---- 标记事件已吸收 ----
    let event_ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    if !event_ids.is_empty() {
        match storage.mark_events_absorbed(&event_ids).await {
            Ok(_) => {
                info!(
                    persona_uid = %persona_owned,
                    event_count = event_ids.len(),
                    "L3 推断：已标记事件吸收"
                );
            }
            Err(e) => {
                // 标记幂等：下次 L3 会重新吸收，失败不阻断流程；错误级别提升以便可观测。
                error!(persona_uid = %persona_owned, error = %e, "L3 推断：标记事件吸收失败");
            }
        }
    }

    // 标记任务完成
    if job_id > 0
        && let Err(e) = job_manager.mark_completed(job_id).await
    {
        warn!(job_id, %e, "标记 L3 推断任务完成失败（已执行，仅状态未更新）");
    }

    info!(
        persona_uid = %persona_owned,
        job_id,
        "L3 推断全流程（Phase A→B→C）完成"
    );
}

// =========================================================
// L3 首轮判定
// =========================================================

/// 判断本轮 L3 是否为首轮推断（= Phase B 未产出任何活跃 trait）。
///
/// 语义说明:
/// - 首轮没有可对比的旧画像分布，漂移检测按语义跳过（`is_first_round=true`）。
/// - "标签/含义未变"的稳定轮只产 `Keep`：`traits_updated` / `traits_deprecated`
///   均为 0，但 `trait_ids` 非空——此时**不是首轮**，必须继续执行漂移检测，
///   否则漂移发现会被推迟一整个推断周期。
pub(super) fn is_first_inference_round(
    phase_b_result: &ramaria_memory::inference::PhaseBResult,
) -> bool {
    phase_b_result.trait_ids.is_empty()
}
