//! crates/ramaria-service/src/index.rs - 检索索引构建、懒加载与增量镜像
//!
//! 设计特点:
//! - 懒加载：首次召回前构建一次（进程启动不为大库付加载代价），构建完成后整体替换
//! - 显式重建：`rebuild` 跳过懒加载早退与冷却窗口，供宿主手动刷新整库索引
//! - 原子替换：新索引在临时实例上完整构建，成功后一次写入替换；读者要么见旧索引，
//!   要么见新索引，不会读到半成品；构建失败旧索引保持可用并置"重建失败"告警位
//! - 数据来源与在线管线一致：各 persona 的未吸收 L1 + L2 事件 + utt 块，另加无主 L1
//! - 降级链：嵌入模型缺失 / 批量向量化失败 → 仅 BM25 + 关键词镜像（不阻塞构建）；
//!   关键词词表读取失败 → 空词典（纯 bigram 口径，行为可预期）
//! - BM25 词典增强迁移：旧分词版本 + 已确认词表非空 → 重建切为词典增强口径并写回版本标记
//! - 增量镜像：L1 生成后同步进检索器与关键词镜像（不重建整库）
//! - 代次刷新：召回前比对库内语料戳与 BM25 分词代次，其他进程写入 / 词典升级后
//!   重建内存索引（同进程脏标记保留为加载窗口内的兜底）；
//!   代次变化触发的重建再受 `[index].refresh_interval_seconds` 冷却窗口约束（0 = 不节流）
//! - 边界：本模块只做"内存索引维护"，不写数据库（除索引版本 / BM25 分词版本标记外）

use std::collections::HashSet;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::{
    BM25_INDEX_VERSION_CURRENT, BM25_INDEX_VERSION_LEGACY, IndexCorpusStamp,
};
use ramaria_core::types::{MemoryL1, now_ms};
use ramaria_memory::keyword::service::KeywordService;
use ramaria_memory::keyword::{CommaSeparatedNormalizer, KeywordNormalizer};
use ramaria_memory::retriever::{L1DocView, L2DocView, Retriever, RetrieverConfig};
// 向量索引写入（`CachedVectorIndex::add`）经 `VectorIndex` trait 提供
use ramaria_memory::VectorIndex;

use crate::engine::Engine;

/// L2 事件单次加载上限（与在线管线重建口径一致，避免超大库一次性拉全量）。
const L2_LOAD_LIMIT: i64 = 1_000;

// =========================================================
// 索引代次（跨进程刷新检测）
// =========================================================

/// 索引代次快照：判定内存索引是否与库内语料一致。
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
/// - `reason`: 失败原因的脱敏文本（已折叠为单行；路径只留文件名、消息类字段只留字符数，
///   不含用户原文）；
/// - `at_ms`: 记录时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexBuildFailure {
    pub reason: String,
    pub at_ms: i64,
}

