//! rust/crates/ramaria-memory/src/event/mod.rs - L1→L2 事件提取模块
//!
//! 设计特点:
//! - batcher: TopicBatcher 主题批量构建（关键词 Jaccard 图 + BFS 连通分量 + 语义融合）
//! - context_retriever: CompositeIndex 三级编排补充上下文检索
//! - 按 persona_uid 分组取待吸收 L1
//! - 调用 LLM 提取结构化 JSON 事件（11 个推断属性）
//! - attitude → paraphrase 去情境化重述（LLM 轻量调用，结果缓存）
//! - 降级策略: 非标准 JSON → 退化为 confidence=0.5 混合事件
//! - 写入 memory_events / event_sources / 标记 L1 absorbed

pub mod batcher;
pub mod context_retriever;
pub mod degrade;
pub mod extractor;
pub mod paraphrase;
pub mod prompt;

pub use batcher::{L1Item, TopicBatcherConfig, TopicCluster};
pub use context_retriever::{ContextDocument, ContextRetriever, ContextRetrieverConfig};
pub use degrade::{DegradeConfig, build_degraded_event};
pub use extractor::{EventExtractor, EventExtractorConfig};
pub use paraphrase::{ParaphraseConfig, generate_paraphrase};
