//! crates/ramaria-core/src/traits/store_version.rs - Ramaria BM25 版本与语料统计戳模块
//!
//! 设计特点:
//! - 定义 BM25 词典增强分词的 settings 键与新旧版本号
//! - 版本口径仅与分词相关，与记忆检索索引版本互不混用
//! - IndexCorpusStamp 以条数与最新写入时间刻画四类语料
//! - 供长驻进程做低价探测判断内存索引是否需要刷新
//! - 只含计数与时间戳，不含任何原文或隐私内容

// =========================================================
// BM25 词典增强分词版本（settings 表键 + 版本号）
// =========================================================

/// settings 表键：BM25 词典增强分词的迁移版本。
///
/// 说明:
/// - 仅与 BM25 索引的分词口径（纯 bigram / 词典增强）相关；
///   与 schema_meta.index_version（记忆检索索引是否构建过）互不混用。
pub const SETTING_BM25_INDEX_VERSION: &str = "bm25_index_version";

/// BM25 分词旧版本：纯 bigram（settings 缺失/不可解析视为本版本）。
pub const BM25_INDEX_VERSION_LEGACY: i32 = 1;

/// BM25 分词当前版本：词典增强（keyword_pool 规范词注入）。
pub const BM25_INDEX_VERSION_CURRENT: i32 = 2;

/// 记忆语料统计戳（跨进程索引刷新检测）。
///
/// 职责:
/// - 以"条数 + 最新写入时间"刻画参与内存检索索引的四类语料（L1 摘要 / L2 事件 /
///   utt 块 / 人格），供长驻进程（如 MCP 服务端）在召回前做低价探测：
///   其他进程写入过新内容时，内存索引需要刷新。
///
/// 字段约定:
/// - `*_count`: 对应表总条数（新增与删除都会引起变化）。
/// - `*_max_created_at`: 对应表 `created_at` 最大值（毫秒时间戳，空表为 0）。
///
/// 安全约束:
/// - 只含计数与时间戳，不含任何原文或隐私内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexCorpusStamp {
    /// 记忆 L1 摘要条数。
    pub l1_count: i64,
    /// 记忆 L1 摘要最新写入时间（毫秒时间戳，空表为 0）。
    pub l1_max_created_at: i64,
    /// L2 事件条数。
    pub event_count: i64,
    /// L2 事件最新写入时间（毫秒时间戳，空表为 0）。
    pub event_max_created_at: i64,
    /// utt 话语块条数。
    pub utt_count: i64,
    /// 人格条数。
    pub persona_count: i64,
}