/// 读取当前库内索引代次快照（读取失败按"不变化"处理并记 warn，不阻塞召回）。
async fn read_stamp(engine: &Engine) -> IndexStamp {
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

// =========================================================
// 懒加载与显式重建
// =========================================================

/// 确保检索索引已加载且与库内语料同代（懒加载 + 代次刷新）。
///
/// 流程:
/// 1. 读取库内代次快照（语料统计 + BM25 分词代次）；
/// 2. 已加载、无脏标记、代次一致 → 直接返回 false；
/// 3. 否则走 [`build_and_swap`] 完整构建路径（清脏后构建）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - `Ok(true)`: 本次调用完成构建（首次加载 / 脏标记重建 / 代次变化刷新）。
/// - `Ok(false)`: 索引已就绪且无需重建。
///
/// 说明:
/// - 并发调用可能重复构建（幂等：最后一次替换生效）。
/// - 脏标记协同（避免丢 L1）：加载窗口内封存产生的新 L1 进不了内存索引，其增量镜像会置脏；
///   本函数在开始构建前清脏，构建期间再产生的增量会重新置脏 → 下次调用再补一次（收敛）。
/// - 代次协同（跨进程）：构建时记录的是构建前读取的快照；构建窗口内其他进程的新写入
///   会让下次比对不等 → 再刷新一次（收敛，不漏新记忆）。
/// - 构建失败时置"重建失败"告警位并上抛错误（旧索引保持可用，见 [`build_and_swap`]）。
pub(crate) async fn ensure_loaded(engine: &Engine) -> RamariaResult<bool> {
    let stamp = read_stamp(engine).await;

    // 已加载且无脏标记 → 只需确认代次是否仍一致；一致则免构建
    if engine.is_retriever_loaded() && !engine.index_dirty() {
        if engine.index_stamp() == Some(stamp) {
            return Ok(false);
        }
        // 代次变化（其他进程写入 / 分词代次升级）触发的重建受配置的最小间隔约束：
        // 冷却窗口内沿用现有索引（窗口过后的下一次召回在同一分支补上重建），
        // 用于写入密集期抑制整库重建风暴；首次加载与同进程脏标记路径不受此约束。
        if !engine.index_rebuild_cooldown_elapsed() {
            tracing::debug!(
                bm25_version = stamp.bm25_version,
                interval_seconds = engine.config().index.refresh_interval_seconds,
                "索引代次已变化，但在重建冷却窗口内：本次沿用现有索引"
            );
            return Ok(false);
        }
        tracing::info!(
            bm25_version = stamp.bm25_version,
            corpus_tracked = stamp.corpus.is_some(),
            "索引代次变化（其他进程写入或分词代次升级），重建内存索引"
        );
    }
    // 构建开始前清脏：构建窗口内新产生的增量会重新置脏（保证不漏、且能收敛）
    engine.clear_index_dirty();

    build_and_swap(engine).await?;
    Ok(true)
}

/// 强制全量重建内存检索索引（跳过懒加载早退与冷却窗口）。
///
/// 用法:
/// - 宿主显式刷新（批量导入完成 / 设置变更 / 诊断修复等场景）调用；
/// - 与懒加载路径共用同一构建实现，不受 `[index].refresh_interval_seconds` 约束。
///
/// 返回:
/// - `Ok(total)`: 重建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
/// - `Err(..)`: 构建失败（旧索引保持可用，告警位置位）。
pub(crate) async fn rebuild(engine: &Engine) -> RamariaResult<usize> {
    build_and_swap(engine).await
}

/// 构建新索引并整体替换懒加载槽（懒加载与显式重建共用的完整构建路径）。
///
/// 流程:
/// 1. 读取库内代次快照（构建前读取，构建完成后记录供下次比对）；
/// 2. 锁外收集文档视图（各 persona 的未吸收 L1 / L2 事件 / utt 块 / 事件关系 + 无主 L1）；
/// 3. 评估 BM25 词典增强迁移决策（已确认词表 + 分词版本标记）；
/// 4. 临时检索器构建（检索配置 + 词典 + 文档索引 + 可选向量）；
/// 5. 图谱构建（事件为实体节点、事件关系为边，装入同一临时实例）；
/// 6. 整体替换懒加载槽（读者不读半成品）；
/// 7. 词典增强迁移完成后写回分词版本标记；
/// 8. 关键词镜像装载（词典池 + 倒排文档 + 语义层）；
/// 9. 记录代次快照与构建完成时间，写回索引版本（失败只记日志）。
///
/// 返回:
/// - `Ok(total)`: 构建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
/// - `Err(..)`: 关键读取失败（旧索引保持可用），并置"重建失败"告警位。
///
/// 降级:
/// - persona 事件 / utt 块 / 事件关系 / 词典 / 向量读取失败按降级链处理，不阻塞构建；
/// - 成功完成后复位"重建失败"告警位（供诊断展示与宿主告警）。
async fn build_and_swap(engine: &Engine) -> RamariaResult<usize> {
    match build_and_swap_inner(engine).await {
        Ok(total) => {
            engine.set_index_rebuild_failed(false);
            engine.clear_index_build_failure();
            Ok(total)
        }
        Err(e) => {
            // 旧索引仍完整可用：置位告警位供宿主提示"记忆注入可能不完整"，
            // 并记录脱敏失败原因供诊断导出（不改变返回错误与降级语义）
            engine.set_index_rebuild_failed(true);
            engine.record_index_build_failure(redact_failure_reason(&e));
            tracing::warn!(
                error = %e,
                "检索器重建失败，保留旧索引继续可用（索引未刷新）"
            );
            Err(e)
        }
    }
}

/// 构造失败原因文本：复用诊断导出的二次脱敏口径（路径只留文件名、
/// 消息类字段只留字符数），并折叠为单行供状态摘要展示（不含用户原文）。
fn redact_failure_reason(error: &RamariaError) -> String {
    let text = crate::diagnostics::redact_for_export(&error.to_string());
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// [`build_and_swap`] 的实际构建实现（告警位维护在外层）。
async fn build_and_swap_inner(engine: &Engine) -> RamariaResult<usize> {
    let storage = engine.storage_ref().as_ref();
    // 构建前读取代次快照；构建窗口内的新写入会在下次比对时触发再刷新
    let stamp = read_stamp(engine).await;
    let started = now_ms();

    // ---- 1. 视图收集（锁外 I/O） ----
    let mut l1_views: Vec<L1DocView> = Vec::new();
    let mut l2_views: Vec<L2DocView> = Vec::new();
    let mut utt_blocks: Vec<ramaria_core::types::UttBlock> = Vec::new();
    let mut event_relations: Vec<ramaria_core::types::EventRelation> = Vec::new();

    for persona in storage.list_personas().await? {
        for l1 in storage.list_unabsorbed_l1(&persona.uid).await? {
            l1_views.push(l1_view(&l1));
        }
        // L2 事件：失败降级为空（单个 persona 的事件读取失败不阻塞整体构建）
        match storage
            .list_events_by_persona(&persona.uid, 0, L2_LOAD_LIMIT)
            .await
        {
            Ok(events) => l2_views.extend(events.iter().map(l2_view)),
            Err(e) => {
                tracing::warn!(persona_uid = %persona.uid, error = %e, "读取事件失败，跳过该 persona 的事件");
            }
        }
        // 事件关系：失败降级为空（单个 persona 的关系读取失败只丢该 persona 的边）
        match storage.list_event_relations_by_persona(&persona.uid).await {
            Ok(relations) => event_relations.extend(relations),
            Err(e) => {
                tracing::warn!(persona_uid = %persona.uid, error = %e, "读取事件关系失败，跳过该 persona 的关系");
            }
        }
        // utt 块：失败降级为空（原文通道缺失，其余通道不受影响）
        match storage.list_utt_blocks_by_persona(&persona.uid).await {
            Ok(blocks) => utt_blocks.extend(blocks),
            Err(e) => {
                tracing::warn!(persona_uid = %persona.uid, error = %e, "读取 utt 块失败，跳过该 persona");
            }
        }
    }

    // 无主 L1（persona_uid IS NULL，导入产生）：检索侧对 NULL 归属不做过滤，必须一并加载
    match storage.list_unabsorbed_l1_unbound().await {
        Ok(unbound) => l1_views.extend(unbound.iter().map(l1_view)),
        Err(e) => {
            tracing::warn!(error = %e, "读取无主 L1 失败，导入摘要可能不可检索");
        }
    }

    let total = l1_views.len() + l2_views.len();

    // ---- 2. BM25 词典增强迁移决策（词表 + 分词版本标记） ----
    let migration = prepare_bm25_migration(engine).await;

    // ---- 3. 临时实例构建（检索配置 + 词典 + 文档索引 + 可选向量） ----
    let mut fresh = Retriever::new();
    *fresh.config_mut() = RetrieverConfig::from_retrieval_config(&engine.config().retrieval);
    if migration.apply_dictionary {
        // 空词典 = 纯 bigram 等价口径：重建总是显式注入，保证口径可预期
        fresh.set_bm25_dictionary(&migration.dictionary);
    }
    for doc in &l1_views {
        fresh.index_l1(doc);
    }
    for doc in &l2_views {
        fresh.index_l2(doc);
    }
    for block in &utt_blocks {
        fresh.index_utt_block(block);
    }
    let vectors_built = build_vectors(engine, &mut fresh, &l1_views, &l2_views).await;

    // ---- 4. 图谱构建（事件为实体节点、事件关系为边）----
    // 无论 `enable_graph` 开关值如何都构建（数据量小；检索期由
    // `RetrieverConfig.enable_graph` 短路，见 `retriever/search.rs` 的通道判断）。
    let (graph_nodes, graph_edges) = build_graph_data(&l2_views, &event_relations);
    fresh.graph_mut().load(&graph_nodes, &graph_edges);

    // ---- 5. 整体替换懒加载槽（此后读者要么见旧索引，要么见新索引）----
    {
        let slot = engine.retriever_slot();
        let mut guard = write_recover(&*slot, "index.retriever_slot");
        *guard = Some(fresh);
    }

    // ---- 6. 词典增强迁移版本写回（索引已是词典增强口径；失败下次重建自动重试）----
    if migration.mark_v2 {
        match storage
            .set_bm25_index_version(BM25_INDEX_VERSION_CURRENT)
            .await
        {
            Ok(()) => {
                tracing::info!(
                    version = BM25_INDEX_VERSION_CURRENT,
                    "BM25 词典增强分词迁移完成"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "写入 BM25 分词版本失败（索引已按词典增强重建，下次重建自动重试）"
                );
            }
        }
    }

    // ---- 7. 关键词镜像装载（词典池 + 倒排文档 + 语义层） ----
    sync_keyword_mirror(engine, &l1_views, &l2_views).await;

    // ---- 8. 记录代次快照与构建完成时间 ----
    engine.record_index_stamp(stamp);
    // 记"完成时间"：冷却窗口按两次重建之间的实际间隔计算（含本次构建耗时）
    engine.record_index_build_time(now_ms());

    // ---- 9. 标记索引已构建（判定只看 `== 0`，写 1 表"已构建"）----
    // 供首次配置状态机判定"索引待构建"项消失；写入失败只记日志，不影响内存索引可用性。
    if let Err(e) = storage.set_index_version(1).await {
        tracing::warn!(
            error = %e,
            "写入索引版本失败（内存索引已可用，首次配置状态判定可能滞后）"
        );
    }

    tracing::info!(
        l1 = l1_views.len(),
        l2 = l2_views.len(),
        utt = utt_blocks.len(),
        graph_nodes = graph_nodes.len(),
        graph_edges = graph_edges.len(),
        vectors = vectors_built,
        elapsed_ms = now_ms().saturating_sub(started),
        bm25_version = stamp.bm25_version,
        "检索索引已加载（共 {total} 条文档）"
    );
    Ok(total)
}

/// 读取 BM25 词典增强分词迁移决策。
///
/// 说明:
/// - settings 键 `bm25_index_version` 缺失 / 不可解析视为旧版本（纯 bigram）；
/// - 词典来源为已确认词表（canonical + 已确认 alias，排除 pending）；
///   读取失败 → 记 warn 并返回 `apply_dictionary=false`（本次不注入词典）、`mark_v2=false`；
/// - 仅当分词版本非当前、且词典非空时 `mark_v2=true`
///   （词典为空时重建与旧版等价，无需升级标记）。
async fn prepare_bm25_migration(engine: &Engine) -> Bm25MigrationPlan {
    let storage = engine.storage_ref().as_ref();

    let current_version = match storage.get_bm25_index_version().await {
        Ok(version) => version,
        Err(e) => {
            tracing::warn!(error = %e, "读取 BM25 分词版本失败，按旧版本处理并尝试迁移");
            BM25_INDEX_VERSION_LEGACY
        }
    };

    let dictionary = match storage.list_established_keywords().await {
        Ok(dictionary) => dictionary,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "加载已确认词表失败，本轮重建不注入词典（保留纯 bigram 口径）"
            );
            return Bm25MigrationPlan {
                dictionary: Vec::new(),
                apply_dictionary: false,
                mark_v2: false,
            };
        }
    };

    let mark_v2 = current_version != BM25_INDEX_VERSION_CURRENT && !dictionary.is_empty();
    tracing::info!(
        current_version,
        current = BM25_INDEX_VERSION_CURRENT,
        dict_size = dictionary.len(),
        mark_v2,
        "BM25 词典增强分词检查完成"
    );
    Bm25MigrationPlan {
        dictionary,
        apply_dictionary: true,
        mark_v2,
    }
}

