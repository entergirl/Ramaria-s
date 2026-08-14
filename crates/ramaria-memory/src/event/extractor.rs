//! rust/crates/ramaria-memory/src/event/extractor.rs - L1→L2 事件提取管线
//!
//! 设计特点:
//! - 依赖注入: 通过 `&dyn LlmProvider` + `&dyn StorageBackend` 解耦具体实现
//! - 使用 `TopicBatcher` 语义聚类替代旧 `chat_partners + take(20)` 分批策略
//! - 通过可选的 `Retriever` 引用启用 CompositeIndex 补充上下文检索
//! - Prompt 新增 motives（底层动机）+ relations（事件关系）输出
//! - 激活 motives 字段写入 + event_relations 表写入
//! - 触发条件: 未吸收 L1 ≥ 5 条 或 最早未吸收 L1 ≥ 7 天
//! - TopicBatcher 将未吸收 L1 聚类为 TopicCluster，每簇独立调用 LLM 提取事件
//! - 降级兜底: JSON 解析失败 → 退化为 confidence=0.5 混合事件
//! - 事件写入后自动生成 paraphrase（attitude 存在且非空时）
//! - 成功后批量标记 L1 为 absorbed + 写入 event_sources + event_relations
//! - 所有可恢复错误转换为 RamariaError，保留上下文

use ramaria_core::traits::ChatRequest;
use ramaria_core::types::{EventRelationKind, Presentation, now_ms};
use ramaria_core::{
    EventRelation, LlmProviderTrait, MemoryEvent, MemoryL1, RamariaError, RamariaResult,
    StorageBackend,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::batcher::{L1Item, TopicBatcher, TopicBatcherConfig, TopicCluster};
use super::context_retriever::{ContextRetriever, ContextRetrieverConfig};
use super::degrade::{DegradeConfig, build_degraded_event};
use super::paraphrase::{ParaphraseConfig, generate_paraphrase};
use super::prompt::{
    build_event_extraction_prompt_for_persona,
    build_event_extraction_prompt_with_context_for_persona,
};
use crate::retriever::Retriever;
use crate::utils;

/// 一天的毫秒数常量。
const MS_PER_DAY: f64 = utils::MS_PER_DAY;

// =========================================================
// LLM 响应 JSON 结构
// =========================================================

/// LLM 返回的单条事件 JSON。
///
/// 说明:
/// - 所有字段为 `Option`，容忍 LLM 输出缺失字段。
/// - 校验阶段填充默认值。
/// - 新增 `motives` 字段（底层动机标签列表）。
#[derive(Debug, Deserialize)]
struct ExtractedEventJson {
    title: Option<String>,
    summary: Option<String>,
    keywords: Option<String>,
    participants: Option<serde_json::Value>, // JSON 数组或 null
    confidence: Option<f64>,
    salience: Option<f64>,
    valence: Option<f64>,
    presentation: Option<String>,
    share: Option<f64>,
    attitude: Option<String>,
    /// 底层动机标签列表，如 ["地位维护", "自主性"]
    #[serde(default)]
    motives: Option<Vec<String>>,
}

/// LLM 返回的事件关系。
///
/// 字段约定:
/// - `from_index` / `to_index`: 引用 events 数组中的事件索引（从 0 开始）。
/// - `kind`: 六种关系类型之一。
/// - `weight`: 关系确信度 0.0..1.0。
#[derive(Debug, Deserialize)]
struct EventRelationOutput {
    from_index: usize,
    to_index: usize,
    kind: String,
    #[serde(default = "default_relation_weight")]
    weight: f64,
    /// 关系逻辑的简要说明（由 LLM 输出，当前仅用于 Prompt 引导，未持久化存储）
    #[serde(default)]
    #[allow(dead_code)]
    detail: Option<String>,
}

fn default_relation_weight() -> f64 {
    0.5
}

/// LLM 返回的完整提取结果（events + relations）。
///
/// 字段约定:
/// - `events` 为必填字段（无 `#[serde(default)]`），用于区分新格式与旧格式单事件对象。
///   若 JSON 对象不含 `"events"` 键，serde 按缺少必填字段报错，降级到 `Array`/`Single` 变体。
/// - `relations` 为可选字段（`#[serde(default)]`），缺失时等价于 `None`。
#[derive(Debug, Deserialize)]
struct EventExtractionResponse {
    events: Vec<ExtractedEventJson>,
    #[serde(default)]
    relations: Option<Vec<EventRelationOutput>>,
}

/// LLM 返回的顶层结构：支持新旧两种格式。
///
/// 解析策略:
/// 1. 尝试解析为 `EventExtractionResponse`（新格式: {"events": [...], "relations": [...]}）
/// 2. 尝试解析为 `Vec<ExtractedEventJson>` 数组（旧格式: [...]）
/// 3. 尝试解析为单对象 `ExtractedEventJson`，包装为单元素数组
/// 4. 失败 → 触发降级
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EventResponse {
    Object(EventExtractionResponse),
    Array(Vec<ExtractedEventJson>),
    // 兼容 LLM 偶尔返回单对象而非数组
    Single(serde_json::Value),
}

/// 解析后的完整结果：事件列表 + 可选的关系列表。
///
/// 职责:
/// - 将 `EventResponse` 统一为此结构，供 `extract_events` 后续处理。
#[derive(Debug)]
struct ParsedExtractionResult {
    events: Vec<ExtractedEventJson>,
    relations: Option<Vec<EventRelationOutput>>,
}

// =========================================================
// Event Extractor 配置
// =========================================================

/// 事件提取器配置。
#[derive(Debug, Clone)]
pub struct EventExtractorConfig {
    /// LLM 生成温度
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
    /// 触发条件 A: 未吸收 L1 ≥ 此数量时触发提取
    pub trigger_count: i64,
    /// 触发条件 B: 最早未吸收 L1 距今 ≥ 此天数时触发提取
    pub trigger_days: i64,
    /// 单次提取最多取多少条 L1
    pub max_l1_per_batch: usize,
    /// 事件输出截断: 最多提取多少条事件
    pub max_events: usize,
    /// 降级事件配置
    pub degrade: DegradeConfig,
    /// Paraphrase 配置
    pub paraphrase: ParaphraseConfig,
    /// CompositeIndex 补充上下文检索配置
    pub context_retriever: ContextRetrieverConfig,
    /// 对话另一方的名称（用于双向对话场景的角色区分）。
    /// `None` 表示未知或单方对话场景。
    pub other_persona_name: Option<String>,
    /// 簇间 LLM 请求间隔（毫秒），用于避免触发远程 API 速率限制。
    /// 默认 0（不等待），建议对 DeepSeek 等有速率限制的 API 设为 500~1000。
    pub cluster_delay_ms: u64,
    /// L2 聚类去重指纹开关（v1.5 三层生成缓存 C，T-V15-3-003）。
    ///
    /// `true` 时:
    /// - 同一 L1 集合（已聚类且无产出）通过指纹直接跳过，不重复聚类；
    /// - 新提取事件与 persona 最近已有事件做相似度去重（近似重复不保存）。
    ///
    /// `false` 时: 事件提取行为回退 v1.4（不做集合跳过/相似度去重）。
    ///
    /// 来源: `[cache].l2_fingerprint_enabled`（默认开启）。
    pub l2_fingerprint_enabled: bool,
    /// 新提取事件与已有事件的相似度去重判定阈值（0.0..=1.0）。
    /// 相似度 ≥ 此值时判为近似重复、跳过保存。来源: `[cache].l2_similarity_threshold`。
    pub l2_similarity_threshold: f64,
    /// 相似度去重比对的最远事件条数（取 persona 最近 N 条，按时间倒序）。
    /// 来源: `[cache].l2_recent_events_limit`。
    pub l2_recent_events_limit: u32,
}

impl Default for EventExtractorConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 8192,
            trigger_count: 5,
            trigger_days: 7,
            max_l1_per_batch: 20,
            max_events: 5,
            degrade: DegradeConfig::default(),
            paraphrase: ParaphraseConfig::default(),
            context_retriever: ContextRetrieverConfig::default(),
            other_persona_name: None,
            cluster_delay_ms: 0,
            l2_fingerprint_enabled: true,
            l2_similarity_threshold: 0.95,
            l2_recent_events_limit: 200,
        }
    }
}

