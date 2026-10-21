//! crates/ramaria-service/src/index/status.rs - Ramaria 索引状态共享槽
//!
//! 设计特点:
//! - 状态槽只承载最近一次构建的状态与进度快照，读写都在短锁内完成
//! - 进度事件由引擎在锁外发出，状态槽不持有回调
//! - 百分比口径集中在此：无向量文档时 Ready 视为完成
//! - 索引代次读取集中在此：读取失败按"不变化"处理，不阻塞召回

use ramaria_core::traits::BM25_INDEX_VERSION_LEGACY;
use ramaria_core::types::IndexStatus;

use super::IndexStamp;
use crate::engine::Engine;

/// 索引状态共享槽（引擎持有，构建路径写入，宿主读取）。
///
/// 字段约定:
/// - `status`: 索引状态机当前值。
/// - `loaded` / `total`: 已加载向量文档数与向量文档总数。
/// - `failed`: 最近一次构建中向量补算失败的条数。
/// - `model`: 当前向量模型标识（无向量路径为 None）。
/// - `last_error`: 最近一次构建失败的脱敏原因（成功后清空）。
/// - `updated_at`: 最后更新时刻（Unix 毫秒；0 = 尚未更新）。
#[derive(Debug, Clone)]
pub(crate) struct IndexStatusState {
    pub(crate) status: IndexStatus,
    pub(crate) loaded: usize,
    pub(crate) total: usize,
    pub(crate) failed: usize,
    pub(crate) model: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) updated_at: i64,
}

impl Default for IndexStatusState {
    fn default() -> Self {
        Self {
            status: IndexStatus::Idle,
            loaded: 0,
            total: 0,
            failed: 0,
            model: None,
            last_error: None,
            updated_at: 0,
        }
    }
}

/// 计算进度百分比。
///
/// 口径:
/// - `total == 0`: Ready 为 100，其余状态为 0；
/// - 其余: `loaded * 100 / total`，封顶 100。
pub(crate) fn percent(status: IndexStatus, loaded: usize, total: usize) -> u8 {
    if total == 0 {
        return if status == IndexStatus::Ready { 100 } else { 0 };
    }
    ((loaded.saturating_mul(100)) / total).min(100) as u8
}

// =========================================================
// 索引代次读取
// =========================================================

/// 读取当前库内索引代次快照（读取失败按"不变化"处理并记 warn，不阻塞召回）。
pub(crate) async fn read_stamp(engine: &Engine) -> IndexStamp {
    let storage = engine.storage_ref().as_ref();

    let bm25_version = match storage.get_bm25_index_version().await {
        Ok(version) => version,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "读取 BM25 分词代次失败，本次按缺失口径比对（不影响召回本身）"
            );
            BM25_INDEX_VERSION_LEGACY
        }
    };

    let corpus = match storage.index_corpus_stamp().await {
        Ok(stamp) => stamp,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "读取索引语料统计失败，本次跳过跨进程刷新检测（同进程脏标记仍然生效）"
            );
            None
        }
    };

    IndexStamp {
        bm25_version,
        corpus,
    }
}