/// BM25 词典增强分词迁移计划（构建前评估，整体替换成功后按需写回版本标记）。
struct Bm25MigrationPlan {
    /// 词典词条（空 = 纯 bigram 口径）。
    dictionary: Vec<String>,
    /// 是否把词典应用到本次重建（词表读取失败时为 false → 本次不注入词典）。
    apply_dictionary: bool,
    /// 重建成功后是否写回 `bm25_index_version = 当前版本`。
    mark_v2: bool,
}

/// 为临时检索器构建向量索引（嵌入不可用 / 失败时静默降级）。
///
/// 返回:
/// - 实际写入向量索引的文档数（0 = 向量通道缺席）。
async fn build_vectors(
    engine: &Engine,
    fresh: &mut Retriever,
    l1_views: &[L1DocView],
    l2_views: &[L2DocView],
) -> usize {
    let Some(provider) = engine.embedding_ref() else {
        tracing::debug!("嵌入模型未配置，本次索引构建跳过向量通道");
        return 0;
    };
    if !provider.is_available() {
        tracing::debug!("嵌入模型不可用，本次索引构建跳过向量通道");
        return 0;
    }

    let mut built = 0usize;

    // L1 摘要向量
    let l1_texts: Vec<&str> = l1_views.iter().map(|doc| doc.summary.as_str()).collect();
    if !l1_texts.is_empty() {
        match provider.embed_batch(&l1_texts).await {
            Ok(vectors) => {
                for (doc, vector) in l1_views.iter().zip(vectors) {
                    let label =
                        ramaria_memory::vector::make_vector_label("l1", &doc.id.to_string());
                    fresh.vector_mut().add(&label, vector, doc.created_at);
                    built += 1;
                }
            }
            Err(e) => tracing::warn!(error = %e, "L1 批量向量化失败，向量通道降级"),
        }
    }

    // L2 标题向量（与在线管线重建口径一致：标题作为事件语义代表）
    let l2_texts: Vec<&str> = l2_views.iter().map(|doc| doc.title.as_str()).collect();
    if !l2_texts.is_empty() {
        match provider.embed_batch(&l2_texts).await {
            Ok(vectors) => {
                for (doc, vector) in l2_views.iter().zip(vectors) {
                    let label =
                        ramaria_memory::vector::make_vector_label("l2", &doc.id.to_string());
                    fresh.vector_mut().add(&label, vector, doc.created_at);
                    built += 1;
                }
            }
            Err(e) => tracing::warn!(error = %e, "L2 批量向量化失败，向量通道降级"),
        }
    }

    built
}

