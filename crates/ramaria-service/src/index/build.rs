//! crates/ramaria-service/src/index/build.rs - Ramaria 索引构建辅助（事件加载 / 图谱 / utt 增量）
//!
//! 设计特点:
//! - L2 事件按配置分页全量或显式截断加载，避免超大库单次大查询
//! - 图谱节点以「人格归属 + 事件标题」为唯一键，跨人格同名事件互不覆盖
//! - utt 块增量镜像：构建在途或索引未加载时置脏，由下一次构建覆盖
//! - 纯构建辅助，不持有引擎，不跨 await 持锁

use std::collections::{HashMap, HashSet};

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::write_recover;
use ramaria_core::types::IndexStatus;

use ramaria_memory::retriever::L2DocView;

use crate::engine::Engine;

/// L2 事件分页加载的页大小（`l2_load_limit=0` 时逐页拉全量，避免单次大查询）。
const L2_LOAD_PAGE_SIZE: i64 = 500;

/// 加载单个 persona 的 L2 事件。
///
/// 口径:
/// - `[index].l2_load_limit = 0`（默认）: 分页全量加载，内存规模由 LRU 上限兜底；
/// - 大于 0: 只取最近 N 条（按开始时间倒序），显式截断。
///
/// 参数:
/// - `engine`: 服务层引擎（读取配置）。
/// - `storage`: 存储后端。
/// - `persona_uid`: 目标人格。
///
/// 返回:
/// - 事件列表（按开始时间倒序）；分页中途失败时上抛错误，由调用方降级。
pub(super) async fn load_persona_events(
    engine: &Engine,
    storage: &dyn ramaria_core::traits::StorageBackend,
    persona_uid: &str,
) -> RamariaResult<Vec<ramaria_core::types::MemoryEvent>> {
    let limit = engine.config().index.l2_load_limit as i64;
    if limit > 0 {
        return storage.list_events_by_persona(persona_uid, 0, limit).await;
    }

    let mut events = Vec::new();
    let mut offset = 0i64;
    loop {
        let page = storage
            .list_events_by_persona(persona_uid, offset, L2_LOAD_PAGE_SIZE)
            .await?;
        let fetched = page.len() as i64;
        events.extend(page);
        if fetched < L2_LOAD_PAGE_SIZE {
            break;
        }
        offset = offset.saturating_add(fetched);
    }
    Ok(events)
}

/// 图谱节点行：`(事件 id, 实体名, 实体类型)`。
pub(super) type GraphNodeRow = (i64, String, String);

/// 图谱边行：`(关系 id, 源事件 id, 目标事件 id, 关系类型)`。
pub(super) type GraphEdgeRow = (i64, i64, i64, String);

/// 从事件视图与事件关系构造图谱节点与边。
///
/// 说明:
/// - 节点以事件为实体：`(事件 id, {persona_uid}\u{1f}{事件标题}, "event")`，
///   人格归属参与节点键，避免跨人格同名事件互相覆盖并在检索期统一过滤；
///   实体名中的标题部分供查询文本子串匹配，空标题跳过；
/// - `GraphRetriever` 节点集合以实体名为键（同名覆盖），为确定性保留首个出现的
///   节点，跳过后续同名节点（只记 debug 跳过数，不记标题文本）；
/// - 边以事件关系为数据源，仅保留两端都在节点集合内的边（事件被空标题过滤 /
///   加载截断时不产生悬挂引用）；
/// - 关系类型权重由 `GraphRetrieverConfig` 既有口径处理（未收录类型走默认权重）。
pub(super) fn build_graph_data(
    l2_views: &[L2DocView],
    relations: &[ramaria_core::types::EventRelation],
) -> (Vec<GraphNodeRow>, Vec<GraphEdgeRow>) {
    let mut nodes: Vec<GraphNodeRow> = Vec::with_capacity(l2_views.len());
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut skipped_duplicates = 0usize;

    for event in l2_views {
        let title = event.title.trim();
        if title.is_empty() {
            continue;
        }
        let key = ramaria_memory::graph_retriever::graph_entity_key(
            Some(event.persona_uid.as_str()),
            title,
        );
        if !seen_names.insert(key.clone()) {
            skipped_duplicates += 1;
            continue;
        }
        nodes.push((event.id, key, "event".to_string()));
    }
    if skipped_duplicates > 0 {
        tracing::debug!(skipped = skipped_duplicates, "图谱构建跳过同名事件节点");
    }

    let node_ids: HashSet<i64> = nodes.iter().map(|(id, _, _)| *id).collect();
    let edges: Vec<GraphEdgeRow> = relations
        .iter()
        .filter(|relation| {
            node_ids.contains(&relation.from_id) && node_ids.contains(&relation.to_id)
        })
        .map(|relation| {
            (
                relation.id,
                relation.from_id,
                relation.to_id,
                relation.kind.as_str().to_string(),
            )
        })
        .collect();

    (nodes, edges)
}

/// 把新生成的 utt 块增量加入内存检索索引（不重建整库）。
///
/// 行为:
/// - 索引已加载且无构建在途 → 逐块写入 BM25 / 原文镜像与可选向量；
/// - 索引未加载或构建在途 → 置脏标记（下次构建从存储全量载入，不会漏）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `blocks`: 本次新建的块（含入库 id）。
/// - `vectors`: 块 id → 向量（仅包含成功生成的向量）。
///
/// 返回:
/// - 实际增量写入的块数；置脏路径返回 0（由下一次构建覆盖）。
pub(crate) fn index_utt_blocks_incrementally(
    engine: &Engine,
    blocks: &[ramaria_core::types::UttBlock],
    vectors: &HashMap<i64, Vec<f32>>,
) -> usize {
    if blocks.is_empty() {
        return 0;
    }
    if engine.is_index_build_inflight() || engine.index_status().status == IndexStatus::Building {
        engine.mark_index_dirty();
        tracing::debug!(
            blocks = blocks.len(),
            "检索索引构建中，utt 增量已置脏（由下次构建覆盖）"
        );
        return 0;
    }
    let slot = engine.retriever_slot();
    let mut guard = write_recover(&*slot, "index.retriever_slot");
    let Some(retriever) = guard.as_mut() else {
        engine.mark_index_dirty();
        tracing::debug!(
            blocks = blocks.len(),
            "检索索引未加载，utt 增量已置脏（下次加载重建）"
        );
        return 0;
    };
    for block in blocks {
        retriever.index_utt_block(block, vectors.get(&block.id).cloned());
    }
    blocks.len()
}