// =========================================================
// Event Extractor
// =========================================================

/// L1→L2 事件提取器。
///
/// 职责:
/// - 检查触发条件，从存储读取未吸收 L1。
/// - 调用 LLM 提取结构化事件。
/// - 处理降级、paraphrase 生成、写回存储。
///
///
/// - 可选的 `Retriever` 引用启用 CompositeIndex 补充上下文检索。
///   设置后，每个 TopicCluster 在 LLM 调用前自动检索历史相关 L1/L2。
///
/// 用法:
/// ```ignore
/// let mut extractor = EventExtractor::new(&llm, &storage, config);
/// extractor.set_retriever(&retriever);  // 启用上下文检索
/// let events = extractor.extract_events("user-0001").await?;
/// ```
pub struct EventExtractor<'a> {
    config: EventExtractorConfig,
    llm: &'a dyn LlmProviderTrait,
    storage: &'a dyn StorageBackend,
    /// 主题批量构建器，持有跨批次 Pending Buffer 状态
    batcher: TopicBatcher,
    /// 可选的三通道检索器引用，用于 CompositeIndex 补充上下文
    retriever: Option<&'a Retriever>,
}

impl<'a> EventExtractor<'a> {
    /// 创建新的事件提取器。
    ///
    /// 自动创建 TopicBatcher，配置从 EventExtractorConfig 派生。
    pub fn new(
        llm: &'a dyn LlmProviderTrait,
        storage: &'a dyn StorageBackend,
        config: EventExtractorConfig,
    ) -> Self {
        let batcher_config =
            TopicBatcherConfig::new().with_max_cluster_size(config.max_l1_per_batch);
        Self {
            config,
            llm,
            storage,
            batcher: TopicBatcher::new(batcher_config),
            retriever: None,
        }
    }

    /// 设置 Retriever 引用，启用 CompositeIndex 补充上下文检索。
    ///
    /// 说明:
    /// - 不设置时（默认），事件提取无历史上下文注入。
    /// - 设置后，每个 TopicCluster 在 LLM 调用前自动检索相关历史 L1/L2
    ///   并注入 Prompt 的"补充背景"段落。
    pub fn set_retriever(&mut self, retriever: &'a Retriever) {
        self.retriever = Some(retriever);
    }

    // =========================================================
    // 公共 API
    // =========================================================

    /// 为指定人格提取事件。
    ///
    /// 流程:
    /// 1. 检查触发条件
    /// 2. 如果不满足触发条件，静默返回空 Vec
    /// 3. 读取未吸收 L1，按 chat_partners 分组
    /// 4. 截断到 batch 上限，格式化 L1 摘要列表
    /// 5. 调用 LLM 提取事件
    /// 6. 解析 JSON → 构建 MemoryEvent 列表
    /// 7. 对每个事件: 生成 paraphrase（如果有 attitude）
    /// 8. 写入事件 + event_sources
    /// 9. 标记 L1 为 absorbed
    ///
    /// 注意:
    /// - `event_relations` 写入已实现（v1.3，6 种关系类型提取，见 `save_cluster_relations`）。
    ///   写入条件：LLM 返回 relations 且本簇事件数 ≥ 2；索引越界/自引用/写失败均降级 warn 不阻塞。
    ///
    /// 参数:
    /// - `persona_uid`: 分析对象的人格标识。传空字符串表示分析默认用户。
    ///
    /// 返回:
    /// - 成功时返回提取的事件列表（可能为空）。
    /// - LLM 调用失败时返回错误（上层应重试或降级）。
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

        // 2.5 L2 聚类去重指纹检查（v1.5 三层生成缓存 C，T-V15-3-003）
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

