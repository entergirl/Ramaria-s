//! crates/ramaria-app/src/session_lifecycle/l2_l3_scheduler.rs - L2 事件提取 & L3 性格推断调度
//!
//! 设计特点:
//! - 实现 `SessionLifecycle` 的 L2 触发检查（路径 A 即时 + 路径 B 定时）与 L3 级联触发
//! - `check_l2_trigger` 遍历所有 persona，检查未吸收 L1 ≥ 阈值 → `run_l2_extraction`
//! - `check_l3_trigger` 检查未吸收事件 ≥ 阈值或最早事件 > 天数 → `run_l3_inference`
//! - L2 事件提取通过 JobManager 包裹，含指数退避重试（最多 3 次）
//! - L3 全流程：Phase A 统计 → Phase B LLM 推断 → Phase C 置信度更新 + 漂移检测
//! - `spawn_l2_l3_scheduler` 后台线程（Thread B）每 24h 定时检查，首次延迟 5min
//! - 所有 LLM 调用失败均不阻塞级联，仅记录 warn/error 日志

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::{MemoryL1, now_ms};
use ramaria_memory::event::{EventExtractor, EventExtractorConfig};
use ramaria_memory::job::{JobManager, JobResult, JobType};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::SessionLifecycle;

// =========================================================
// L2 事件提取触发检查（路径 A + 路径 B）
// =========================================================

