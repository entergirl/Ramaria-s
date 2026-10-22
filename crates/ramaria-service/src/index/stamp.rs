//! crates/ramaria-service/src/index/stamp.rs - Ramaria 索引代次与构建失败记录
//!
//! 设计特点:
//! - 代次快照供跨进程刷新判定：语料统计戳缺失时退化为同进程脏标记语义
//! - 构建失败记录与代次分开维护：失败不清除上一次成功构建的规模与模型
//! - 纯数据类型，不含 I/O

use ramaria_core::traits::IndexCorpusStamp;

///
/// 字段约定:
/// - `bm25_version`: BM25 分词代次（settings 键 `bm25_index_version`）；
///   词典升级后变化 → 本次重建按词典增强口径分词。
/// - `corpus`: 记忆语料统计戳（L1 / 事件 / utt / 人格的条数与最新写入时间）；
///   `None` = 后端不提供统计，退化为同进程脏标记语义（不误判为"已变化"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexStamp {
    pub bm25_version: i32,
    pub corpus: Option<IndexCorpusStamp>,
}

/// 最近一次索引构建失败记录（供诊断导出与宿主提示）。
///
/// 字段约定:
/// - `reason`: 失败原因的脱敏文本（已折叠为单行；路径只留文件名、消息类字段只留字符数，不含用户原文）；
/// - `at_ms`: 记录时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexBuildFailure {
    pub reason: String,
    pub at_ms: i64,
}

// =========================================================
// 懒加载与显式重建
// =========================================================
