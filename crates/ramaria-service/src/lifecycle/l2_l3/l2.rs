//! crates/ramaria-service/src/lifecycle/l2_l3/l2.rs - Ramaria L2 事件提取触发与执行
//!
//! 设计特点:
//! - `check_l2_trigger`：遍历全部 persona，未吸收 L1 ≥ 阈值 → 事件提取；另处理无主 L1 归属
//! - `run_l2_extraction`：经 `JobManager` 包裹（指数退避重试）；成功后按开关执行知识事实抽取并级联 L3
//! - 停止位判定 `shutdown_requested`：`None` = 调用方没有宿主循环，不中断
//! - 所有 LLM 失败均不阻塞级联，仅记日志降级；日志只记 persona 与计数，不记原文

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ramaria_core::traits::EmbeddingProvider;
use ramaria_memory::event::{EventExtractor, EventExtractorConfig};
use ramaria_memory::job::{JobManager, JobResult, JobType};
use tracing::{error, info, warn};

use crate::engine::Engine;

use super::l3::check_l3_trigger;
use super::unbound::process_unbound_l1_for_l2;

/// 宿主停止位是否已置位（`None` = 调用方没有宿主循环，不中断）。
pub(super) fn shutdown_requested(shutdown: Option<&AtomicBool>) -> bool {
    shutdown.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

// =========================================================
// L2 事件提取触发检查（路径 A + 路径 B 共用的触发判定）
// =========================================================

/// 检查 L2 事件提取触发条件（路径 A：即时触发）。
///
/// 遍历所有 persona，检查未吸收 L1 是否 ≥ 阈值（默认 5 条）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `shutdown`: 宿主停止位；`None` 表示调用方没有宿主循环（不中断）。
pub(crate) async fn check_l2_trigger(engine: &Engine, shutdown: Option<&AtomicBool>) {
    let storage = engine.storage_ref().as_ref();

    let personas = match storage.list_personas().await {
        Ok(p) => p,
        Err(e) => {
            error!(%e, "L2 触发检查：无法列出 persona");
            return;
        }
    };

    let mut total_personas = 0usize;
    let mut checked = 0usize;
    let mut triggered = 0usize;
    let mut skipped = 0usize;

    for persona in &personas {
        total_personas += 1;
        if shutdown_requested(shutdown) {
            return;
        }

        let unabsorbed = match storage.list_unabsorbed_l1(&persona.uid).await {
            Ok(l) => l,
            Err(e) => {
                warn!(persona_uid = %persona.uid, %e, "L2 触发检查：查询未吸收 L1 失败");
                continue;
            }
        };

        checked += 1;
        let trigger_count = engine.config().thresholds.l2_trigger_count as usize;
        if unabsorbed.len() >= trigger_count {
            triggered += 1;
            info!(
                persona_uid = %persona.uid,
                unabsorbed_count = unabsorbed.len(),
                trigger_count,
                "L2 触发条件满足，启动事件提取"
            );
            // 确定对话另一方名称（仅当 personas 恰好 2 个时可靠）
            let other_name = if personas.len() == 2 {
                personas
                    .iter()
                    .find(|p| p.uid != persona.uid)
                    .map(|p| p.name.clone())
            } else {
                None
            };
            run_l2_extraction(engine, shutdown, &persona.uid, other_name).await;
        } else {
            skipped += 1;
            info!(
                persona_uid = %persona.uid,
                persona_name = %persona.name,
                unabsorbed_count = unabsorbed.len(),
                trigger_count,
                "L2 触发条件未满足（需要 {} 条未吸收 L1，当前 {} 条）",
                trigger_count,
                unabsorbed.len()
            );
        }
    }

    info!(
        total_personas,
        checked,
        triggered,
        skipped,
        "L2 触发检查完成: {} 个 persona 中 {} 个触发 L2，{} 个条件未满足",
        checked,
        triggered,
        skipped
    );

    // ---- 无主 L1 处理（数据断层修复）----
    // 导入产生的 L1 固定 persona_uid=NULL，persona 循环查不到它们，
    // L2 触发条件永不满足 → 事件恒为 0。此处单独把无主 L1 按来源
    // session 的归属 persona 归并，满足计数阈值即触发 L2 提取。
    let unbound_stats = process_unbound_l1_for_l2(
        engine,
        shutdown,
        engine.config().thresholds.l2_trigger_count as usize,
        0.0,
    )
    .await;

    info!(
        ?unbound_stats,
        "L2 触发检查：无主 L1 处理完成（归属 {} / 无法归属 {} / 触发组 {} / 待下次组 {}）",
        unbound_stats.attributed,
        unbound_stats.unattributable,
        unbound_stats.triggered_personas,
        unbound_stats.pending_groups
    );
}

/// 执行一次 L2 事件提取（通过 JobManager 包裹，带重试和可观测性）。
///
/// `other_persona_name` 用于双向对话场景的角色区分：当已知对话另一方时，
/// EventExtractor 会在 Prompt 中注入角色提示，帮助 LLM 正确区分"用户"
/// 与"另一方"的行为归属。
///
/// 重试策略:
/// - LLM 调用失败 → 可重试（JobResult::Retryable），最多 3 次，指数退避。
/// - 存储写入失败 → 同上可重试。
/// - 成功但无事件 → 视为 Success（正常情况，非错误）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `shutdown`: 宿主停止位（供提取成功后的 L3 级联透传）。
/// - `persona_uid`: 目标人格。
/// - `other_persona_name`: 对话另一方名称（仅恰好两个 persona 时给出）。
pub(super) async fn run_l2_extraction(
    engine: &Engine,
    shutdown: Option<&AtomicBool>,
    persona_uid: &str,
    other_persona_name: Option<String>,
) {
    let storage = engine.storage_ref().as_ref();
    let persona_owned = persona_uid.to_string();
    // auto_fact_detect 增强抽取所需的配置快照（总开关门控）与 embedding 引用（可选）。
    // 仅开关开启时读取 embedding（快照在锁外使用，不跨 `.await` 持锁）。
    let knowledge_enabled = engine.config().knowledge.auto_fact_detect;
    let knowledge_config = engine.config().knowledge.clone();
    let embedding: Option<Arc<dyn EmbeddingProvider>> = if knowledge_enabled {
        engine.embedding_ref()
    } else {
        None
    };
    // LLM 快照按轮次现取（热更新语义；一次提取全程使用同一份快照）
    let llm = engine.llm_ref();
    let job_manager = JobManager::with_defaults(storage);
    let payload = serde_json::json!({ "persona_uid": &persona_owned }).to_string();

    // 通过 JobManager 包裹执行：create → running → execute → completed/failed
    // 重试由 JobManager 内部处理（指数退避，最大 3 次）
    let other_name = other_persona_name.clone();
    let job_result = job_manager
        .execute_with_retry(JobType::EventExtract, Some(&payload), None, || {
            // 每次尝试都新建 EventExtractor（提取器创建代价低，且避免重试时复用状态）
            let config = EventExtractorConfig {
                other_persona_name: other_name.clone(),
                cluster_delay_ms: engine.config().thresholds.cluster_delay_ms,
                temperature: engine.config().event_extraction.temperature,
                max_tokens: engine.config().event_extraction.max_tokens,
                max_events: engine.config().event_extraction.max_events,
                // 触发阈值与调度器保持一致：调度器按计数/时间触发后，
                // 提取器内部 should_trigger 用同一阈值二次确认，避免自定义阈值下静默跳过。
                trigger_count: engine.config().thresholds.l2_trigger_count as i64,
                trigger_days: engine.config().thresholds.l2_trigger_days as i64,
                // L2 聚类去重指纹：从 [cache] 配置组传播
                l2_fingerprint_enabled: engine.config().cache.l2_fingerprint_enabled,
                l2_similarity_threshold: engine.config().cache.l2_similarity_threshold,
                l2_recent_events_limit: engine.config().cache.l2_recent_events_limit,
                // 降级事件动态置信度开关：从 [event_extraction] 配置组传播
                degrade: ramaria_memory::event::DegradeConfig {
                    dynamic_confidence_enabled: engine
                        .config()
                        .event_extraction
                        .degraded_confidence_enabled,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut extractor = EventExtractor::new(llm.as_ref(), storage, config);
            let uid = persona_owned.clone();
            let know_enabled = knowledge_enabled;
            let know_config = knowledge_config.clone();
            let embedding_owned = embedding.clone();
            async move {
                // auto_fact_detect 需要"本批 L1"作为线索→断言（策略②）输入：
                // 事件提取成功后本批 L1 会被标记 absorbed，故在提取前捕获待吸收列表。
                // 仅开关开启时预读，默认关闭路径不增加额外查询（保持既有行为）。
                let l1_batch = if know_enabled {
                    match storage.list_unabsorbed_l1(&uid).await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(
                                persona_uid = %uid,
                                error = %e,
                                "事实抽取：预读本批 L1 失败，降级为空（不阻塞事件提取）"
                            );
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                match extractor.extract_events(&uid).await {
                    Ok(events) if events.is_empty() => {
                        info!(persona_uid = %uid, "L2 提取完成，无新事件");
                        JobResult::Success
                    }
                    Ok(events) => {
                        info!(
                            persona_uid = %uid,
                            event_count = events.len(),
                            "L2 事件提取完成"
                        );
                        // auto_fact_detect 增强抽取：开关开启且本批有事件才执行；
                        // 编排器内部静默降级，任何失败不改变 Job 结果、不阻塞 L3 级联。
                        if know_enabled {
                            let report = crate::fact_extract::run_fact_extraction(
                                storage,
                                &know_config,
                                &uid,
                                &l1_batch,
                                &events,
                                embedding_owned.as_deref(),
                            )
                            .await;
                            info!(
                                persona_uid = %uid,
                                regular = report.regular_candidates,
                                implied = report.implied_candidates,
                                l1_evidence = report.l1_candidates,
                                deduped = report.deduped,
                                promoted = report.promoted_active,
                                overwritten = report.overwritten,
                                candidates_saved = report.candidates_saved,
                                errors = report.errors,
                                "auto_fact_detect 事实抽取完成（增强层）"
                            );
                        }
                        JobResult::Success
                    }
                    Err(e) => {
                        // LLM 调用失败或存储写入失败，标记为可重试
                        warn!(
                            persona_uid = %uid,
                            error = %e,
                            "L2 事件提取失败，将重试"
                        );
                        JobResult::Retryable(e.to_string())
                    }
                }
            }
        })
        .await;

    match job_result {
        Ok(job_id) => {
            info!(persona_uid = %persona_owned, job_id, "L2 事件提取任务完成");
            // L2 成功后级联检查 L3（路径 A）
            check_l3_trigger(engine, shutdown, &persona_owned).await;
        }
        Err(e) => {
            error!(
                persona_uid = %persona_owned,
                error = %e,
                "L2 事件提取失败（已达最大重试次数），L3 级联跳过"
            );
        }
    }
}