impl SessionLifecycle {
    /// 检查 L2 事件提取触发条件（路径 A：即时触发）。
    ///
    /// 对齐 Python `merger.check_and_merge` 的计数触发路径。
    /// 遍历所有 persona，检查未吸收 L1 是否 ≥ 阈值（默认 5 条）。
    pub(crate) async fn check_l2_trigger(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
    ) {
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
            if self.shutdown_flag.load(Ordering::Relaxed) {
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
            let trigger_count = self.config.thresholds.l2_trigger_count as usize;
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
                self.run_l2_extraction(storage, llm, &persona.uid, other_name)
                    .await;
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
        let unbound_stats = self
            .process_unbound_l1_for_l2(
                storage,
                llm,
                self.config.thresholds.l2_trigger_count as usize,
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

    /// 执行 L2 事件提取（通过 JobManager 包裹，带重试和可观测性）。
    ///
    /// 对齐 Python `merger.check_and_merge` 的 LLM 提取逻辑。
    ///
    /// 新增 `other_persona_name` 参数，用于双向对话场景的角色区分。
    /// 当已知对话另一方时，EventExtractor 会在 Prompt 中注入角色提示，
    /// 帮助 LLM 正确区分"用户"与"另一方"的行为归属。
    ///
    /// 重试策略:
    /// - LLM 调用失败 → 可重试（JobResult::Retryable），最多 3 次，指数退避。
    /// - 存储写入失败 → 同上可重试。
    /// - 成功但无事件 → 视为 Success（正常情况，非错误）。
    async fn run_l2_extraction(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: &str,
        other_persona_name: Option<String>,
    ) {
        let persona_owned = persona_uid.to_string();
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
                    cluster_delay_ms: self.config.thresholds.cluster_delay_ms,
                    temperature: self.config.event_extraction.temperature,
                    max_tokens: self.config.event_extraction.max_tokens,
                    max_events: self.config.event_extraction.max_events,
                    // 触发阈值与调度器保持一致：调度器按计数/时间触发后，
                    // 提取器内部 should_trigger 用同一阈值二次确认，避免自定义阈值下静默跳过。
                    trigger_count: self.config.thresholds.l2_trigger_count as i64,
                    trigger_days: self.config.thresholds.l2_trigger_days as i64,
                    // v1.5 L2 聚类去重指纹：从 [cache] 配置组传播
                    l2_fingerprint_enabled: self.config.cache.l2_fingerprint_enabled,
                    l2_similarity_threshold: self.config.cache.l2_similarity_threshold,
                    l2_recent_events_limit: self.config.cache.l2_recent_events_limit,
                    // 降级事件动态置信度开关：从 [event_extraction] 配置组传播
                    degrade: ramaria_memory::event::DegradeConfig {
                        dynamic_confidence_enabled: self
                            .config
                            .event_extraction
                            .degraded_confidence_enabled,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut extractor = EventExtractor::new(llm, storage, config);
                let uid = persona_owned.clone();
                async move {
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
                self.check_l3_trigger(storage, llm, &persona_owned).await;
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

impl SessionLifecycle {
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
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        trigger_count: usize,
        trigger_days: f64,
    ) -> UnboundL1ProcessStats {
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
        let mut session_owner: HashMap<Uuid, Option<String>> =
            HashMap::with_capacity(by_session.len());
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
            if self.shutdown_flag.load(Ordering::Relaxed) {
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
            self.run_l2_extraction(storage, llm, owner, other_name)
                .await;
        }

        stats
    }
}

// =========================================================
// L3 性格推断触发检查
// =========================================================

impl SessionLifecycle {
    /// 检查 L3 性格推断触发条件。
    ///
    /// 对齐 Python `profile_manager` + Phase A→B→C 管线。
    /// 触发条件：未吸收事件 ≥ 阈值（默认 10 条）或最早事件 > 阈值天数（默认 30 天）。
    pub(crate) async fn check_l3_trigger(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: &str,
    ) {
        let events = match storage.list_unabsorbed_events(persona_uid).await {
            Ok(e) => e,
            Err(e) => {
                warn!(persona_uid, %e, "L3 触发检查：查询未吸收事件失败");
                return;
            }
        };

        if events.is_empty() {
            return;
        }

        let now = now_ms();
        let oldest_event_age_days = events
            .iter()
            .map(|e| e.start)
            .min()
            .map(|min_time| (now - min_time) as f64 / (1000.0 * 86400.0))
            .unwrap_or(0.0);

        // L3 触发条件来自配置（对齐 Python ProfileConfig）
        let trigger_count = self.config.thresholds.l3_trigger_count as usize;
        let trigger_days = self.config.thresholds.l3_trigger_days as f64;

        let should_trigger = events.len() >= trigger_count || oldest_event_age_days >= trigger_days;

        if should_trigger {
            info!(
                persona_uid,
                event_count = events.len(),
                oldest_days = %format!("{:.1}", oldest_event_age_days),
                "L3 触发条件满足，启动性格推断"
            );
            self.run_l3_inference(storage, llm, persona_uid).await;
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
        }
    }

    /// 执行 L3 性格推断（Phase A 统计 → Phase B LLM 推断 → Phase C 置信度更新）。
    ///
    /// 对齐 Python `profile_manager.extract_profile` + Rust inference 管线。
    ///
    /// 可观测性:
    /// - 通过 JobManager 创建 `PersonalityInference` 任务记录，
    ///   记录开始/完成/failed 时间，便于运维排查"何时对谁做了推断"。
    ///
    /// - Phase A: 校准权重链 + 三轨准入 + 分层收缩 + 动机统计
    /// - Phase B: LLM 三步结构化推断（注入因果链特征 + 动机维度）
    /// - Phase C: 校准化置信度更新 + 四维度漂移检测
    async fn run_l3_inference(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: &str,
    ) {
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

        if job_id > 0 {
            let _ = job_manager.mark_running(job_id).await;
        }

        // ---- 统计特征提取（纯数值，不调 LLM） ----
        use ramaria_memory::inference::{StatsConfig, run_phase_a_stats};

        let stats_config = StatsConfig::default();
        let stats_summary = run_phase_a_stats(&events, &stats_config);

        info!(
            persona_uid = %persona_owned,
            event_count = events.len(),
            category_count = stats_summary.categories.len(),
            job_id,
            "L3 Phase A 统计完成"
        );

        // 将统计结果持久化到 cluster_snapshots 表
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
            "L3 Phase A 推断流程完成，开始 Phase B"
        );

        // ---- LLM 三步结构化推断 ----
        use ramaria_memory::inference::confidence::ConfidenceConfig;
        use ramaria_memory::inference::drift::DriftConfig;
        use ramaria_memory::inference::inferrer::InferrerConfig;
        use ramaria_memory::inference::run_phase_b_inference;

        let inferrer_config = InferrerConfig::from(self.config.inference.inferrer.clone());
        let phase_b_result = match run_phase_b_inference(
            llm,
            storage,
            &stats_summary,
            &persona_owned,
            &inferrer_config,
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
                if job_id > 0 {
                    let _ = job_manager
                        .mark_failed(job_id, &format!("Phase B 推断失败: {e}"))
                        .await;
                }
                return;
            }
        };

        // ---- 置信度更新 + 漂移检测 ----
        use ramaria_memory::inference::run_phase_c_update;

        // 判断是否为首轮推断
        let is_first_round =
            phase_b_result.traits_updated == 0 && phase_b_result.traits_deprecated == 0;

        let confidence_config = ConfidenceConfig::from(self.config.inference.confidence.clone());
        let mut drift_config = DriftConfig::from(self.config.inference.drift.clone());
        // 漂移检测是否从快照恢复真实旧分布（配置开关）
        drift_config.restore_real_distribution = self
            .config
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
                    warn!(persona_uid = %persona_owned, error = %e, "L3 推断：标记事件吸收失败");
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
}

// =========================================================
// 后台线程 B：L2/L3 定时触发
// =========================================================

impl SessionLifecycle {
    /// 启动后台 L2/L3 定时检查线程（Thread B）。
    ///
    /// 对齐 Python `SessionManager._l2_checker_loop`。
    ///
    /// 逻辑:
    /// - 每 `config.session.l2_check_interval_seconds`（默认 86400s = 24h）轮询
    /// - 遍历所有 persona：
    ///   - 最早未吸收 L1 > 7 天 → 触发 L2 事件提取
    ///   - 最早未吸收事件 > 30 天 → 触发 L3 性格推断
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider。
    pub fn spawn_l2_l3_scheduler(
        self: &Arc<Self>,
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
    ) -> tokio::task::JoinHandle<()> {
        let slf = Arc::clone(self);
        let interval_secs = self.config.session.l2_check_interval_seconds;
        let shutdown_flag = Arc::clone(&self.shutdown_flag);

        info!(interval_secs, "后台 L2/L3 定时检查线程启动（Thread B）");

        tokio::spawn(async move {
            // 首次延迟：启动 5 分钟后执行首次检查（避免阻塞启动流程）
            tokio::time::sleep(Duration::from_secs(300)).await;

            loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    info!("L2/L3 定时检查线程收到停止信号，退出");
                    return;
                }

                // 执行定时检查
                slf.run_scheduled_l2_l3_check(storage.as_ref(), llm.as_ref())
                    .await;

                // 等待下一次检查（可中断，每 60s 检查一次停止信号）
                let mut sleep_secs = interval_secs as u64;
                while sleep_secs > 0 && !shutdown_flag.load(Ordering::Relaxed) {
                    let chunk = sleep_secs.min(60);
                    tokio::time::sleep(Duration::from_secs(chunk)).await;
                    sleep_secs = sleep_secs.saturating_sub(chunk);
                }
            }
        })
    }

    /// 执行一次性 L2/L3 定时检查。
    ///
    /// 对齐 Python `merger.check_and_merge` 的时间触发路径（路径 B）。
    async fn run_scheduled_l2_l3_check(&self, storage: &dyn StorageBackend, llm: &dyn LlmProvider) {
        debug!("L2/L3 定时检查开始");

        let personas = match storage.list_personas().await {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "L2/L3 定时检查：无法列出 persona");
                return;
            }
        };

        let now = now_ms();
        let ms_per_day: i64 = 86_400_000;

        for persona in &personas {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return;
            }

            // ---- L2 时间触发 ----
            // 对齐 Python：最早未吸收 L1 > 7 天则触发
            match storage.list_unabsorbed_l1(&persona.uid).await {
                Ok(l1_list) => {
                    if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
                        let age_days = (now - oldest) as f64 / ms_per_day as f64;
                        if age_days >= 7.0 {
                            info!(
                                persona_uid = %persona.uid,
                                %age_days,
                                l1_count = l1_list.len(),
                                "L2 定时触发（路径 B：最早未吸收 L1 > 7 天）"
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
                            self.run_l2_extraction(storage, llm, &persona.uid, other_name)
                                .await;
                        }
                    }
                }
                Err(e) => {
                    warn!(persona_uid = %persona.uid, %e, "L2 定时检查：查询 L1 失败");
                }
            }

            // ---- L3 时间触发 ----
            // 对齐 Python profile 更新：最早未吸收事件 > 30 天则触发
            match storage.list_unabsorbed_events(&persona.uid).await {
                Ok(events) => {
                    if let Some(oldest) = events.iter().map(|e| e.start).min() {
                        let age_days = (now - oldest) as f64 / ms_per_day as f64;
                        if age_days >= 30.0 {
                            info!(
                                persona_uid = %persona.uid,
                                %age_days,
                                event_count = events.len(),
                                "L3 定时触发（路径 B：最早未吸收事件 > 30 天）"
                            );
                            self.run_l3_inference(storage, llm, &persona.uid).await;
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
        let unbound_stats = self
            .process_unbound_l1_for_l2(
                storage,
                llm,
                0,
                self.config.thresholds.l2_trigger_days as f64,
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
}

// =========================================================
// 单元测试：无主 L1 归属处理（数据断层修复链路）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockLlm, MockStorage};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{Persona, PersonaKind};
    use std::sync::Arc;

    /// 构造一条无主 L1（persona_uid=None、未吸收），归属于给定 session。
    fn make_unbound_l1(session_id: Uuid) -> MemoryL1 {
        MemoryL1::new(session_id, "导入会话摘要内容".into(), None)
    }

    /// 预置双 persona（char + user），用于 other_name 分支与归属解析。
    fn setup_personas(storage: &MockStorage) {
        storage.add_persona(Persona::new(
            "char-0001".into(),
            "角色".into(),
            PersonaKind::Char,
            1,
            "local".into(),
        ));
        storage.add_persona(Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            2,
            "local".into(),
        ));
    }

    /// 核心修复路径：无主 L1 ≥ 计数阈值 → 归属到来源 session 的 persona 并触发 L2。
    #[tokio::test]
    async fn unbound_l1_attributes_and_triggers_on_count() {
        let storage = Arc::new(MockStorage::new());
        setup_personas(&storage);

        // 归属 persona = char-0001 的 session
        let session = storage.create_session(Some("char-0001")).await.unwrap();

        // 5 条无主 L1（等于默认触发阈值 5）
        let l1s: Vec<MemoryL1> = (0..5)
            .map(|i| {
                let mut l1 = make_unbound_l1(session.id);
                l1.summary = format!("摘要 {i}");
                l1
            })
            .collect();
        storage.add_l1_summaries("", l1s);

        let lifecycle = SessionLifecycle::new(RamariaConfig::default());
        let llm = Arc::new(MockLlm::local());
        let stats = lifecycle
            .process_unbound_l1_for_l2(storage.as_ref(), llm.as_ref(), 5, 0.0)
            .await;

        // 触发判定
        assert_eq!(stats.total, 5, "应发现 5 条无主 L1");
        assert_eq!(stats.attributed, 5, "5 条全部可归属");
        assert_eq!(stats.unattributable, 0, "无无法归属项");
        assert_eq!(stats.triggered_personas, 1, "应触发 1 个 persona 的 L2");
        assert_eq!(stats.pending_groups, 0, "无待下次组");

        // 归属结果：无主通道清空，目标 persona 通道获得 5 条
        let unbound_left = storage.list_recent_l1_by_persona("", 100).await.unwrap();
        assert!(unbound_left.is_empty(), "无主 L1 应全部被归属");
        let bound = storage
            .list_recent_l1_by_persona("char-0001", 100)
            .await
            .unwrap();
        assert_eq!(bound.len(), 5, "char-0001 应获得 5 条 L1");
        assert!(
            bound
                .iter()
                .all(|l| l.persona_uid.as_deref() == Some("char-0001")),
            "归属后 persona_uid 应回填"
        );
    }

    /// 计数未达阈值：保持无主状态，不触发提取。
    #[tokio::test]
    async fn unbound_l1_pending_below_threshold() {
        let storage = Arc::new(MockStorage::new());
        setup_personas(&storage);

        let session = storage.create_session(Some("char-0001")).await.unwrap();
        let l1s: Vec<MemoryL1> = (0..3).map(|_| make_unbound_l1(session.id)).collect();
        storage.add_l1_summaries("", l1s);

        let lifecycle = SessionLifecycle::new(RamariaConfig::default());
        let llm = Arc::new(MockLlm::local());
        let stats = lifecycle
            .process_unbound_l1_for_l2(storage.as_ref(), llm.as_ref(), 5, 0.0)
            .await;

        assert_eq!(stats.total, 3);
        assert_eq!(stats.attributed, 3, "归属判定仍成立（可归属）");
        assert_eq!(stats.pending_groups, 1, "未达阈值 → 待下次检查");
        assert_eq!(stats.triggered_personas, 0, "不触发提取");

        // 无主通道仍保有全部 3 条（未被归属）
        let unbound_left = storage.list_recent_l1_by_persona("", 100).await.unwrap();
        assert_eq!(unbound_left.len(), 3, "低于阈值不应归属，保持无主");
        let bound = storage
            .list_recent_l1_by_persona("char-0001", 100)
            .await
            .unwrap();
        assert!(bound.is_empty(), "不应有归属");
    }

    /// 无法归属（来源 session 不存在）：计入统计、保持无主、不中断处理。
    #[tokio::test]
    async fn unbound_l1_unattributable_keeps_unbound() {
        let storage = Arc::new(MockStorage::new());
        setup_personas(&storage);

        // 不创建对应 session —— L1 来源 session 不存在
        let ghost_session = Uuid::new_v4();
        let l1s: Vec<MemoryL1> = (0..6).map(|_| make_unbound_l1(ghost_session)).collect();
        storage.add_l1_summaries("", l1s);

        let lifecycle = SessionLifecycle::new(RamariaConfig::default());
        let llm = Arc::new(MockLlm::local());
        let stats = lifecycle
            .process_unbound_l1_for_l2(storage.as_ref(), llm.as_ref(), 5, 0.0)
            .await;

        assert_eq!(stats.total, 6);
        assert_eq!(stats.attributed, 0, "无法归属");
        assert_eq!(stats.unattributable, 6, "全部计入无法归属");
        assert_eq!(stats.triggered_personas, 0, "无候选不触发");

        let unbound_left = storage.list_recent_l1_by_persona("", 100).await.unwrap();
        assert_eq!(
            unbound_left.len(),
            6,
            "无法归属的 L1 保持无主，等待后续处理"
        );
    }

    /// 时间触发（路径 B）：最早无主 L1 年龄 ≥ 阈值时触发，即使计数不足。
    #[tokio::test]
    async fn unbound_l1_age_trigger() {
        let storage = Arc::new(MockStorage::new());
        setup_personas(&storage);

        let session = storage.create_session(Some("char-0001")).await.unwrap();
        let now = now_ms();
        // 1 条 10 天前的无主 L1（时间触发阈值 7 天，计数阈值不启用）
        let mut l1 = make_unbound_l1(session.id);
        l1.created_at = now - 10 * 86_400_000;
        storage.add_l1_summaries("", vec![l1]);

        let lifecycle = SessionLifecycle::new(RamariaConfig::default());
        let llm = Arc::new(MockLlm::local());
        let stats = lifecycle
            .process_unbound_l1_for_l2(storage.as_ref(), llm.as_ref(), 0, 7.0)
            .await;

        assert_eq!(stats.total, 1);
        assert_eq!(stats.triggered_personas, 1, "年龄 ≥ 7 天应触发 L2");
        assert_eq!(stats.pending_groups, 0);

        let bound = storage
            .list_recent_l1_by_persona("char-0001", 100)
            .await
            .unwrap();
        assert_eq!(bound.len(), 1, "时间触发同样应归属 L1");
    }

    /// 安全默认：trigger_count 与 trigger_days 均为 0 → 不触发、不归属。
    #[tokio::test]
    async fn unbound_l1_no_trigger_with_zero_thresholds() {
        let storage = Arc::new(MockStorage::new());
        setup_personas(&storage);

        let session = storage.create_session(Some("char-0001")).await.unwrap();
        let l1s: Vec<MemoryL1> = (0..10).map(|_| make_unbound_l1(session.id)).collect();
        storage.add_l1_summaries("", l1s);

        let lifecycle = SessionLifecycle::new(RamariaConfig::default());
        let llm = Arc::new(MockLlm::local());
        let stats = lifecycle
            .process_unbound_l1_for_l2(storage.as_ref(), llm.as_ref(), 0, 0.0)
            .await;

        assert_eq!(stats.triggered_personas, 0, "双零阈值不应触发");
        assert_eq!(stats.pending_groups, 1, "应计入待下次组");
        let unbound_left = storage.list_recent_l1_by_persona("", 100).await.unwrap();
        assert_eq!(unbound_left.len(), 10, "不应发生任何归属");
    }
}