/// 图谱节点行：`(事件 id, 实体名, 实体类型)`。
type GraphNodeRow = (i64, String, String);

/// 图谱边行：`(关系 id, 源事件 id, 目标事件 id, 关系类型)`。
type GraphEdgeRow = (i64, i64, i64, String);

/// 从事件视图与事件关系构造图谱节点与边。
///
/// 说明:
/// - 节点以事件为实体：`(事件 id, 事件标题, "event")`，事件标题即实体名
///   （供查询文本子串匹配），空标题跳过；
/// - `GraphRetriever` 节点集合以实体名为键（同名覆盖），为确定性保留首个出现的
///   节点，跳过后续同名节点（只记 debug 跳过数，不记标题文本）；
/// - 边以事件关系为数据源，仅保留两端都在节点集合内的边（事件被空标题过滤 /
///   加载截断时不产生悬挂引用）；
/// - 关系类型权重由 `GraphRetrieverConfig` 既有口径处理（未收录类型走默认权重）。
fn build_graph_data(
    l2_views: &[L2DocView],
    relations: &[ramaria_core::types::EventRelation],
) -> (Vec<GraphNodeRow>, Vec<GraphEdgeRow>) {
    let mut nodes: Vec<GraphNodeRow> = Vec::with_capacity(l2_views.len());
    let mut seen_names: HashSet<&str> = HashSet::new();
    let mut skipped_duplicates = 0usize;

    for event in l2_views {
        let entity_name = event.title.trim();
        if entity_name.is_empty() {
            continue;
        }
        if !seen_names.insert(entity_name) {
            skipped_duplicates += 1;
            continue;
        }
        nodes.push((event.id, entity_name.to_string(), "event".to_string()));
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

/// 装载关键词镜像（词典池 + 倒排文档 + 语义层），任一环节失败静默降级。
async fn sync_keyword_mirror(engine: &Engine, l1_views: &[L1DocView], l2_views: &[L2DocView]) {
    let storage = engine.storage_ref().as_ref();

    // 词典池行（失败 → 空词典，倒排仍可用）
    let rows = match storage.list_keyword_pool_entries().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "加载关键词词条失败，镜像词典为空");
            Vec::new()
        }
    };

    {
        let mirror = engine.keyword_mirror();
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.load_pool_entries(&rows);
        guard.reset_docs_from_views(l1_views, l2_views);
        tracing::info!(
            docs = guard.doc_count(),
            pool = guard.pool_len(),
            "关键词镜像已随索引装载"
        );
    }

    // 语义层：锁内取词表 → 锁外构建 → 锁内挂载（避免 std 写锁跨 await）
    let terms: Vec<ramaria_core::keyword::KeywordToken> = {
        let mirror = engine.keyword_mirror();
        let guard = read_recover(&*mirror, "index.keyword_mirror");
        guard
            .pool()
            .established_terms()
            .into_iter()
            .cloned()
            .collect()
    };
    // 嵌入 provider 取快照后在锁外使用（缺失 → 语义层降级为无向量构建）
    let provider = engine.embedding_ref();
    let fuzzy = KeywordService::build_fuzzy(&terms, provider.as_deref()).await;
    {
        let mirror = engine.keyword_mirror();
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.set_fuzzy(fuzzy);
    }
}

