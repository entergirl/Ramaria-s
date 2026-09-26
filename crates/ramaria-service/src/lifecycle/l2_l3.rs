//! crates/ramaria-service/src/lifecycle/l2_l3.rs - L2 事件提取与 L3 性格推断调度
//!
//! 设计特点:
//! - `check_l2_trigger`：遍历全部 persona，未吸收 L1 ≥ 阈值 → 事件提取；另处理无主 L1 归属
//! - `check_l3_trigger`：未吸收事件 ≥ 计数阈值，或时间线（`> 0`）下最早事件超龄 → 性格推断
//! - L2 提取经 `JobManager` 包裹（指数退避重试）；成功后按开关执行知识事实抽取并级联 L3
//! - L3 全流程：Phase A 统计 + 分层收缩 → Phase B LLM 推断 → Phase C 置信度更新 + 漂移检测
//! - `spawn_scheduler` 后台定时任务：先延迟再周期检查（按 60 秒分片感知停止位）
//! - 所有 LLM 失败均不阻塞级联，仅记日志降级；停止位由宿主以共享原子标志传入

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{MemoryL1, now_ms};
use ramaria_memory::event::{EventExtractor, EventExtractorConfig};
use ramaria_memory::job::{JobManager, JobResult, JobType};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::engine::Engine;

