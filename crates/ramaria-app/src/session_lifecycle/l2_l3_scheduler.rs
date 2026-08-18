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

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::now_ms;
use ramaria_memory::event::{EventExtractor, EventExtractorConfig};
use ramaria_memory::job::{JobManager, JobResult, JobType};
use tracing::{debug, error, info, warn};

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

        debug!("L2/L3 定时检查完成");
    }
}
