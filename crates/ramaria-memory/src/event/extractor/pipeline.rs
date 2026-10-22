//! crates/ramaria-memory/src/event/extractor/pipeline.rs - L1→L2 事件提取主流程
//!
//! 设计特点:
//! - 触发门禁 → 未吸收 L1 语义聚类 → 逐簇 LLM 提取 → 解析与去重
//! - 失败降级为低置信混合事件，全流程收集后单事务写入
//! - 写入前自动生成 paraphrase，任一步失败整体回滚

use super::*;

impl<'a> EventExtractor<'a> {
    // =========================================================
    // 公共 API
    // =========================================================

    /// 为指定人格提取事件。
    ///
    /// 流程:
    /// 1. 检查触发条件
    /// 2. 如果不满足触发条件，静默返回空 Vec
    /// 3. 读取未吸收 L1 并通过 TopicBatcher 语义聚类
    /// 4. 对每个簇调用 LLM 提取事件（失败/解析失败时降级为低置信混合事件）
    /// 5. 解析 JSON → 构建 MemoryEvent 列表（含 paraphrase 生成与相似度去重）
    /// 6. 收集事件 + 来源链接 + 事件关系 + 待吸收 L1
    /// 7. 单事务写入批次（事件 + 来源 + 关系 + L1 吸收标记）
    ///
    /// 注意:
    /// - `event_relations` 映射见 `map_cluster_relations`（6 种关系类型）。
    ///   映射条件：LLM 返回 relations 且本簇已保存事件数 ≥ 2；
    ///   索引越界/端点被相似度去重跳过/自引用均丢弃，不错误连边。
    /// - 批次写入为全事务语义（`StorageBackend::save_event_batch`）：
    ///   任一环节失败整体回滚并上抛，上层重试不会产生半批数据。
    ///
    /// 参数:
    /// - `persona_uid`: 分析对象的人格标识。传空字符串表示分析默认用户。
    ///
    /// 返回:
    /// - 成功时返回提取的事件列表（可能为空；事件 id 已回填）。
    /// - LLM 调用失败时返回错误（上层应重试或降级）；
    ///   批次写入失败时整体回滚并返回错误。
    pub async fn extract_events(&mut self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        // 1. 检查触发条件
        if !self.should_trigger(persona_uid).await? {
            debug!(%persona_uid, "未满足事件提取触发条件，跳过");
            return Ok(vec![]);
        }

        // 2. 读取未吸收 L1
        let l1_list = self
            .storage
            .list_unabsorbed_l1(persona_uid)
            .await
            .map_err(|e| {
                warn!(%persona_uid, error=%e, "读取未吸收 L1 失败");
                RamariaError::storage(format!("读取 {persona_uid} 未吸收 L1 失败: {e}"))
            })?;

        if l1_list.is_empty() {
            debug!(%persona_uid, "无未吸收 L1");
            return Ok(vec![]);
        }

        // 2.5 L2 聚类去重指纹检查（v1.5 三层生成缓存 C）
        //
        // 语义: 若同一 L1 集合此前已被聚类且无任何事件产出（已登记指纹），
        // 则本次直接跳过——重跑/重试/失败恢复场景不重复聚类、不重复花费 API 账单。
        // 集合一旦变化（新增/移除 L1），指纹必然变化，自动触发重新聚类。
        //
        // 降级: 指纹查询失败仅记 warn 并继续正常聚类（不阻塞主流程）。
        let fingerprint = compute_l1_set_fingerprint(&l1_list);
        let fingerprint_enabled = self.config.l2_fingerprint_enabled;
        if fingerprint_enabled {
            match self
                .storage
                .l2_fingerprint_exists(persona_uid, &fingerprint)
                .await
            {
                Ok(true) => {
                    info!(
                        %persona_uid,
                        fingerprint = %fingerprint,
                        l1_count = l1_list.len(),
                        "L2 集合指纹命中：同集合已聚类且无产出，跳过本次事件提取（不重复聚类）"
                    );
                    return Ok(vec![]);
                }
                Ok(false) => {
                    debug!(
                        %persona_uid,
                        fingerprint = %fingerprint,
                        l1_count = l1_list.len(),
                        "L2 集合指纹未命中，正常聚类"
                    );
                }
                Err(e) => {
                    warn!(
                        %persona_uid,
                        fingerprint = %fingerprint,
                        error = %e,
                        "L2 集合指纹查询失败，降级正常聚类（不阻塞）"
                    );
                }
            }
        }

        // 3. 转换为 L1Item 并通过 TopicBatcher 语义聚类
        let l1_items: Vec<L1Item> = l1_list.iter().map(L1Item::from).collect();
        let now = now_ms();
        let (clusters, _expired) = self.batcher.build_clusters(l1_items, now);

        if clusters.is_empty() {
            debug!(%persona_uid, "TopicBatcher 未产出簇，跳过事件提取");
            // 无产出也登记指纹：下次同集合直接跳过（不重复聚类）
            self.record_fingerprint_if_no_output(persona_uid, &fingerprint, 0)
                .await;
            return Ok(vec![]);
        }

        // 查询 persona 显示名称，用于 Prompt 中替换"用户"
        let persona_name = self
            .storage
            .get_persona_by_uid(persona_uid)
            .await
            .map(|p| p.map(|p| p.name).unwrap_or_else(|| persona_uid.to_string()))
            .unwrap_or_else(|e| {
                warn!(%persona_uid, error = %e, "查询 persona 名称失败，回退到 uid");
                persona_uid.to_string()
            });

        info!(
            %persona_uid,
            total_l1 = l1_list.len(),
            cluster_count = clusters.len(),
            "TopicBatcher 聚类完成"
        );

        // 4. 对每个簇独立调用 LLM 提取事件。
        // 循环内只累积，不写库；循环结束后以单事务写入批次（失败整体回滚）。
        // 下标约定: sources / relations 的下标均指向 batch.events 位置。
        let mut batch = EventBatchWrite::default();
        let mut all_l1_ids: Vec<Uuid> = Vec::new();

        // 4.0 相似度去重事件池（v1.5）：取 persona 最近 N 条已有事件，
        // 供新提取事件做近似重复比对（重跑场景不产生重复事件）。
        // 降级: 查询失败记 warn 后置空池（跳过相似度去重，不阻塞提取）。
        let dedup_pool = if fingerprint_enabled {
            match self
                .storage
                .list_recent_events(persona_uid, self.config.l2_recent_events_limit)
                .await
            {
                Ok(pool) => pool,
                Err(e) => {
                    warn!(
                        %persona_uid,
                        error = %e,
                        "相似度去重：查询最近事件失败，本次跳过相似度去重（不阻塞）"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let mut dedup_skipped = 0usize;
        let mut truncated_events = 0usize;

        for (ci, cluster) in clusters.iter().enumerate() {
            let cluster_l1_ids: Vec<Uuid> = cluster.l1_items.iter().map(|i| i.id).collect();
            let cluster_size = cluster.l1_items.len();

            // 找到对应的原始 MemoryL1（用于降级和 event_sources）
            let cluster_l1: Vec<&MemoryL1> = cluster_l1_ids
                .iter()
                .filter_map(|id| l1_list.iter().find(|l| l.id == *id))
                .collect();

            // 格式化簇内 L1
            let formatted = Self::format_l1_from_cluster(cluster);

            // CompositeIndex 补充上下文检索
            let context_docs = if let Some(retriever) = self.retriever {
                let ctx_retriever =
                    ContextRetriever::new(retriever, self.config.context_retriever.clone());
                ctx_retriever.retrieve_context(cluster, persona_uid)
            } else {
                Vec::new()
            };

            // 构建 Prompt（带或不带补充上下文）
            // 使用 persona 实际名称替代"用户"
            // 注入对话另一方角色提示
            let other_name = self.config.other_persona_name.as_deref();
            let prompt = if context_docs.is_empty() {
                build_event_extraction_prompt_for_persona(&formatted, &persona_name, other_name)
            } else {
                debug!(
                    %persona_uid,
                    cluster_idx = ci,
                    context_doc_count = context_docs.len(),
                    "注入 CompositeIndex 补充上下文"
                );
                build_event_extraction_prompt_with_context_for_persona(
                    &formatted,
                    &context_docs,
                    &persona_name,
                    other_name,
                )
            };

            // 调用 LLM
            let request_id = Uuid::new_v4();
            let llm_request = ChatRequest {
                system_prompt: String::new(),
                memory_context: None,
                history: vec![],
                user_message: prompt,
                temperature: self.config.temperature,
                max_tokens: self.config.max_tokens,
                request_id,
                template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
                enable_thinking: None,
            };

            let raw_response = match self.llm.chat(&llm_request).await {
                Ok(text) => text,
                Err(e) => {
                    warn!(%persona_uid, %request_id, cluster_idx = ci, error=%e,
                        "簇 {} LLM 调用失败，触发降级", ci);
                    let (degraded_event, degraded_sources) =
                        self.degrade_cluster(persona_uid, &cluster_l1);
                    let batch_index = batch.events.len();
                    batch.events.push(degraded_event);
                    for (l1_id, weight) in degraded_sources {
                        batch.sources.push((batch_index, l1_id, weight));
                    }
                    all_l1_ids.extend(cluster_l1_ids);
                    continue;
                }
            };

            // 隐私红线：LLM 原始响应不落日志，仅记录字符长度供诊断
            debug!(%persona_uid, %request_id, cluster_idx = ci,
                len = raw_response.chars().count(),
                "LLM 返回 {} 字符（原始响应不记录）", raw_response.chars().count());

            // 解析 JSON
            let parsed = match Self::parse_event_response(&raw_response) {
                Ok(result) if !result.events.is_empty() => result,
                Ok(_) | Err(_) => {
                    warn!(%persona_uid, cluster_idx = ci, "簇 {} JSON 解析/空结果，触发降级", ci);
                    let (degraded_event, degraded_sources) =
                        self.degrade_cluster(persona_uid, &cluster_l1);
                    let batch_index = batch.events.len();
                    batch.events.push(degraded_event);
                    for (l1_id, weight) in degraded_sources {
                        batch.sources.push((batch_index, l1_id, weight));
                    }
                    all_l1_ids.extend(cluster_l1_ids);
                    continue;
                }
            };

            // 截断到 max_events（每簇）。
            // 说明: LLM 输出超过上限时后段事件被丢弃，属已知压缩上限
            // （prompt 与默认配置同以 5 为上限，通常仅在不遵守或配置调低时触发）。
            // 记录丢弃数量供"事件漏抽"诊断。
            let parsed_events = parsed.events;
            let truncated_count = parsed_events.len().saturating_sub(self.config.max_events);
            truncated_events += truncated_count;
            let extracted: Vec<ExtractedEventJson> = parsed_events
                .into_iter()
                .take(self.config.max_events)
                .collect();
            let relations = parsed.relations;

            // 时间范围（按发生时间；L1Item.occurred_at 已是有效值）
            let time_range = (
                cluster
                    .l1_items
                    .first()
                    .map(|i| i.occurred_at)
                    .unwrap_or(now),
                cluster
                    .l1_items
                    .last()
                    .map(|i| i.occurred_at)
                    .unwrap_or(now),
            );

            // 情境强度
            let avg_situation: Option<i32> = {
                let values: Vec<i32> = cluster_l1
                    .iter()
                    .filter_map(|l1| l1.situation_strength)
                    .collect();
                if values.is_empty() {
                    None
                } else {
                    Some(values.iter().sum::<i32>() / values.len() as i32)
                }
            };

            // 构建 MemoryEvent 并累积到批次。
            // batch_index_by_position 与"提取后事件数组"等长、逐位置记录事件在
            // batch.events 中的下标：相似度去重跳过的位置保持 None，
            // 供 relations 按 LLM 输出位置正确映射（见 `map_cluster_relations`，
            // 避免压缩后的批次下标把关系连到错误事件）。
            let mut batch_index_by_position: Vec<Option<usize>> = vec![None; extracted.len()];
            for (position, ej) in extracted.into_iter().enumerate() {
                let mut event = Self::build_event(
                    persona_uid,
                    ej,
                    time_range.0,
                    time_range.1,
                    now,
                    avg_situation,
                );

                // v1.5 相似度去重：与 persona 最近已有事件比对，
                // 近似重复（相似度 ≥ 阈值）的事件不保存（不重复入库）。
                if !dedup_pool.is_empty()
                    && dedup_pool.iter().any(|existing| {
                        event_text_similarity(&event, existing)
                            >= self.config.l2_similarity_threshold
                    })
                {
                    dedup_skipped += 1;
                    debug!(
                        %persona_uid,
                        title = %event.title,
                        threshold = self.config.l2_similarity_threshold,
                        "L2 相似度去重：与已有事件近似重复，跳过保存"
                    );
                    continue;
                }

                // paraphrase
                if let Some(ref attitude) = event.attitude
                    && !attitude.trim().is_empty()
                {
                    let context = format!("{} {}", event.title, event.summary);
                    let paraphrase =
                        generate_paraphrase(self.llm, attitude, &context, &self.config.paraphrase)
                            .await;
                    event.paraphrase = paraphrase;
                }

                // 累积事件与来源（写入由循环结束后的单事务完成）
                let batch_index = batch.events.len();
                batch.events.push(event);
                batch_index_by_position[position] = Some(batch_index);

                // event_sources：簇内每条 L1 对该事件等权
                for l1 in &cluster_l1 {
                    let weight = 1.0 / cluster_size as f64;
                    batch.sources.push((batch_index, l1.id, weight));
                }
            }

            // 收集事件关系（按 LLM 输出位置映射到批次下标；
            // 端点被相似度去重跳过的关系丢弃，不错误连边）。
            let batch_position_count = batch_index_by_position.iter().flatten().count();
            if let Some(ref rels) = relations
                && !rels.is_empty()
                && batch_position_count >= 2
            {
                let mapped =
                    Self::map_cluster_relations(rels, &batch_index_by_position, persona_uid, ci);
                let mapped_count = mapped.len();
                batch.relations.extend(mapped);
                debug!(
                    %persona_uid,
                    cluster_idx = ci,
                    mapped_relation_count = mapped_count,
                    "事件关系映射完成（随批次统一写入）"
                );
            }

            all_l1_ids.extend(cluster_l1_ids);
            debug!(%persona_uid, cluster_idx = ci, cluster_size, "簇 {} 处理完成", ci);

            // 请求间节流（v1.4 抽象，L1/L2 共用）：避免触发远程 API 速率限制。
            // 实现见 `crate::llm_gate::inter_llm_delay`（delay=0 时跳过）。
            crate::llm_gate::inter_llm_delay(
                self.config.cluster_delay_ms,
                &format!("L2 簇间 cluster={ci}"),
            )
            .await;
        }

        // 5. 单事务写入批次（事件 + 来源 + 关系 + L1 吸收标记）。
        // 任一环节失败整体回滚并上抛：上层重试不会留下半批数据。
        let absorbed_l1_count = all_l1_ids.len();
        batch.absorbed_l1_ids = all_l1_ids;

        let event_ids = self.storage.save_event_batch(&batch).await.map_err(|e| {
            error!(%persona_uid, error = %e, "事件批次写入失败（整体回滚）");
            RamariaError::storage(format!("写入事件批次失败: {e}"))
        })?;

        info!(
            %persona_uid,
            event_count = batch.events.len(),
            source_count = batch.sources.len(),
            relation_count = batch.relations.len(),
            absorbed_l1_count,
            "事件批次写入完成（单事务）"
        );

        // 用返回 id 回填事件（顺序与 batch.events 一致）
        let mut all_events = batch.events;
        debug_assert_eq!(
            all_events.len(),
            event_ids.len(),
            "批次返回 id 数应与事件数一致"
        );
        for (event, id) in all_events.iter_mut().zip(event_ids.iter()) {
            event.id = *id;
        }

        // 5.5 无产出登记指纹（v1.5）：
        // 全部簇均未产出事件时登记 L1 集合指纹，下次同集合直接跳过。
        self.record_fingerprint_if_no_output(persona_uid, &fingerprint, all_events.len())
            .await;

        info!(
            %persona_uid,
            event_count = all_events.len(),
            absorbed_l1 = absorbed_l1_count,
            dedup_skipped,
            truncated_events,
            "事件提取完成"
        );

        Ok(all_events)
    }
}