        // 4. 对每个簇独立调用 LLM 提取事件
        let mut all_events: Vec<MemoryEvent> = Vec::new();
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
            };

            let raw_response = match self.llm.chat(&llm_request).await {
                Ok(text) => text,
                Err(e) => {
                    warn!(%persona_uid, %request_id, cluster_idx = ci, error=%e,
                        "簇 {} LLM 调用失败，触发降级", ci);
                    let events = self.degrade_cluster(persona_uid, &cluster_l1).await?;
                    all_events.extend(events);
                    all_l1_ids.extend(cluster_l1_ids);
                    continue;
                }
            };

            // 记录原始响应用于诊断（截断到 ~2000 字节，避免日志膨胀）。
            // 必须停在 UTF-8 字符边界上，否则直接切片会 panic。
            let preview = if raw_response.len() > 2000 {
                let mut end = 2000;
                while !raw_response.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...<truncated>", &raw_response[..end])
            } else {
                raw_response.clone()
            };
            debug!(%persona_uid, %request_id, cluster_idx = ci,
                len = raw_response.len(),
                raw = %preview,
                "LLM 返回 {} 字符", raw_response.len());

            // 解析 JSON
            let parsed = match Self::parse_event_response(&raw_response) {
                Ok(result) if !result.events.is_empty() => result,
                Ok(_) | Err(_) => {
                    warn!(%persona_uid, cluster_idx = ci, "簇 {} JSON 解析/空结果，触发降级", ci);
                    let events = self.degrade_cluster(persona_uid, &cluster_l1).await?;
                    all_events.extend(events);
                    all_l1_ids.extend(cluster_l1_ids);
                    continue;
                }
            };

            // 截断到 max_events（每簇）
            let extracted: Vec<ExtractedEventJson> = parsed
                .events
                .into_iter()
                .take(self.config.max_events)
                .collect();
            let relations = parsed.relations;

            // 时间范围
            let time_range = (
                cluster
                    .l1_items
                    .first()
                    .map(|i| i.created_at)
                    .unwrap_or(now),
                cluster.l1_items.last().map(|i| i.created_at).unwrap_or(now),
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

            // 构建 MemoryEvent 并保存（记录 index→event_id 映射供 relations 使用）
            let mut cluster_event_ids: Vec<i64> = Vec::with_capacity(extracted.len());
            for ej in extracted {
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

                let event_id = self.storage.save_event(&event).await.map_err(|e| {
                    warn!(%persona_uid, error=%e, "写入 memory_event 失败");
                    RamariaError::storage(format!("写入事件失败: {e}"))
                })?;
                event.id = event_id;
                cluster_event_ids.push(event_id);
                all_events.push(event.clone());

                // event_sources
                for l1 in &cluster_l1 {
                    let weight = 1.0 / cluster_size as f64;
                    if let Err(e) = self
                        .storage
                        .save_event_source(event_id, l1.id, weight)
                        .await
                    {
                        warn!(%event_id, l1_id = %l1.id, error=%e,
                            "写入 event_source 失败（非致命）");
                    }
                }
            }

            // 写入事件关系
            if let Some(ref rels) = relations
                && !rels.is_empty()
                && cluster_event_ids.len() >= 2
            {
                let saved_count = self
                    .save_cluster_relations(rels, &cluster_event_ids, persona_uid, ci)
                    .await;
                debug!(
                    %persona_uid,
                    cluster_idx = ci,
                    saved_relation_count = saved_count,
                    "事件关系写入完成"
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

        // 5. 批量标记 L1 为 absorbed
        if !all_l1_ids.is_empty()
            && let Err(e) = self.storage.mark_l1_absorbed(&all_l1_ids).await
        {
            warn!(%persona_uid, error=%e, "标记 L1 absorbed 失败（非致命）");
        }

        // 5.5 无产出登记指纹（v1.5）：
        // 全部簇均未产出事件时登记 L1 集合指纹，下次同集合直接跳过。
        self.record_fingerprint_if_no_output(persona_uid, &fingerprint, all_events.len())
            .await;

        info!(
            %persona_uid,
            event_count = all_events.len(),
            absorbed_l1 = all_l1_ids.len(),
            dedup_skipped,
            "事件提取完成"
        );

        Ok(all_events)
    }

    /// 记录"已聚类且无产出"的 L1 集合指纹（v1.5 L2 聚类去重指纹）。
    ///
    /// 语义:
    /// - 仅当指纹开关开启且 `event_count == 0`（无事件产出）时登记；
    ///   有产出时 L1 会被标记 absorbed，下次集合变化指纹自然失效，无需登记。
    /// - 登记失败仅记 warn（降级：下次会重复聚类，不阻塞主流程）。
    async fn record_fingerprint_if_no_output(
        &self,
        persona_uid: &str,
        fingerprint: &str,
        event_count: usize,
    ) {
        if !self.config.l2_fingerprint_enabled || event_count > 0 {
            return;
        }
        match self
            .storage
            .save_l2_fingerprint(persona_uid, fingerprint)
            .await
        {
            Ok(_) => {
                info!(
                    %persona_uid,
                    fingerprint = %fingerprint,
                    "L2 无产出：已登记 L1 集合指纹（同集合下次直接跳过）"
                );
            }
            Err(e) => {
                warn!(
                    %persona_uid,
                    fingerprint = %fingerprint,
                    error = %e,
                    "L2 指纹登记失败（下次将重复聚类，不阻塞）"
                );
            }
        }
    }

    /// 对单个簇执行降级处理。
    async fn degrade_cluster(
        &self,
        persona_uid: &str,
        l1_batch: &[&MemoryL1],
    ) -> RamariaResult<Vec<MemoryEvent>> {
        let l1_owned: Vec<MemoryL1> = l1_batch.iter().map(|l| (*l).clone()).collect();
        let event = build_degraded_event(persona_uid, &l1_owned, &self.config.degrade);

        let event_id = self
            .storage
            .save_event(&event)
            .await
            .map_err(|e| RamariaError::storage(format!("写入降级事件失败: {e}")))?;

        let mut saved_event = event;
        saved_event.id = event_id;

        for l1 in l1_batch {
            let weight = 1.0 / l1_batch.len() as f64;
            if let Err(e) = self
                .storage
                .save_event_source(event_id, l1.id, weight)
                .await
            {
                warn!(%event_id, l1_id = %l1.id, error=%e, "降级: event_source 写入失败（非致命）");
            }
        }

        Ok(vec![saved_event])
    }

    // =========================================================
    // 内部方法
    // =========================================================

    /// 检查是否满足事件提取触发条件。
    ///
    /// 条件 A: 未吸收 L1 ≥ trigger_count
    /// 条件 B: 最早未吸收 L1 距今 ≥ trigger_days 天
    ///
    /// 满足任一条件即触发。
    async fn should_trigger(&self, persona_uid: &str) -> RamariaResult<bool> {
        let l1_list = match self.storage.list_unabsorbed_l1(persona_uid).await {
            Ok(list) => list,
            Err(e) => {
                warn!(%persona_uid, error=%e, "检查触发条件时读取 L1 失败");
                return Err(RamariaError::storage(format!(
                    "检查 {persona_uid} 触发条件失败: {e}"
                )));
            }
        };

        // 条件 A: 数量
        if l1_list.len() >= self.config.trigger_count as usize {
            debug!(
                %persona_uid,
                count = l1_list.len(),
                threshold = self.config.trigger_count,
                "满足触发条件 A: 数量 ≥ {}",
                self.config.trigger_count
            );
            return Ok(true);
        }

        // 条件 B: 时间
        if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
            let now = now_ms();
            let age_ms = now.saturating_sub(oldest);
            // 使用 f64 避免整数除法截断: 23h59m 不应算作 1 天
            let age_days = (age_ms as f64) / MS_PER_DAY;
            if age_days >= self.config.trigger_days as f64 {
                debug!(
                    %persona_uid,
                    age_days,
                    threshold = self.config.trigger_days,
                    "满足触发条件 B: 最早 L1 距今 {} 天 ≥ {} 天",
                    age_days,
                    self.config.trigger_days
                );
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// 将 LLM 返回的事件关系写入 event_relations 表。
    ///
    /// 参数:
    /// - `rels`: LLM 输出的关系列表（from_index/to_index 引用 events 数组索引）。
    /// - `event_ids`: 本簇已保存事件的实际 DB ID 列表（与 events 数组顺序一致）。
    /// - `persona_uid`: 人格标识（用于日志）。
    /// - `cluster_idx`: 簇索引（用于日志）。
    ///
    /// 返回:
    /// - 成功写入的关系数量。
    async fn save_cluster_relations(
        &self,
        rels: &[EventRelationOutput],
        event_ids: &[i64],
        persona_uid: &str,
        cluster_idx: usize,
    ) -> usize {
        let mut saved_count: usize = 0;
        let now = now_ms();

        for rel in rels {
            // 索引边界校验
            if rel.from_index >= event_ids.len() || rel.to_index >= event_ids.len() {
                warn!(
                    %persona_uid,
                    cluster_idx,
                    from_index = rel.from_index,
                    to_index = rel.to_index,
                    event_count = event_ids.len(),
                    "事件关系索引越界，跳过"
                );
                continue;
            }

            // 不允许自引用
            if rel.from_index == rel.to_index {
                debug!(
                    %persona_uid,
                    cluster_idx,
                    index = rel.from_index,
                    "事件关系自引用，跳过"
                );
                continue;
            }

            let kind = parse_relation_kind(&rel.kind);
            let from_id = event_ids[rel.from_index];
            let to_id = event_ids[rel.to_index];

            let event_rel = EventRelation {
                id: 0,
                from_id,
                to_id,
                kind,
                weight: rel.weight.clamp(0.0, 1.0),
                created_at: now,
            };

            match self.storage.save_event_relation(&event_rel).await {
                Ok(_) => {
                    saved_count += 1;
                }
                Err(e) => {
                    warn!(
                        %persona_uid,
                        cluster_idx,
                        from_id,
                        to_id,
                        kind = %event_rel.kind.as_str(),
                        error = %e,
                        "写入 event_relation 失败（非致命）"
                    );
                }
            }
        }

        saved_count
    }

    /// 从 TopicCluster 格式化 L1 摘要列表。
    ///
    /// 格式（v1.4 M4：每条 L1 可附带结构化证据线索行）:
    /// ```text
    /// [1] 2025-06-01 摘要文本 (keywords: kw1, kw2)
    /// [线索] 证据文本（time: 上周三；who: 用户；cause: 需求变更频繁）
    /// ```
    ///
    /// 说明:
    /// - evidence_notes 非空时，在摘要行下方输出 `[线索]` 行，
    ///   槽位仅展示非空项（缺失槽位省略，避免空占位干扰 LLM）。
    /// - cause 槽位承载因果线索，供 L2 事件提取作为背景参考（不视为事实断言）。
    fn format_l1_from_cluster(cluster: &TopicCluster) -> String {
        let mut lines = Vec::with_capacity(cluster.l1_items.len());
        for (i, item) in cluster.l1_items.iter().enumerate() {
            let date = timestamp_to_date_str(item.created_at);
            let kw_str = if item.keywords.is_empty() {
                String::new()
            } else {
                let kw_list: Vec<&str> = item.keywords.iter().map(|k| k.as_str()).collect();
                format!(" (keywords: {})", kw_list.join(", "))
            };
            lines.push(format!("[{}] {} {}{}", i + 1, date, item.summary, kw_str));

            // 结构化证据线索行（v1.4 M4）：非空时输出，槽位仅列非空项
            if !item.evidence_notes.is_empty() {
                for note in &item.evidence_notes {
                    let mut slots: Vec<String> = Vec::with_capacity(3);
                    if let Some(time) = note.time.as_deref() {
                        slots.push(format!("time: {time}"));
                    }
                    if let Some(who) = note.who.as_deref() {
                        slots.push(format!("who: {who}"));
                    }
                    if let Some(cause) = note.cause.as_deref() {
                        slots.push(format!("cause: {cause}"));
                    }
                    let slot_str = if slots.is_empty() {
                        String::new()
                    } else {
                        format!("（{}）", slots.join("；"))
                    };
                    lines.push(format!("[线索] {}{}", note.text, slot_str));
                }
            }
        }
        lines.join("\n")
    }

    /// 解析 LLM 响应为事件列表和关系列表。
    ///
    /// 三步递进策略（与 summarizer 一致）:
    /// 1. 直接 `serde_json::from_str`
    /// 2. 剥离 `<think>...</think>` 标签后重试
    /// 3. 正则提取 JSON 数组/对象
    ///
    /// 返回 `ParsedExtractionResult`，包含 events 和可选的 relations。
    fn parse_event_response(raw: &str) -> RamariaResult<ParsedExtractionResult> {
        // 步骤 1: 直接解析
        if let Ok(response) = serde_json::from_str::<EventResponse>(raw) {
            return response.into_result();
        }

        // 步骤 2: 剥离 think 标签
        let stripped = utils::strip_thinking(raw);
        if stripped != raw
            && let Ok(response) = serde_json::from_str::<EventResponse>(&stripped)
        {
            return response.into_result();
        }

        // 步骤 3: 正则提取
        if let Some(extracted) = utils::extract_first_json_array(raw)
            && let Ok(response) = serde_json::from_str::<EventResponse>(&extracted)
        {
            return response.into_result();
        }

        // 步骤 3b: LLM 可能返回完整的 JSON 对象（含 events/relations），
        // 而 extract_first_json_array 仅提取数组。尝试正则提取 JSON 对象 {...}。
        if let Some(obj_str) = utils::extract_first_json_object(raw)
            && let Ok(response) = serde_json::from_str::<EventResponse>(&obj_str)
        {
            return response.into_result();
        }

        Err(RamariaError::validation(format!(
            "事件 JSON 解析失败，原始响应前 200 字符: {}",
            raw.chars().take(200).collect::<String>()
        )))
    }

    /// 从 ExtractedEventJson 构建 MemoryEvent。
    ///
    /// motives 从 JSON 提取，过滤空字符串后以逗号分隔存储。
    ///
    /// 参数:
    /// - `situation_strength`: 从源 L1 传播的情境强度（1-5），
    ///   None 时等效 3（中性情境， 加权 ×1.0）。
    fn build_event(
        persona_uid: &str,
        json: ExtractedEventJson,
        start: i64,
        end: i64,
        now: i64,
        situation_strength: Option<i32>,
    ) -> MemoryEvent {
        let title = json.title.as_deref().unwrap_or("").trim().to_string();
        let title = if title.is_empty() || title.chars().count() > 20 {
            let truncated: String = title.chars().take(20).collect();
            if truncated.is_empty() {
                "（无标题事件）".to_string()
            } else {
                truncated
            }
        } else {
            title
        };

        let summary = json
            .summary
            .as_deref()
            .unwrap_or("（无描述）")
            .trim()
            .to_string();
        let summary = if summary.is_empty() {
            "（无描述）".to_string()
        } else {
            summary
        };

        let keywords = json
            .keywords
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let participants = json.participants.as_ref().and_then(|v| match v {
            serde_json::Value::Array(arr) => {
                let names: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if names.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&names).unwrap_or_default())
                }
            }
            _ => None,
        });

        let confidence = json.confidence.unwrap_or(0.5).clamp(0.0, 1.0);
        let salience = utils::clamp_salience(json.salience.unwrap_or(0.5));
        let valence = utils::clamp_valence(json.valence.unwrap_or(0.0));
        let presentation = parse_presentation(json.presentation.as_deref());
        let share = json.share.unwrap_or(0.5).clamp(0.0, 1.0);
        let attitude = json
            .attitude
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // 提取 motives → 过滤空串 → 逗号分隔存储
        let motives = json.motives.and_then(|m| {
            let filtered: Vec<&str> = m
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if filtered.is_empty() {
                None
            } else {
                Some(filtered.join(","))
            }
        });

        MemoryEvent {
            id: 0,
            persona_uid: persona_uid.to_string(),
            title,
            summary,
            keywords,
            participants,
            start,
            end,
            confidence,
            salience,
            valence,
            presentation,
            share,
            attitude,
            paraphrase: None, // 后续异步生成
            absorbed: 0,
            situation_strength,
            motives,
            created_at: now,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        }
    }
}

