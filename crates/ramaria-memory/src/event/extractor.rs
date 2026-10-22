//! crates/ramaria-memory/src/event/extractor.rs - L1→L2 事件提取管线
//!
//! 设计特点:
//! - 依赖注入: 通过 `&dyn LlmProvider` + `&dyn StorageBackend` 解耦具体实现
//! - 使用 `TopicBatcher` 语义聚类替代旧 `chat_partners + take(20)` 分批策略
//! - 通过可选的 `Retriever` 引用启用 CompositeIndex 补充上下文检索
//! - Prompt 新增 motives（底层动机）+ relations（事件关系）输出
//! - 激活 motives 字段写入 + event_relations 表写入
//! - 触发条件: 未吸收 L1 ≥ 5 条 或 最早未吸收 L1 ≥ 7 天
//! - TopicBatcher 将未吸收 L1 聚类为 TopicCluster，每簇独立调用 LLM 提取事件
//! - 降级兜底: LLM 调用失败或 JSON 解析失败 → 退化为低置信混合事件
//! - 事件写入前自动生成 paraphrase（attitude 存在且非空时）
//! - 全流程收集事件 / 来源 / 关系后在单事务内写入并标记 L1 吸收（任一步失败整体回滚）
//! - 所有可恢复错误转换为 RamariaError，保留上下文
//! - 辅助逻辑按职责拆入子模块（trigger/convert/relations/batch），本文件保留主流程与配置

use ramaria_core::traits::ChatRequest;
use ramaria_core::types::now_ms;
use ramaria_core::{
    EventBatchWrite, LlmProviderTrait, MemoryEvent, MemoryL1, RamariaError, RamariaResult,
    StorageBackend,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::batcher::{L1Item, TopicBatcher, TopicBatcherConfig};
use super::context_retriever::{ContextRetriever, ContextRetrieverConfig};
use super::degrade::DegradeConfig;
use super::paraphrase::{ParaphraseConfig, generate_paraphrase};
use super::prompt::{
    build_event_extraction_prompt_for_persona,
    build_event_extraction_prompt_with_context_for_persona,
};
use crate::retriever::Retriever;
use crate::utils;

mod batch;
mod convert;
mod dedup;
mod parse;
mod relations;
mod trigger;

use dedup::{compute_l1_set_fingerprint, event_text_similarity};
use parse::ExtractedEventJson;

#[cfg(test)]
mod tests;

/// 一天的毫秒数常量。
const MS_PER_DAY: f64 = utils::MS_PER_DAY;

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
    /// L2 聚类去重指纹开关（v1.5 三层生成缓存 C）。
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
/// // 依赖 &dyn LlmProviderTrait 与 &dyn StorageBackend（及可选 &Retriever），
/// // 需完整 mock 才能运行，示例仅示意调用形态。
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
}

mod pipeline;