/// 宿主停止位是否已置位（`None` = 调用方没有宿主循环，不中断）。
fn shutdown_requested(shutdown: Option<&AtomicBool>) -> bool {
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
async fn run_l2_extraction(
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

// =========================================================
// 无主 L1 处理（数据断层修复）：L2 触发链路补全
// =========================================================

/// 无主 L1 处理统计（供日志聚合与可观测性）。
#[derive(Debug, Default, Clone)]
struct UnboundL1ProcessStats {
    /// 无主未吸收 L1 总数
    total: usize,
    /// 已归属到 persona 的条数（可触发候选）
    attributed: usize,
    /// 无法归属的条数（来源 session 缺失或 session.persona_uid 为 NULL）
    unattributable: usize,
    /// 达到触发条件并启动 L2 提取的 persona 组数
    triggered_personas: usize,
    /// 未达触发条件、保持无主状态待下次检查的 persona 组数
    pending_groups: usize,
}

/// 处理"无主"L1（`persona_uid IS NULL`，导入产生的 L1 属此类）。
///
/// 背景（数据断层修复）:
/// - 导入的 L1 摘要固定 NULL 归属（摘要不应被特定画像独占），
///   但 L2 事件提取严格按 persona 遍历 `list_unabsorbed_l1(persona_uid)`，
///   NULL 归属的 L1 对任何 persona 都查不到 → L2 永不触发 → 事件恒为 0。
/// - 本函数打通该链路：把无主 L1 按来源 session 的归属 persona 归并，
///   满足触发条件（计数/时间二选一）时回填 persona_uid 后走标准 L2 提取。
///
/// 归属规则:
/// - 每条无主 L1 的 `session_id` → `sessions.persona_uid` 即其归属 persona
///   （导入场景下 session 归属为处理侧 persona，即"对方"画像）。
/// - session 不存在 / session.persona_uid 为 NULL / 查询失败 → 无法归属，
///   保持无主状态（记 warn + 统计），不阻塞其他组的处理。
///
/// 触发语义:
/// - `trigger_count > 0`：启用计数触发（路径 A），未吸收 L1 ≥ 该值即触发。
/// - `trigger_days > 0.0`：启用时间触发（路径 B），最早无主 L1 年龄 ≥ 该值即触发。
/// - 两者均 > 0 时满足任一即触发；均为 0 时不触发（安全默认）。
///
/// 幂等与降级:
/// - 归属仅更新仍为 NULL 且未吸收的 L1（`assign_l1_persona_uid` 幂等），
///   重复调用不会覆盖既有归属。
/// - 归属失败仅跳过该组（记 error），不阻塞其他组的 L2 提取。
/// - 提取成功时 L1 被标记 absorbed，后续检查自然跳过；
///   提取失败（LLM 不可用）时 L1 已归属到 persona，由标准 persona 循环负责重试。
async fn process_unbound_l1_for_l2(
    engine: &Engine,
    shutdown: Option<&AtomicBool>,
    trigger_count: usize,
    trigger_days: f64,
) -> UnboundL1ProcessStats {
    let storage = engine.storage_ref().as_ref();
    let mut stats = UnboundL1ProcessStats::default();

    // 1. 读取无主未吸收 L1（storage 既有通道，此前仅检索索引使用）
    let unbound = match storage.list_unabsorbed_l1_unbound().await {
        Ok(list) => list,
        Err(e) => {
            error!(error = %e, "L2 无主 L1 处理：查询无主未吸收 L1 失败");
            return stats;
        }
    };
    stats.total = unbound.len();
    if unbound.is_empty() {
        return stats;
    }
    info!(
        total = unbound.len(),
        "L2 触发检查：发现无主 L1，开始归属处理（数据断层修复链路）"
    );

    // 2. 按来源 session 分组（同一 session 的 L1 归属相同，去重查询）
    let mut by_session: HashMap<Uuid, Vec<&MemoryL1>> = HashMap::new();
    for l1 in &unbound {
        by_session.entry(l1.session_id).or_default().push(l1);
    }

    // 3. 解析每个 session 的归属 persona_uid（逐 session 查询，失败记 warn 不中断）
    let mut session_owner: HashMap<Uuid, Option<String>> = HashMap::with_capacity(by_session.len());
    for sid in by_session.keys() {
        let owner = match storage.get_session(*sid).await {
            Ok(Some(s)) => s.persona_uid,
            Ok(None) => {
                warn!(session_id = %sid, "L2 无主 L1 处理：来源 session 不存在，无法归属");
                None
            }
            Err(e) => {
                warn!(session_id = %sid, error = %e, "L2 无主 L1 处理：查询来源 session 失败，无法归属");
                None
            }
        };
        session_owner.insert(*sid, owner);
    }

    // 4. 按归属 persona 聚合（无法归属的计入统计，保持无主状态）
    let mut by_persona: HashMap<String, Vec<&MemoryL1>> = HashMap::new();
    for (sid, l1s) in &by_session {
        match session_owner.get(sid).and_then(|o| o.as_ref()) {
            Some(owner) => {
                let entry = by_persona.entry(owner.clone()).or_default();
                entry.extend(l1s.iter().copied());
                stats.attributed += l1s.len();
            }
            None => {
                stats.unattributable += l1s.len();
            }
        }
    }

    if by_persona.is_empty() {
        debug!(
            unattributable = stats.unattributable,
            "L2 无主 L1 处理：无任何可归属候选，结束"
        );
        return stats;
    }

    // 5. 懒加载 persona 列表（确定对话另一方名称，仅当存在归属候选时查询）
    let personas = match storage.list_personas().await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "L2 无主 L1 处理：查询 persona 列表失败，另一方名称为空");
            Vec::new()
        }
    };

    let now = now_ms();
    let ms_per_day: i64 = 86_400_000;

    // 6. 逐 persona 检查触发条件并执行 L2 提取
    for (owner, l1s) in &by_persona {
        if shutdown_requested(shutdown) {
            warn!("L2 无主 L1 处理：收到停止信号，中断后续处理");
            break;
        }

        // 计数触发（路径 A）与时间触发（路径 B）独立判定
        let count_ok = trigger_count > 0 && l1s.len() >= trigger_count;
        let oldest_age_days = l1s
            .iter()
            .map(|l| l.created_at)
            .min()
            .map(|min| (now.saturating_sub(min)) as f64 / ms_per_day as f64)
            .unwrap_or(0.0);
        let age_ok = trigger_days > 0.0 && oldest_age_days >= trigger_days;

        if !(count_ok || age_ok) {
            stats.pending_groups += 1;
            info!(
                persona_uid = %owner,
                l1_count = l1s.len(),
                oldest_days = %format!("{oldest_age_days:.1}"),
                trigger_count,
                trigger_days,
                "L2 无主 L1 处理：触发条件未满足，保持无主状态待下次检查"
            );
            continue;
        }

        // 回填 persona_uid（幂等：仅更新仍为 NULL 且未吸收的记录）
        let ids: Vec<Uuid> = l1s.iter().map(|l| l.id).collect();
        match storage.assign_l1_persona_uid(&ids, owner).await {
            Ok(assigned) => {
                info!(
                    persona_uid = %owner,
                    assigned,
                    total = ids.len(),
                    "L2 无主 L1 处理：已归属 {} 条无主 L1 到 persona（{} 条跳过，可能已归属/已吸收）",
                    assigned,
                    ids.len().saturating_sub(assigned)
                );
            }
            Err(e) => {
                error!(
                    persona_uid = %owner,
                    error = %e,
                    "L2 无主 L1 处理：归属失败，跳过该组（不阻塞其他组）"
                );
                continue;
            }
        }

        // 确定对话另一方名称（仅当 personas 恰好 2 个时可靠）
        let other_name = if personas.len() == 2 {
            personas
                .iter()
                .find(|p| p.uid.as_str() != owner.as_str())
                .map(|p| p.name.clone())
        } else {
            None
        };

        stats.triggered_personas += 1;
        info!(
            persona_uid = %owner,
            l1_count = l1s.len(),
            "L2 无主 L1 处理：触发条件满足，启动事件提取（数据断层修复）"
        );
        run_l2_extraction(engine, shutdown, owner, other_name).await;
    }

    stats
}

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
async fn run_l3_inference(engine: &Engine, persona_uid: &str) {
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
fn is_first_inference_round(phase_b_result: &ramaria_memory::inference::PhaseBResult) -> bool {
    phase_b_result.trait_ids.is_empty()
}

// =========================================================
// 后台定时任务：L2/L3 定时触发
// =========================================================

/// 启动后台 L2/L3 定时检查任务。
///
/// 逻辑:
/// - 首轮检查延迟 `first_delay_seconds` 秒执行（避开宿主启动阶段）；
/// - 之后每 `interval_seconds` 秒检查一轮，遍历所有 persona（时间线阈值来自配置，
///   `> 0` 才启用）：
///   - 最早未吸收 L1 超过 `[thresholds].l2_trigger_days` 天 → 触发 L2 事件提取
///   - 最早未吸收事件超过 `[thresholds].l3_trigger_days` 天 → 触发 L3 性格推断
/// - 停止位置位后退出；等待按 60 秒分片，每片感知一次停止位。
///
/// 参数:
/// - `engine`: 服务层引擎（`Arc` 共享）。
/// - `shutdown`: 宿主停止位（true = 循环应在下一轮退出）。
/// - `first_delay_seconds`: 首轮检查前的延迟秒数。
/// - `interval_seconds`: 两轮检查之间的间隔秒数。
pub(crate) fn spawn_scheduler(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
    first_delay_seconds: u64,
    interval_seconds: u64,
) -> tokio::task::JoinHandle<()> {
    info!(
        interval_seconds,
        first_delay_seconds, "后台 L2/L3 定时检查任务启动"
    );

    tokio::spawn(async move {
        // 首次延迟：避免在宿主启动阶段执行检查
        tokio::time::sleep(Duration::from_secs(first_delay_seconds)).await;

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("L2/L3 定时检查任务收到停止信号，退出");
                return;
            }

            // 执行定时检查
            run_scheduled_check(&engine, Some(&shutdown)).await;

            // 等待下一次检查（可中断，每 60s 检查一次停止信号）
            let mut sleep_secs = interval_seconds;
            while sleep_secs > 0 && !shutdown.load(Ordering::Relaxed) {
                let chunk = sleep_secs.min(60);
                tokio::time::sleep(Duration::from_secs(chunk)).await;
                sleep_secs = sleep_secs.saturating_sub(chunk);
            }
        }
    })
}