// =========================================================
// L2 聚类去重指纹辅助函数（v1.5 三层生成缓存 C，T-V15-3-003）
// =========================================================

/// 计算 L1 集合指纹：`sha256(按 L1 id 升序拼接的 id 列表)` 的 hex 摘要。
///
/// 性质:
/// - **顺序无关**：先排序再拼接，同一集合无论 L1 读取顺序如何均得同指纹。
/// - **集合敏感性**：新增/移除任一 L1 → 指纹必然变化 → 自动触发重新聚类
///   （同集合跳过、集合变更重聚类的核心）。
/// - **不落原文**：仅由 L1 id（UUID）推导，不含对话原文（隐私红线）。
pub(crate) fn compute_l1_set_fingerprint(l1_list: &[MemoryL1]) -> String {
    let mut ids: Vec<String> = l1_list.iter().map(|l| l.id.to_string()).collect();
    ids.sort();
    let joined = ids.join("|");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    hex_digest(&hasher.finalize())
}

/// 将 SHA-256 摘要编码为 64 字符小写 hex。
fn hex_digest(digest: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 计算两个事件的去重相似度（0.0..=1.0）。
///
/// 取「标题+摘要字符 bigram Jaccard」与「关键词集合 Jaccard」的**较大值**:
/// - 重跑/失败恢复场景: LLM 重新生成的标题/摘要措辞可能略有差异，
///   但关键词提取通常一致 → 关键词通道命中（高相似 → 判重）；
/// - 关键词缺失或两侧关键词不同的边缘场景 → 文本 bigram 通道兜底。
pub(crate) fn event_text_similarity(a: &MemoryEvent, b: &MemoryEvent) -> f64 {
    let bigram = char_bigram_jaccard(
        &format!("{} {}", a.title, a.summary),
        &format!("{} {}", b.title, b.summary),
    );
    let kw = keyword_jaccard(a.keywords.as_deref(), b.keywords.as_deref());
    bigram.max(kw)
}

/// 字符 bigram Jaccard 相似度。
///
/// 归一化（小写 + 仅保留字母数字/空白）后按 Unicode 字符取相邻二元组，
/// 对中英文混合文本均有效；两文本均无 bigram 时返回 0.0（不误判为相似）。
fn char_bigram_jaccard(text_a: &str, text_b: &str) -> f64 {
    let set_a = char_bigrams(&normalize_for_dedup(text_a));
    let set_b = char_bigrams(&normalize_for_dedup(text_b));
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }
    let inter = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    inter as f64 / union as f64
}