// =========================================================
// 增量镜像（L1 生成后）
// =========================================================

/// 把新生成的 L1 同步进检索器与关键词镜像（不重建整库）。
///
/// 行为:
/// - 检索器已加载 → 生成摘要向量（嵌入可用时）后增量索引；
///   未加载 → 跳过（下次懒加载会从存储全量载入，不会漏）。
/// - 关键词镜像 → 倒排增量 + 词典池累积（语义层不随增量重建，保持既有方言）；
///   持久化为 `pending` 的词条不进入内存池——词表与持久化状态保持一致
///   （pending 不算已建立词条，不参与分词字典 / 语义层口径）。
///
/// 降级:
/// - 向量生成失败 → 仅 BM25（记 debug，不阻塞）；
/// - 词条状态查询失败 → 跳过 pending 过滤（按旧口径全部入池）。
pub(crate) async fn index_l1_into_mirrors(engine: &Engine, l1: &MemoryL1) {
    let doc = l1_view(l1);

    // 向量（嵌入可用时；失败/不可用 → None）
    let vector = match engine.embedding_ref() {
        Some(provider) if provider.is_available() => match provider.embed(&l1.summary).await {
            Ok(vector) => Some(vector),
            Err(e) => {
                tracing::debug!(error = %e, "L1 增量向量生成失败，仅入 BM25 索引");
                None
            }
        },
        _ => None,
    };

    // 检索器增量
    {
        let slot = engine.retriever_slot();
        let mut guard = write_recover(&*slot, "index.retriever_slot");
        if let Some(retriever) = guard.as_mut() {
            retriever.index_l1_with_vector(&doc, vector);
            tracing::debug!(l1_id = %l1.id, "L1 已增量加入检索器索引");
        } else {
            // 索引未加载：本次增量进不了内存索引，置脏标记保证下次加载会重建并覆盖该 L1
            // （否则"加载窗口内封存的 L1"在进程生命周期内一直检索不到）
            engine.mark_index_dirty();
            tracing::debug!(
                l1_id = %l1.id,
                "检索索引未加载，已置脏标记（下次加载重建，本次 L1 不会漏）"
            );
        }
    }

    // 关键词镜像增量
    {
        let tokens = CommaSeparatedNormalizer.normalize(doc.keywords.as_deref().unwrap_or(""));
        let now = now_ms();

        // pending 词条不进入内存池（established 词表与持久化状态一致）；
        // 状态查询在拿镜像写锁之前完成，不持锁跨 await。
        let texts: Vec<String> = tokens.iter().map(|t| t.as_str().to_string()).collect();
        let pending_texts: HashSet<String> = match engine
            .storage_ref()
            .list_keyword_statuses(&texts)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter(|(_, alias_status)| alias_status.as_deref() == Some("pending"))
                .map(|(keyword, _)| keyword)
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "词条状态查询失败，pending 过滤跳过（按旧口径入池）");
                HashSet::new()
            }
        };
        let accepted: Vec<ramaria_core::keyword::KeywordToken> = tokens
            .into_iter()
            .filter(|token| !pending_texts.contains(token.as_str()))
            .collect();

        let mirror = engine.keyword_mirror();
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.index_l1(&doc);
        guard.upsert_pool_tokens(&accepted, now);
        // 语义层陈旧可观测（节流 5 分钟，日志不含词条文本）
        guard.warn_if_fuzzy_stale(now);
    }
}

// =========================================================
// 视图转换
// =========================================================

/// `MemoryL1` → 检索视图。
fn l1_view(l1: &MemoryL1) -> L1DocView {
    L1DocView {
        id: l1.id,
        summary: l1.summary.clone(),
        keywords: l1.keywords.clone(),
        persona_uid: l1.persona_uid.clone(),
        created_at: l1.created_at,
        salience: l1.salience,
        last_accessed_at: l1.last_accessed_at,
    }
}

/// `MemoryEvent` → 检索视图。
fn l2_view(event: &ramaria_core::types::MemoryEvent) -> L2DocView {
    L2DocView {
        id: event.id,
        title: event.title.clone(),
        summary: event.summary.clone(),
        keywords: event.keywords.clone(),
        attitude: event.attitude.clone(),
        paraphrase: event.paraphrase.clone(),
        persona_uid: event.persona_uid.clone(),
        share: event.share,
        confidence: event.confidence,
        created_at: event.created_at,
        salience: event.salience,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