/// 执行一次性 L2/L3 定时检查。
///
/// 流程:
/// 1. L1 补扫：消费封存失败遗留的摘要任务；
/// 2. 遍历所有 persona（时间线阈值来自配置，`> 0` 才启用）：
///    - 最早未吸收 L1 超过 `[thresholds].l2_trigger_days` 天 → 触发 L2 事件提取（时间触发路径）；
///    - 最早未吸收事件超过 `[thresholds].l3_trigger_days` 天 → 触发 L3 性格推断（时间触发路径）；
/// 3. 无主 L1 按时间阈值（`[thresholds].l2_trigger_days`）归属并触发提取。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `shutdown`: 宿主停止位；`None` 表示调用方没有宿主循环（不中断）。
pub(crate) async fn run_scheduled_check(engine: &Engine, shutdown: Option<&AtomicBool>) {
    let storage = engine.storage_ref().as_ref();
    debug!("L2/L3 定时检查开始");

    // L1 补扫：消费封存时 L1 生成失败遗留的 pending 任务（LLM 恢复后自动补跑摘要），
    // 使"失败登记"不再空转（启动补扫之外的周期性消费点）。
    let l1_retried = crate::lifecycle::l1::retry_pending_l1_jobs(engine).await;
    if l1_retried > 0 {
        info!(l1_retried, "L2/L3 定时检查：L1 补扫完成");
    }

    let personas = match storage.list_personas().await {
        Ok(p) => p,
        Err(e) => {
            error!(%e, "L2/L3 定时检查：无法列出 persona");
            return;
        }
    };

    let now = now_ms();
    let ms_per_day: i64 = 86_400_000;
    // 时间线阈值来自配置：`> 0` 才启用该时间线（`0` = 不按时间触发；计数线不受影响）
    let l2_trigger_days = engine.config().thresholds.l2_trigger_days as f64;
    let l3_trigger_days = engine.config().thresholds.l3_trigger_days as f64;

    for persona in &personas {
        if shutdown_requested(shutdown) {
            return;
        }

        // ---- L2 时间触发 ----
        // 最早未吸收 L1 超过配置阈值天数则触发（阈值 > 0 才启用）
        match storage.list_unabsorbed_l1(&persona.uid).await {
            Ok(l1_list) => {
                if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
                    let age_days = (now - oldest) as f64 / ms_per_day as f64;
                    if l2_trigger_days > 0.0 && age_days >= l2_trigger_days {
                        info!(
                            persona_uid = %persona.uid,
                            %age_days,
                            l1_count = l1_list.len(),
                            trigger_days = l2_trigger_days,
                            "L2 定时触发（路径 B：最早未吸收 L1 超过阈值天数）"
                        );
                        // 定时路径也确定对话另一方
                        let other_name = if personas.len() == 2 {
                            personas
                                .iter()
                                .find(|p| p.uid != persona.uid)
                                .map(|p| p.name.clone())
                        } else {
                            None
                        };
                        run_l2_extraction(engine, shutdown, &persona.uid, other_name).await;
                    }
                }
            }
            Err(e) => {
                warn!(persona_uid = %persona.uid, %e, "L2 定时检查：查询 L1 失败");
            }
        }

        // ---- L3 时间触发 ----
        // 最早未吸收事件超过配置阈值天数则触发（阈值 > 0 才启用）
        match storage.list_unabsorbed_events(&persona.uid).await {
            Ok(events) => {
                if let Some(oldest) = events.iter().map(|e| e.start).min() {
                    let age_days = (now - oldest) as f64 / ms_per_day as f64;
                    if l3_trigger_days > 0.0 && age_days >= l3_trigger_days {
                        info!(
                            persona_uid = %persona.uid,
                            %age_days,
                            event_count = events.len(),
                            trigger_days = l3_trigger_days,
                            "L3 定时触发（路径 B：最早未吸收事件超过阈值天数）"
                        );
                        run_l3_inference(engine, &persona.uid).await;
                    }
                }
            }
            Err(e) => {
                warn!(persona_uid = %persona.uid, %e, "L3 定时检查：查询事件失败");
            }
        }
    }

    // ---- 无主 L1 时间触发（数据断层修复）----
    // 定时路径：最早无主 L1 年龄 ≥ 阈值时触发 L2 提取（导入数据同样适用），
    // 与 persona 循环的时间触发（路径 B）语义一致。
    let unbound_stats = process_unbound_l1_for_l2(
        engine,
        shutdown,
        0,
        engine.config().thresholds.l2_trigger_days as f64,
    )
    .await;
    info!(
        ?unbound_stats,
        "L2/L3 定时检查：无主 L1 处理完成（归属 {} / 无法归属 {} / 触发组 {} / 待下次组 {}）",
        unbound_stats.attributed,
        unbound_stats.unattributable,
        unbound_stats.triggered_personas,
        unbound_stats.pending_groups
    );

    debug!("L2/L3 定时检查完成");
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        MockLlm, engine_with_db, engine_with_llm_and_config, seed_l1, seed_persona,
    };
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use ramaria_core::types::MemoryEvent;
    use std::time::Instant;

    /// 空库（无 persona）：检查正常返回，不产生任何任务。
    #[tokio::test]
    async fn empty_store_is_noop() {
        let (engine, storage, dir) = engine_with_db("l2l3-empty").await;

        check_l2_trigger(&engine, None).await;

        let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(pending.is_empty(), "空库不应创建任务: {pending:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未达阈值：不触发提取（L1 保持未吸收、无事件、无任务）。
    #[tokio::test]
    async fn below_threshold_does_not_trigger() {
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-below", MockLlm::local(), RamariaConfig::default())
                .await;
        seed_persona(&storage, "char-0001").await;
        // 1 条未吸收 L1 < 默认阈值 5
        seed_l1(
            &storage,
            "char-0001",
            "用户提到最近在准备考试",
            Some("考试"),
            1_000,
        )
        .await;

        check_l2_trigger(&engine, None).await;

        // L1 保持未吸收（未触发提取 → 未被吸收）
        let unabsorbed = storage
            .list_unabsorbed_l1("char-0001")
            .await
            .expect("查询未吸收 L1 应成功");
        assert_eq!(unabsorbed.len(), 1, "未达阈值不应吸收 L1");

        // 无事件产出
        let events = storage
            .list_events_by_persona("char-0001", 0, 100)
            .await
            .expect("查询事件应成功");
        assert!(events.is_empty(), "未达阈值不应产出事件");

        // 无事件提取任务登记
        let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(
            !pending
                .iter()
                .any(|(_, job_type, _)| job_type == JobType::EventExtract.as_str()),
            "未达阈值不应创建事件提取任务: {pending:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无主 L1：达到计数阈值 → 按来源会话归属到 persona，并进入 L2 提取链路。
    #[tokio::test]
    async fn unbound_l1_is_attributed_and_triggers() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_count = 1;
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l2l3-unbound",
            MockLlm::with_reply(r#"{"events": []}"#),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;

        // 无主 L1（persona_uid = None），来源会话归 char-0001
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        let l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

        check_l2_trigger(&engine, None).await;

        // 归属回填为来源会话的 persona
        let stored = storage
            .list_memory_l1(session.id)
            .await
            .expect("读取 L1 应成功");
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].persona_uid.as_deref(),
            Some("char-0001"),
            "无主 L1 应回填到来源会话的 persona"
        );
        // 无主通道清空
        let unbound = storage
            .list_unabsorbed_l1_unbound()
            .await
            .expect("查询无主 L1 应成功");
        assert!(unbound.is_empty(), "归属后无主通道应清空");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 时间触发（路径 B）：最早无主 L1 年龄 ≥ 阈值时触发（即使计数不足）。
    #[tokio::test]
    async fn unbound_l1_age_trigger() {
        let mut config = RamariaConfig::default();
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l2l3-unbound-age",
            MockLlm::with_reply(r#"{"events": []}"#),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;

        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        // 1 条 10 天前的无主 L1（时间触发阈值 7 天，计数阈值不启用）
        let mut l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
        l1.created_at = now_ms() - 10 * 86_400_000;
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

        let stats = process_unbound_l1_for_l2(&engine, None, 0, 7.0).await;

        assert_eq!(stats.total, 1);
        assert_eq!(stats.triggered_personas, 1, "年龄 ≥ 7 天应触发 L2");
        assert_eq!(stats.pending_groups, 0);

        let bound = storage
            .list_recent_l1_by_persona("char-0001", 100)
            .await
            .expect("查询 L1 应成功");
        assert_eq!(bound.len(), 1, "时间触发同样应归属 L1");
        assert_eq!(bound[0].persona_uid.as_deref(), Some("char-0001"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 停止位置位：不处理任何 persona（无主 L1 保持无主、未回填）。
    #[tokio::test]
    async fn shutdown_flag_interrupts_check() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_count = 1;
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-shutdown", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        // 无主 L1 + 一条已归属未吸收 L1（两类输入都应保持原状）
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        let l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
        seed_l1(&storage, "char-0001", "用户提到最近在准备考试", None, 1_000).await;

        let flag = AtomicBool::new(true);
        check_l2_trigger(&engine, Some(&flag)).await;

        // 无主 L1 保持无主
        let unbound = storage
            .list_unabsorbed_l1_unbound()
            .await
            .expect("查询应成功");
        assert_eq!(unbound.len(), 1, "停止位置位时无主 L1 不应被归属");
        assert!(unbound[0].persona_uid.is_none(), "无主 L1 归属不应被回填");
        // 已归属 L1 保持未吸收（未触发提取）
        let unabsorbed = storage
            .list_unabsorbed_l1("char-0001")
            .await
            .expect("查询应成功");
        assert_eq!(unabsorbed.len(), 1, "停止位置位时不应触发提取");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L3 触发条件未满足：未吸收事件为空 → 不创建性格推断任务。
    #[tokio::test]
    async fn l3_trigger_skips_without_events() {
        let (engine, storage, dir) = engine_with_db("l2l3-l3-none").await;
        seed_persona(&storage, "char-0001").await;

        assert!(
            !check_l3_trigger(&engine, None, "char-0001").await,
            "无未吸收事件不应触发推断"
        );

        let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(
            !pending
                .iter()
                .any(|(_, job_type, _)| job_type == JobType::PersonalityInference.as_str()),
            "无未吸收事件不应创建性格推断任务: {pending:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 即时路径：L3 时间线阈值 `0` = 不按时间触发 —— 超龄事件保持未吸收、不启动推断。
    #[tokio::test]
    async fn instant_check_zero_l3_days_disables_age_trigger() {
        let mut config = RamariaConfig::default();
        config.thresholds.l3_trigger_days = 0;
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-l3-zero", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        // 40 天前的事件：即使超过默认 30 天，阈值 0 下也不应触发
        let start = now_ms() - 40 * 86_400_000;
        let mut event = MemoryEvent::new(
            "char-0001".to_string(),
            "旧事件".to_string(),
            "很久以前发生的事件".to_string(),
            start,
            start + 3_600_000,
        );
        event.confidence = 0.8;
        storage.save_event(&event).await.expect("写入事件应成功");

        assert!(
            !check_l3_trigger(&engine, None, "char-0001").await,
            "时间线阈值 0 时不应按事件年龄触发推断"
        );
        let unabsorbed = storage
            .list_unabsorbed_events("char-0001")
            .await
            .expect("查询未吸收事件应成功");
        assert_eq!(unabsorbed.len(), 1, "未触发时事件应保持未吸收");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 即时路径：L3 时间线阈值大于 0 时生效 —— 超龄事件触发推断（对照 `0` 值关闭）。
    #[tokio::test]
    async fn instant_check_age_trigger_fires_when_days_enabled() {
        let mut config = RamariaConfig::default();
        config.thresholds.l3_trigger_days = 1;
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-l3-age", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        // 2 天前的事件：超过自定义阈值 1 天 → 应触发
        let start = now_ms() - 2 * 86_400_000;
        let mut event = MemoryEvent::new(
            "char-0001".to_string(),
            "工作压力事件".to_string(),
            "用户最近工作压力很大".to_string(),
            start,
            start + 3_600_000,
        );
        event.confidence = 0.8;
        storage.save_event(&event).await.expect("写入事件应成功");

        assert!(
            check_l3_trigger(&engine, None, "char-0001").await,
            "时间线阈值大于 0 且事件超龄时应触发推断"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 即时路径：时间线阈值 `0` 不影响计数线 —— 未吸收事件达计数阈值仍触发。
    #[tokio::test]
    async fn instant_check_count_trigger_ignores_disabled_age_line() {
        let mut config = RamariaConfig::default();
        config.thresholds.l3_trigger_days = 0;
        config.thresholds.l3_trigger_count = 1;
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-l3-count", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        let start = now_ms() - 3_600_000;
        let mut event = MemoryEvent::new(
            "char-0001".to_string(),
            "工作压力事件".to_string(),
            "用户最近工作压力很大".to_string(),
            start,
            start + 600_000,
        );
        event.confidence = 0.8;
        storage.save_event(&event).await.expect("写入事件应成功");

        assert!(
            check_l3_trigger(&engine, None, "char-0001").await,
            "时间线关闭不影响计数线：达计数阈值即触发"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 后台调度任务可按停止位关停（拉起后置位，限时内退出）。
    #[tokio::test]
    async fn spawn_scheduler_stops_on_flag() {
        let (engine, _storage, dir) = engine_with_db("l2l3-spawn").await;
        let engine = Arc::new(engine);
        let shutdown = Arc::new(AtomicBool::new(false));

        let handle = spawn_scheduler(Arc::clone(&engine), Arc::clone(&shutdown), 0, 1);
        shutdown.store(true, Ordering::Release);

        // 轮询等待退出（最多 6 秒）：不依赖固定 sleep，避免慢机偶发失败
        let deadline = Instant::now() + Duration::from_secs(6);
        while !handle.is_finished() {
            assert!(
                Instant::now() < deadline,
                "停止位置位后调度任务应在限时内退出"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        handle.await.expect("调度任务不应 panic");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================
    // 定时检查时间线（配置驱动）
    // =========================================================

    /// 定时检查：L2 时间线按 `[thresholds].l2_trigger_days` 判定 —— 2 天前的未吸收
    /// L1 超过自定义阈值（1 天）时触发提取，L1 被吸收且有事件产出。
    #[tokio::test]
    async fn scheduled_check_uses_configured_l2_days() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_days = 1;
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l2l3-sched-l2",
            MockLlm::with_reply(r#"{"events": []}"#),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        // 3 条 2 天前的未吸收 L1（关键词连通 → 单簇）；计数阈值 5 未满足
        let two_days_ago = now_ms() - 2 * 86_400_000;
        for i in 0..3 {
            seed_l1(
                &storage,
                "char-0001",
                "用户最近工作压力很大",
                Some("工作压力"),
                two_days_ago + i,
            )
            .await;
        }

        run_scheduled_check(&engine, None).await;

        let unabsorbed = storage
            .list_unabsorbed_l1("char-0001")
            .await
            .expect("查询未吸收 L1 应成功");
        assert!(
            unabsorbed.is_empty(),
            "超过自定义天数阈值应触发提取并吸收 L1: {unabsorbed:?}"
        );
        let events = storage
            .list_events_by_persona("char-0001", 0, 100)
            .await
            .expect("查询事件应成功");
        assert!(!events.is_empty(), "触发提取应有事件产出（降级事件亦可）");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 定时检查：L2 时间线阈值 `0` = 不按时间触发 —— 未吸收 L1 保持未吸收、无事件产出。
    #[tokio::test]
    async fn scheduled_check_zero_l2_days_disables_age_trigger() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_days = 0;
        config.thresholds.cluster_delay_ms = 0;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l2l3-sched-l2-zero",
            MockLlm::with_reply(r#"{"events": []}"#),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        // 3 条 10 天前的未吸收 L1：即使超过默认 7 天，阈值 0 下也不应触发
        let long_ago = now_ms() - 10 * 86_400_000;
        for i in 0..3 {
            seed_l1(
                &storage,
                "char-0001",
                "用户最近工作压力很大",
                Some("工作压力"),
                long_ago + i,
            )
            .await;
        }

        run_scheduled_check(&engine, None).await;

        let unabsorbed = storage
            .list_unabsorbed_l1("char-0001")
            .await
            .expect("查询未吸收 L1 应成功");
        assert_eq!(unabsorbed.len(), 3, "阈值 0 时时间线不触发，L1 保持未吸收");
        let events = storage
            .list_events_by_persona("char-0001", 0, 100)
            .await
            .expect("查询事件应成功");
        assert!(events.is_empty(), "未触发不应产出事件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 定时检查：L3 时间线按 `[thresholds].l3_trigger_days` 判定 —— 2 天前的未吸收
    /// 事件超过自定义阈值（1 天）时触发推断，事件被吸收。
    #[tokio::test]
    async fn scheduled_check_uses_configured_l3_days() {
        let mut config = RamariaConfig::default();
        config.thresholds.l3_trigger_days = 1;
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-sched-l3", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        let start = now_ms() - 2 * 86_400_000;
        let mut event = MemoryEvent::new(
            "char-0001".to_string(),
            "工作压力事件".to_string(),
            "用户最近工作压力很大".to_string(),
            start,
            start + 3_600_000,
        );
        event.confidence = 0.8; // ≥ 0.6 才参与性格推断
        storage.save_event(&event).await.expect("写入事件应成功");

        run_scheduled_check(&engine, None).await;

        let unabsorbed = storage
            .list_unabsorbed_events("char-0001")
            .await
            .expect("查询未吸收事件应成功");
        assert!(
            unabsorbed.is_empty(),
            "超过自定义天数阈值应触发 L3 推断并吸收事件: {unabsorbed:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 定时检查：L3 时间线阈值 `0` = 不按时间触发 —— 未吸收事件保持未吸收。
    #[tokio::test]
    async fn scheduled_check_zero_l3_days_disables_age_trigger() {
        let mut config = RamariaConfig::default();
        config.thresholds.l3_trigger_days = 0;
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2l3-sched-l3-zero", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        // 40 天前的事件：即使超过默认 30 天，阈值 0 下也不应触发
        let start = now_ms() - 40 * 86_400_000;
        let mut event = MemoryEvent::new(
            "char-0001".to_string(),
            "旧事件".to_string(),
            "很久以前发生的事件".to_string(),
            start,
            start + 3_600_000,
        );
        event.confidence = 0.8;
        storage.save_event(&event).await.expect("写入事件应成功");

        run_scheduled_check(&engine, None).await;

        let unabsorbed = storage
            .list_unabsorbed_events("char-0001")
            .await
            .expect("查询未吸收事件应成功");
        assert_eq!(unabsorbed.len(), 1, "阈值 0 时时间线不触发，事件保持未吸收");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 定时检查：默认阈值（7 天）行为回归 —— 8 天前的未吸收 L1 触发提取，
    /// 6 天前的保持未吸收。
    #[tokio::test]
    async fn scheduled_check_default_l2_days_behavior() {
        let mut config = RamariaConfig::default();
        config.thresholds.cluster_delay_ms = 0;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l2l3-sched-default",
            MockLlm::with_reply(r#"{"events": []}"#),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        // char-0001：3 条 8 天前的 L1（超过默认 7 天）→ 应触发
        let over_age = now_ms() - 8 * 86_400_000;
        for i in 0..3 {
            seed_l1(
                &storage,
                "char-0001",
                "用户最近工作压力很大",
                Some("工作压力"),
                over_age + i,
            )
            .await;
        }
        // char-0002：3 条 6 天前的 L1（未达默认 7 天）→ 不应触发
        let within_age = now_ms() - 6 * 86_400_000;
        for i in 0..3 {
            seed_l1(
                &storage,
                "char-0002",
                "用户最近睡眠质量不好",
                Some("睡眠"),
                within_age + i,
            )
            .await;
        }

        run_scheduled_check(&engine, None).await;

        let triggered = storage
            .list_unabsorbed_l1("char-0001")
            .await
            .expect("查询未吸收 L1 应成功");
        assert!(
            triggered.is_empty(),
            "默认 7 天阈值下 8 天前的 L1 应触发提取"
        );
        let pending = storage
            .list_unabsorbed_l1("char-0002")
            .await
            .expect("查询未吸收 L1 应成功");
        assert_eq!(
            pending.len(),
            3,
            "6 天前的 L1 未达默认 7 天阈值，保持未吸收"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 定时检查：默认阈值（30 天）行为回归 —— 40 天前的未吸收事件触发推断并吸收，
    /// 20 天前的保持未吸收。
    #[tokio::test]
    async fn scheduled_check_default_l3_days_behavior() {
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l2l3-sched-default-l3",
            MockLlm::local(),
            RamariaConfig::default(),
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;

        let over_age = now_ms() - 40 * 86_400_000;
        let mut triggered_event = MemoryEvent::new(
            "char-0001".to_string(),
            "旧事件".to_string(),
            "很久以前发生的事件".to_string(),
            over_age,
            over_age + 3_600_000,
        );
        triggered_event.confidence = 0.8;
        storage
            .save_event(&triggered_event)
            .await
            .expect("写入事件应成功");

        let within_age = now_ms() - 20 * 86_400_000;
        let mut pending_event = MemoryEvent::new(
            "char-0002".to_string(),
            "近期事件".to_string(),
            "近期发生的事件".to_string(),
            within_age,
            within_age + 3_600_000,
        );
        pending_event.confidence = 0.8;
        storage
            .save_event(&pending_event)
            .await
            .expect("写入事件应成功");

        run_scheduled_check(&engine, None).await;

        let triggered = storage
            .list_unabsorbed_events("char-0001")
            .await
            .expect("查询未吸收事件应成功");
        assert!(
            triggered.is_empty(),
            "默认 30 天阈值下 40 天前的事件应触发推断并吸收"
        );
        let pending = storage
            .list_unabsorbed_events("char-0002")
            .await
            .expect("查询未吸收事件应成功");
        assert_eq!(
            pending.len(),
            1,
            "20 天前的事件未达默认 30 天阈值，保持未吸收"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================
    // L3 首轮判定（稳定轮不豁免漂移检测）
    // =========================================================

    /// 回归：Keep-only 稳定轮（traits_updated / traits_deprecated 均为 0，
    /// 但本轮仍产出活跃 trait）不是首轮，必须继续执行漂移检测。
    #[test]
    fn keep_only_round_is_not_first_round() {
        use ramaria_memory::inference::{PhaseBResult, PhaseBSource};

        let keep_only = PhaseBResult {
            traits_saved: 0,
            traits_updated: 0,
            traits_deprecated: 0,
            source: PhaseBSource::LlmInference,
            trait_ids: vec![1, 2],
            traits: vec![],
        };
        assert!(
            !is_first_inference_round(&keep_only),
            "Keep-only 稳定轮不应被判为首轮（否则漂移检测被跳过）"
        );

        let no_trait = PhaseBResult {
            traits_saved: 0,
            traits_updated: 0,
            traits_deprecated: 0,
            source: PhaseBSource::MockFallback,
            trait_ids: vec![],
            traits: vec![],
        };
        assert!(
            is_first_inference_round(&no_trait),
            "仅当本轮无任何活跃 trait 时才视为首轮"
        );
    }
}
