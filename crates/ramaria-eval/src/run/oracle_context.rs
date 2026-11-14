//! crates/ramaria-eval/src/run/oracle_context.rs - Ramaria 评测压缩层 oracle 上下文
//!
//! 设计特点:
//! - 只读：不写库、不封存、不触碰检索器，条件间互不污染
//! - 只取 gold 消息覆盖到的 L1：上下文即压缩层上界，召回质量不参与
//! - 索引按工作库路径 + 人格缓存，同一进程内跨条件与跨题复用，严格模式各库各建一次
//! - 命中结果按发生时间升序去重后交给共用渲染，预算裁剪与 full-context 同一实现
//! - 空 gold 返回空串：预置上下文存在即跳过召回，注入为空

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_service::{Engine, L1MemoryView};

use super::context;

#[cfg(test)]
mod tests;

// =========================================================
// 索引
// =========================================================

/// 人格的 L1 → 消息索引。
///
/// 字段约定:
/// - `views`: 该人格全部 L1 摘要（采集顺序，渲染不依赖它排序）；
/// - `message_to_l1`: 消息 id → 覆盖该消息的 L1 下标集合。
#[derive(Debug)]
struct OracleIndex {
    views: Vec<L1MemoryView>,
    message_to_l1: HashMap<String, Vec<usize>>,
}

impl OracleIndex {
    /// 读取人格全部 L1 并展开消息区间。
    ///
    /// 说明: 单个 L1 展开失败按无覆盖处理（与服务层软引用容错口径一致）。
    async fn build(engine: &Engine, persona_uid: &str) -> RamariaResult<Self> {
        let views = context::collect_l1(engine, persona_uid).await?;
        let mut message_to_l1: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, view) in views.iter().enumerate() {
            let messages = engine
                .memory_unit_sources("l1", &view.id.to_string())
                .await?;
            for message in messages {
                message_to_l1.entry(message).or_default().push(index);
            }
        }
        Ok(Self {
            views,
            message_to_l1,
        })
    }

    /// gold 消息覆盖到的 L1 按时间升序渲染。
    ///
    /// 参数:
    /// - `gold`: gold 消息 id；
    /// - `budget_chars`: 字符预算（含头部与编号）。
    ///
    /// 返回:
    /// - 无命中（含空 gold）返回空串；
    /// - 多个 gold 消息落到同一 L1 时只渲染一次。
    fn render(&self, gold: &[String], budget_chars: usize) -> String {
        let mut hits: BTreeSet<usize> = BTreeSet::new();
        for message in gold {
            if let Some(indexes) = self.message_to_l1.get(message) {
                hits.extend(indexes.iter().copied());
            }
        }
        if hits.is_empty() {
            return String::new();
        }

        let mut views: Vec<L1MemoryView> = hits
            .into_iter()
            .map(|index| self.views[index].clone())
            .collect();
        views.sort_by_key(|view| (view.occurred_at, view.created_at, view.id));
        context::render(&views, budget_chars)
    }
}

// =========================================================
// 缓存
// =========================================================

/// 按「工作库路径 + 人格」缓存的 L1 索引。
///
/// 说明: 工作库在条件生命周期内只读，索引建一次即可跨题复用；
/// 严格模式下每个条件一份库，缓存键自然分开。
#[derive(Debug, Default)]
pub(super) struct OracleCache {
    indexes: HashMap<(PathBuf, String), Arc<OracleIndex>>,
}

impl OracleCache {
    /// 装配一条题的预置上下文。
    ///
    /// 参数:
    /// - `engine`: 条件引擎（只读使用）；
    /// - `db`: 该条件工作库路径，缓存键的一半；
    /// - `persona_uid`: 目标人格；
    /// - `gold`: gold 消息 id；
    /// - `budget_chars`: 字符预算。
    ///
    /// 返回:
    /// - 渲染文本；空 gold 或无命中返回空串。
    /// - 读取失败返回存储错误。
    pub(super) async fn context(
        &mut self,
        engine: &Engine,
        db: &Path,
        persona_uid: &str,
        gold: &[String],
        budget_chars: usize,
    ) -> RamariaResult<String> {
        if gold.is_empty() {
            return Ok(String::new());
        }
        let key = (db.to_path_buf(), persona_uid.to_string());
        let index = match self.indexes.get(&key) {
            Some(index) => index.clone(),
            None => {
                let index = Arc::new(OracleIndex::build(engine, persona_uid).await?);
                self.indexes.insert(key, index.clone());
                index
            }
        };
        Ok(index.render(gold, budget_chars))
    }
}
