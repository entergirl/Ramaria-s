//! crates/ramaria-memory/src/fact/mod.rs - 知识层（persona_facts 生命周期）
//!
//! 设计特点:
//! - 从事件抽取事实 → 判重 → 分层 → 版本链仲裁 → 检索注入全链路
//! - 判重: 同 field 语义余弦 ≥ 0.85 且关键词交集 ≥ 1 → 不入库（双条件）
//! - 分层: stable（不轻易覆盖）/ volatile（新覆盖旧保留版本链，随事件时间衰减）/ historical（只追加）
//! - 版本链仲裁: manual > 多事件互证 > 单事件（时间新者胜）；互证 = ≥2 独立事件 + 语义 ≥ 0.7 + valence 方向一致
//! - 主观隐含事实: conf=0.5 入 candidate 轨道，互证后提升 active
//! - 规则判定器检索注入: 零新增 LLM 调用，不命中不注入（静默降级）
//!
//! 模块组织:
//! - `dedup.rs`: 判重（纯逻辑，mock embedding 确定测试）
//! - `tier.rs`: 分层决策 + 时效衰减
//! - `arbitration.rs`: 冲突仲裁（互证/优先级/矛盾保护）
//! - `extractor.rs`: 事实抽取（规则兜底 + LLM 可选）
//! - `retriever.rs`: 规则判定器 + 同 field/向量召回 + 注入文本构造

pub mod arbitration;
pub mod dedup;
pub mod extractor;
pub mod retriever;
pub mod tier;

// =========================================================
// 公共类型与 re-export
// =========================================================

pub use arbitration::{ArbitrateOutcome, Arbitration, ArbitrationInput, Mutation};
pub use dedup::{DedupInput, DedupVerdict, check_dedup};
pub use extractor::{
    ExtractInput, FactCandidate, FactExtractor, RuleExtractor, build_extract_prompt,
};
pub use retriever::{
    KnowledgeQuery, KnowledgeRetrieval, MatchLevel, build_knowledge_injection,
    judge_knowledge_query, render_knowledge_cards,
};
pub use tier::{FactTierPolicy, decay_weight, describe_tier, tier_for_field};