/// 归一化去重文本：小写化并仅保留字母数字与空白（去除标点干扰）。
fn normalize_for_dedup(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 取文本的 Unicode 字符二元组集合（相邻两字符为一组）。
fn char_bigrams(normalized: &str) -> HashSet<String> {
    let chars: Vec<char> = normalized.chars().collect();
    chars
        .windows(2)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

/// 关键词集合 Jaccard 相似度（关键词为逗号分隔字符串）。
///
/// 任一侧关键词缺失/为空时返回 0.0（信息不足不判重）。
fn keyword_jaccard(kw_a: Option<&str>, kw_b: Option<&str>) -> f64 {
    let set_a = split_keywords(kw_a);
    let set_b = split_keywords(kw_b);
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }
    let inter = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    inter as f64 / union as f64
}

/// 将逗号分隔的关键词字符串解析为去空集合。
fn split_keywords(kw: Option<&str>) -> HashSet<String> {
    kw.map(|s| {
        s.split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

// =========================================================
// EventResponse 辅助
// =========================================================

impl EventResponse {
    /// 将 `EventResponse` 转为统一的 `ParsedExtractionResult`。
    ///
    /// 处理四种 LLM 返回形式:
    /// 1. 新格式: `{"events": [...], "relations": [...]}` → Object
    /// 2. 旧格式: JSON 数组 `[...]` → Array
    /// 3. 单个对象 → 包装为单元素事件列表
    /// 4. 嵌套数组（罕见）→ 二次解包
    fn into_result(self) -> RamariaResult<ParsedExtractionResult> {
        match self {
            EventResponse::Object(resp) => Ok(ParsedExtractionResult {
                events: resp.events,
                relations: resp.relations,
            }),
            EventResponse::Array(events) => Ok(ParsedExtractionResult {
                events,
                relations: None,
            }),
            EventResponse::Single(val) => {
                // 尝试将单个对象包装为单元素事件列表
                if val.is_object() {
                    if let Ok(event) = serde_json::from_value::<ExtractedEventJson>(val) {
                        return Ok(ParsedExtractionResult {
                            events: vec![event],
                            relations: None,
                        });
                    }
                    return Err(RamariaError::validation("事件 JSON 对象反序列化失败"));
                }
                // 可能是嵌套数组（LLM 偶尔返回 [[...]] 形式）
                if let Ok(events) = serde_json::from_value::<Vec<ExtractedEventJson>>(val) {
                    return Ok(ParsedExtractionResult {
                        events,
                        relations: None,
                    });
                }
                Err(RamariaError::validation("事件响应格式无法识别"))
            }
        }
    }
}

fn parse_presentation(s: Option<&str>) -> Presentation {
    match s {
        Some("objective") => Presentation::Objective,
        Some("subjective") => Presentation::Subjective,
        Some("mixed") => Presentation::Mixed,
        _ => Presentation::Mixed,
    }
}

/// 将 LLM 输出的关系类型字符串解析为 `EventRelationKind`。
///
/// 说明:
/// - 六种标准关系类型：CausedBy/PartOf/RelatedTo/ContinuedBy/Contradicts/Timeline
/// - 不可识别时默认 `RelatedTo`，并记录 warn 日志。
fn parse_relation_kind(s: &str) -> EventRelationKind {
    match s {
        "CausedBy" => EventRelationKind::CausedBy,
        "PartOf" => EventRelationKind::PartOf,
        "RelatedTo" => EventRelationKind::RelatedTo,
        "ContinuedBy" => EventRelationKind::ContinuedBy,
        "Contradicts" => EventRelationKind::Contradicts,
        "Timeline" => EventRelationKind::Timeline,
        other => {
            tracing::warn!(kind = other, "未知事件关系类型，降级为 RelatedTo");
            EventRelationKind::RelatedTo
        }
    }
}

/// 将 Unix 毫秒时间戳转为 `YYYY-MM-DD` 字符串。
///
/// 用途: L1 摘要列表格式化，供 LLM 理解事件时间顺序。
///
/// 说明: 使用简化的儒略日算法，仅用于显示，不追求高精度到秒。
fn timestamp_to_date_str(ts_ms: i64) -> String {
    let total_secs = ts_ms / 1000;
    let days_since_epoch = total_secs / 86400;
    let (y, m, d) = days_to_ymd(days_since_epoch);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(total_days: i64) -> (i64, u32, u32) {
    let mut days = total_days;
    let mut year = 1970i64;

    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let month_days: [i64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u32;
    for &md in month_days.iter() {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, (days + 1) as u32)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::Presentation;
    use ramaria_core::types::now_ms;
    use uuid::Uuid;

    // ---- format_l1_from_cluster ----

    #[test]
    fn format_single_l1_from_cluster() {
        let item = L1Item {
            id: Uuid::new_v4(),
            summary: "测试摘要".into(),
            keywords: vec![
                ramaria_core::keyword::KeywordToken::new("测试").unwrap(),
                ramaria_core::keyword::KeywordToken::new("摘要").unwrap(),
            ],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 1_700_000_000_000,
        };
        let cluster = TopicCluster::new(vec![item]);
        let formatted = EventExtractor::format_l1_from_cluster(&cluster);
        assert!(formatted.contains("[1]"));
        assert!(formatted.contains("测试摘要"));
        assert!(formatted.contains("keywords"));
    }

    #[test]
    fn format_multiple_l1_from_cluster() {
        let item1 = L1Item {
            id: Uuid::new_v4(),
            summary: "第一条摘要".into(),
            keywords: vec![],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: now_ms(),
        };
        let item2 = L1Item {
            id: Uuid::new_v4(),
            summary: "第二条摘要".into(),
            keywords: vec![ramaria_core::keyword::KeywordToken::new("kw").unwrap()],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: now_ms(),
        };
        let cluster = TopicCluster::new(vec![item1, item2]);
        let formatted = EventExtractor::format_l1_from_cluster(&cluster);
        assert!(formatted.contains("[1]"));
        assert!(formatted.contains("[2]"));
        assert!(formatted.contains("第一条摘要"));
        assert!(formatted.contains("第二条摘要"));
    }
    /// v1.4 M4（T-V14-4-004）：evidence_notes 非空时格式化输出 `[线索]` 行，
    /// cause 因果线索槽位随行注入（仅供 L2 背景参考）。
    #[test]
    fn format_l1_with_evidence_notes_injects_clue_lines() {
        use ramaria_core::types::EvidenceNote;
        let item = L1Item {
            id: Uuid::new_v4(),
            summary: "用户讨论项目延期安排".into(),
            keywords: vec![],
            embedding: None,
            evidence_notes: vec![EvidenceNote {
                text: "用户提到项目延期到月底".into(),
                time: Some("上周三".into()),
                who: Some("用户".into()),
                cause: Some("需求变更频繁".into()),
            }],
            salience: 0.5,
            created_at: now_ms(),
        };
        let cluster = TopicCluster::new(vec![item]);
        let formatted = EventExtractor::format_l1_from_cluster(&cluster);
        assert!(formatted.contains("[线索]"), "应输出线索行标记");
        assert!(formatted.contains("用户提到项目延期到月底"), "应含证据文本");
        assert!(
            formatted.contains("cause: 需求变更频繁"),
            "应注入 cause 槽位"
        );
        assert!(formatted.contains("time: 上周三"), "应注入 time 槽位");
        assert!(formatted.contains("who: 用户"), "应注入 who 槽位");
    }

    /// v1.4 M4：线索的缺失槽位不输出占位（仅 text 的线索只输出文本本身）。
    #[test]
    fn format_l1_evidence_notes_omits_missing_slots() {
        use ramaria_core::types::EvidenceNote;
        let item = L1Item {
            id: Uuid::new_v4(),
            summary: "仅文本线索摘要".into(),
            keywords: vec![],
            embedding: None,
            evidence_notes: vec![EvidenceNote::new("用户提到通勤时间变长")],
            salience: 0.5,
            created_at: now_ms(),
        };
        let cluster = TopicCluster::new(vec![item]);
        let formatted = EventExtractor::format_l1_from_cluster(&cluster);
        assert!(formatted.contains("[线索] 用户提到通勤时间变长"));
        assert!(!formatted.contains("time:"), "缺失槽位不应输出占位");
        assert!(!formatted.contains("who:"));
        assert!(!formatted.contains("cause:"));
    }

    /// v1.4 M4：无证据线索时输出与 v1.3 完全一致（不产生空线索行，回归保护）。
    #[test]
    fn format_l1_without_evidence_notes_unchanged() {
        let item = L1Item {
            id: Uuid::new_v4(),
            summary: "无证据摘要".into(),
            keywords: vec![ramaria_core::keyword::KeywordToken::new("kw").unwrap()],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: now_ms(),
        };
        let cluster = TopicCluster::new(vec![item]);
        let formatted = EventExtractor::format_l1_from_cluster(&cluster);
        assert!(formatted.contains("[1]"));
        assert!(formatted.contains("无证据摘要"));
        assert!(formatted.contains("(keywords: kw)"));
        assert!(!formatted.contains("[线索]"), "无线索时不应出现线索行");
    }

    // ---- parse_event_response ----

    #[test]
    fn parse_valid_event_array() {
        let raw = r#"[
            {"title": "跳槽", "summary": "用户换了新工作", "confidence": 0.9, "salience": 0.75}
        ]"#;
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].title.as_deref(), Some("跳槽"));
        assert!(result.relations.is_none());
    }

    #[test]
    fn parse_empty_array() {
        let raw = "[]";
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert!(result.events.is_empty());
    }

    #[test]
    fn parse_single_object_wrapped() {
        // LLM 有时返回单对象而非数组
        let raw = r#"{"title": "事件", "summary": "描述", "confidence": 0.8, "salience": 0.5}"#;
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn parse_with_think_tags() {
        let raw = "<think>analyzing</think>\n[{\"title\": \"测试\", \"summary\": \"摘要\", \"confidence\": 0.7}]";
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn parse_with_prefix_text() {
        let raw = "以下是提取的事件：\n[{\"title\": \"事件\", \"summary\": \"描述\", \"confidence\": 0.8}]";
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let raw = "这不是JSON";
        assert!(EventExtractor::parse_event_response(raw).is_err());
    }

    // ---- 新格式解析 ----

    #[test]
    fn parse_v13_format_with_events_and_relations() {
        let raw = r#"{
            "events": [
                {"title": "跳槽", "summary": "换工作", "confidence": 0.9, "motives": ["自主"]},
                {"title": "失眠", "summary": "工作压力失眠", "confidence": 0.85}
            ],
            "relations": [
                {"from_index": 0, "to_index": 1, "kind": "CausedBy", "weight": 0.8, "detail": "跳槽导致压力"}
            ]
        }"#;
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0].title.as_deref(), Some("跳槽"));
        assert!(result.relations.is_some());
        let rels = result.relations.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].kind, "CausedBy");
        assert_eq!(rels[0].from_index, 0);
        assert_eq!(rels[0].to_index, 1);
    }

    #[test]
    fn parse_v13_format_without_relations() {
        let raw = r#"{
            "events": [
                {"title": "事件", "summary": "描述", "confidence": 0.7, "motives": ["归属"]}
            ]
        }"#;
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
        // relations 字段缺失 → None
        assert!(result.relations.is_none());
    }

    #[test]
    fn parse_v13_format_empty_relations() {
        let raw = r#"{
            "events": [
                {"title": "事件", "summary": "描述", "confidence": 0.7}
            ],
            "relations": []
        }"#;
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
        // 空 relations 数组 → Some([])
        assert!(result.relations.is_some());
        assert!(result.relations.unwrap().is_empty());
    }

    #[test]
    fn parse_v13_format_with_prefix_text() {
        // LLM 可能在 JSON 对象前加前缀文字
        let raw = "以下是提取的结果：\n{\"events\": [{\"title\": \"事件\", \"summary\": \"描述\", \"confidence\": 0.8}]}";
        let result = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(result.events.len(), 1);
    }

    // ---- parse_relation_kind ----

    /// parse_relation_kind 各类型参数化验证（未知类型回退 RelatedTo）。
    #[test]
    fn parse_relation_kind_cases() {
        use ramaria_core::types::EventRelationKind;
        let cases = [
            ("CausedBy", EventRelationKind::CausedBy),
            ("PartOf", EventRelationKind::PartOf),
            ("RelatedTo", EventRelationKind::RelatedTo),
            ("ContinuedBy", EventRelationKind::ContinuedBy),
            ("Contradicts", EventRelationKind::Contradicts),
            ("Timeline", EventRelationKind::Timeline),
            ("UnknownType", EventRelationKind::RelatedTo), // 未知 → 默认 RelatedTo
        ];
        for (input, expected) in cases {
            assert_eq!(parse_relation_kind(input), expected, "input={input:?}");
        }
    }

    // ---- build_event ----

    #[test]
    fn build_event_with_all_fields() {
        let json = ExtractedEventJson {
            title: Some("跳槽".into()),
            summary: Some("用户换了新工作".into()),
            keywords: Some("工作, 跳槽, 职业".into()),
            participants: Some(serde_json::json!(["老板", "同事"])),
            confidence: Some(0.9),
            salience: Some(0.75),
            valence: Some(-0.5),
            presentation: Some("subjective".into()),
            share: Some(0.3),
            attitude: Some("既兴奋又不安".into()),
            motives: Some(vec!["自主".to_string(), "地位".to_string()]),
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now - 1000, now, now, None);

        assert_eq!(event.title, "跳槽");
        assert_eq!(event.persona_uid, "user-0001");
        assert!((event.confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(event.presentation, Presentation::Subjective);
        assert!(event.situation_strength.is_none());
        assert!(event.attitude.is_some());
        assert_eq!(event.motives.as_deref(), Some("自主,地位"));
    }

    #[test]
    fn build_event_with_empty_motives() {
        let json = ExtractedEventJson {
            title: Some("事件".into()),
            summary: Some("描述".into()),
            keywords: None,
            participants: None,
            confidence: None,
            salience: None,
            valence: None,
            presentation: None,
            share: None,
            attitude: None,
            motives: Some(vec!["".to_string(), "  ".to_string()]),
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
        // 空字符串被过滤 → motives 为 None
        assert!(event.motives.is_none());
    }

    #[test]
    fn build_event_with_none_motives() {
        let json = ExtractedEventJson {
            title: Some("事件".into()),
            summary: Some("描述".into()),
            keywords: None,
            participants: None,
            confidence: None,
            salience: None,
            valence: None,
            presentation: None,
            share: None,
            attitude: None,
            motives: None,
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
        assert!(event.motives.is_none());
    }

    #[test]
    fn build_event_long_title_truncation() {
        let json = ExtractedEventJson {
            title: Some("这是一个超过二十个字的非常长的标题需要截断处理".into()),
            summary: Some("描述".into()),
            keywords: None,
            participants: None,
            confidence: None,
            salience: None,
            valence: None,
            presentation: None,
            share: None,
            attitude: None,
            motives: None,
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
        assert!(event.title.chars().count() <= 20);
    }

    #[test]
    fn build_event_defaults() {
        let json = ExtractedEventJson {
            title: Some("事件".into()),
            summary: Some("描述".into()),
            keywords: None,
            participants: None,
            confidence: None,
            salience: None,
            valence: None,
            presentation: None,
            share: None,
            attitude: None,
            motives: None,
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now, now, now, None);

        assert!((event.confidence - 0.5).abs() < f64::EPSILON);
        assert!((event.salience - 0.5).abs() < f64::EPSILON);
        assert!((event.valence - 0.0).abs() < f64::EPSILON);
        assert_eq!(event.presentation, Presentation::Mixed);
        assert!(event.situation_strength.is_none());
        assert_eq!(event.share, 0.5);
    }

    // ---- 钳制函数（与 utils.rs 自身测试重复，已删除） ----

    // ---- timestamp_to_date_str ----

    /// timestamp_to_date_str 各时间戳参数化验证。
    #[test]
    fn timestamp_to_date_cases() {
        let cases = [(0i64, "1970-01-01"), (1_748_736_000_000i64, "2025-06-01")];
        for (ts, expected) in cases {
            assert_eq!(timestamp_to_date_str(ts), expected, "ts={ts}");
        }
    }

    // ---- extract_first_json_array ----

    #[test]
    fn extract_array_simple() {
        let text = r#"前缀 [{"a":1}, {"b":2}] 后缀"#;
        let result = crate::utils::extract_first_json_array(text).unwrap();
        assert!(result.starts_with('['));
        assert!(result.ends_with(']'));
    }

    #[test]
    fn extract_array_no_brackets() {
        assert!(crate::utils::extract_first_json_array("no array here").is_none());
    }

    // ---- EventResponse::into_result ----

    #[test]
    fn response_array_to_result() {
        let resp = EventResponse::Array(vec![ExtractedEventJson {
            title: Some("测试".into()),
            summary: Some("摘要".into()),
            keywords: None,
            participants: None,
            confidence: Some(0.8),
            salience: Some(0.5),
            valence: Some(0.0),
            presentation: None,
            share: None,
            attitude: None,
            motives: None,
        }]);
        let result = resp.into_result().unwrap();
        assert_eq!(result.events.len(), 1);
        assert!(result.relations.is_none());
    }

    // ---- L2 聚类去重指纹（v1.5 T-V15-3-003）----

    /// 构造测试用 L1（仅指纹/相似度计算需要的字段有值）。
    fn l1_for_fp(id: &str, summary: &str) -> MemoryL1 {
        MemoryL1 {
            id: Uuid::parse_str(id).expect("合法 UUID"),
            session_id: Uuid::new_v4(),
            summary: summary.to_string(),
            keywords: None,
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: now_ms(),
            last_accessed_at: None,
            persona_uid: None,
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        }
    }

    #[test]
    fn fingerprint_is_deterministic_and_order_independent() {
        let a = l1_for_fp("00000000-0000-0000-0000-000000000001", "摘要 A");
        let b = l1_for_fp("00000000-0000-0000-0000-000000000002", "摘要 B");
        let c = l1_for_fp("00000000-0000-0000-0000-000000000003", "摘要 C");

        // 同集合（不同读取顺序）→ 同指纹
        let fp_abc = compute_l1_set_fingerprint(&[a.clone(), b.clone(), c.clone()]);
        let fp_cba = compute_l1_set_fingerprint(&[c.clone(), b.clone(), a.clone()]);
        assert_eq!(fp_abc, fp_cba, "集合指纹应与读取顺序无关");
        assert_eq!(fp_abc.len(), 64, "SHA-256 hex 应为 64 字符");

        // 集合变化（移除/新增 L1）→ 指纹变化（自动触发重聚类）
        let fp_ab = compute_l1_set_fingerprint(&[a, b]);
        assert_ne!(fp_abc, fp_ab, "移除 L1 后指纹应变化");
    }

    #[test]
    fn fingerprint_ignores_l1_content_but_is_stable_across_calls() {
        // 指纹仅由 L1 id 决定（隐私：不含摘要原文）；
        // 同一 id 集合即使摘要文本变化，指纹也稳定（保证重跑同集合稳定跳过）。
        let id = "00000000-0000-0000-0000-00000000000a";
        let fp1 = compute_l1_set_fingerprint(&[l1_for_fp(id, "第一次摘要")]);
        let fp2 = compute_l1_set_fingerprint(&[l1_for_fp(id, "重新生成的摘要")]);
        assert_eq!(fp1, fp2);
    }

    /// 构造测试用事件（title/summary/keywords 为相似度输入）。
    fn event_for_sim(title: &str, summary: &str, keywords: Option<&str>) -> MemoryEvent {
        MemoryEvent {
            id: 0,
            persona_uid: "p1".into(),
            title: title.into(),
            summary: summary.into(),
            keywords: keywords.map(|s| s.to_string()),
            participants: None,
            start: 0,
            end: 0,
            confidence: 0.5,
            salience: 0.5,
            valence: 0.0,
            presentation: Presentation::Mixed,
            share: 0.5,
            attitude: None,
            paraphrase: None,
            absorbed: 0,
            situation_strength: None,
            motives: None,
            created_at: 0,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        }
    }

    #[test]
    fn event_similarity_identical_text_is_high() {
        let a = event_for_sim(
            "项目延期",
            "用户表示项目延期到月底，原因是需求频繁变更",
            Some("项目,延期,需求"),
        );
        let b = event_for_sim(
            "项目延期",
            "用户表示项目延期到月底，原因是需求频繁变更",
            Some("项目,延期,需求"),
        );
        let sim = event_text_similarity(&a, &b);
        assert!(sim >= 0.99, "完全相同事件相似度应接近 1.0，实际 {sim}");
    }

    #[test]
    fn event_similarity_rerun_slight_wording_diff_hits_keyword_channel() {
        // 重跑场景：标题/摘要措辞有差异，但关键词一致 → 关键词通道保证判重
        let a = event_for_sim(
            "项目延期",
            "用户说项目要延期到月底，因为需求总在变",
            Some("项目,延期,需求"),
        );
        let b = event_for_sim(
            "项目延期",
            "用户表示项目延期至月底，由于需求频繁变更",
            Some("项目,延期,需求"),
        );
        let sim = event_text_similarity(&a, &b);
        assert!(sim >= 0.95, "关键词一致应判重，实际 {sim}");
    }

    #[test]
    fn event_similarity_unrelated_events_are_low() {
        let a = event_for_sim(
            "周末爬山",
            "用户周末和朋友去爬山，天气很好",
            Some("爬山,周末,天气"),
        );
        let b = event_for_sim("项目延期", "用户表示项目延期到月底", Some("项目,延期,需求"));
        let sim = event_text_similarity(&a, &b);
        assert!(sim < 0.95, "无关事件相似度应低于阈值，实际 {sim}");
    }

    #[test]
    fn event_similarity_missing_keywords_falls_back_to_text() {
        // 关键词缺失 → 纯文本 bigram 通道兜底
        let a = event_for_sim("加班到深夜", "用户连续加班到深夜，身体疲惫", None);
        let b = event_for_sim("加班到深夜", "用户连续加班到深夜，身体非常疲惫", None);
        let sim = event_text_similarity(&a, &b);
        assert!(sim > 0.7, "文本高度相似应通过 bigram 通道判高，实际 {sim}");
        assert!(sim < 1.0, "存在措辞差异，不应完全相等");
    }

    #[test]
    fn event_similarity_empty_text_is_zero() {
        let a = event_for_sim("", "", None);
        let b = event_for_sim("", "", None);
        assert_eq!(event_text_similarity(&a, &b), 0.0, "空文本不应误判为相似");
    }
}
