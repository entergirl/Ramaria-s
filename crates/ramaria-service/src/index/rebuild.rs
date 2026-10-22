//! crates/ramaria-service/src/index/rebuild.rs - Ramaria 索引重建与槽位替换
//!
//! 设计特点:
//! - 完整重建路径：视图收集 → 向量规划 → 临时实例构建 → 整体替换
//! - 失败时保留上一次成功构建的规模与模型，只置告警位与脱敏失败原因
//! - BM25 词典增强迁移在替换成功后写回分词版本标记，失败由下次重建重试
//! - 关键词镜像装载与缺失向量补算都在替换之后进行，构建在途不影响旧索引

use std::collections::HashMap;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::{BM25_INDEX_VERSION_CURRENT, BM25_INDEX_VERSION_LEGACY};
use ramaria_core::types::{IndexStatus, VECTOR_DOC_L1, VECTOR_DOC_L2, VECTOR_DOC_UTT, now_ms};
use ramaria_memory::keyword::service::KeywordService;
use ramaria_memory::retriever::{L1DocView, L2DocView, Retriever, RetrieverConfig};

use crate::engine::Engine;

use super::build::{build_graph_data, load_persona_events};
use super::status::read_stamp;
use super::vectors;
use super::view::{l1_view, l2_view};

/// 构建新索引并整体替换懒加载槽（懒加载与显式重建共用的完整构建路径）。
///
/// 流程:
/// 1. 读取库内代次快照（构建前读取，构建完成后记录供下次比对）；
/// 2. 锁外收集文档视图（各 persona 的未吸收 L1 / L2 事件 / utt 块 / 事件关系 + 无主 L1）；
/// 3. 向量规划：开关 / provider / 模型指纹通过后按指纹读取已落库向量并校验维度；
/// 4. 评估 BM25 词典增强迁移决策（已确认词表 + 分词版本标记）；
/// 5. 临时检索器构建（检索配置 + 词典 + 文档索引 + 已加载向量）；
/// 6. 图谱构建（事件为实体节点、事件关系为边，装入同一临时实例）；
/// 7. 整体替换懒加载槽并标记构建中（此后读者要么见旧索引，要么见新索引）；
/// 8. 词典增强迁移完成后写回分词版本标记；
/// 9. 关键词镜像装载（词典池 + 倒排文档 + 语义层）；
/// 10. 缺失向量分批补算（每批推进进度）；
/// 11. 记录代次快照与构建完成时间，状态置就绪。
///
/// 返回:
/// - `Ok(total)`: 构建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
/// - `Err(..)`: 关键读取失败（旧索引保持可用），状态置降级并置"重建失败"告警位。
///
/// 降级:
/// - persona 事件 / utt 块 / 事件关系 / 词典 / 向量读取失败按降级链处理，不阻塞构建；
/// - 成功完成后复位"重建失败"告警位与状态失败原因（供诊断展示与宿主告警）。
pub(super) async fn build_and_swap(engine: &Engine) -> RamariaResult<usize> {
    // 失败时保留"最近一次成功构建"的规模与模型：构建入口会把状态切到 Building/0/0，
    // 若直接复用当前快照，降级视图会显示加载 0 条并丢失可用索引的真实规模。
    let previous = engine.index_status();
    match build_and_swap_inner(engine).await {
        Ok(total) => {
            engine.set_index_rebuild_failed(false);
            engine.clear_index_build_failure();
            engine.clear_index_status_error();
            Ok(total)
        }
        Err(e) => {
            // 旧索引仍完整可用：置位告警位供宿主提示"记忆注入可能不完整"，
            // 并记录脱敏失败原因供诊断导出（不改变返回错误与降级语义）
            let reason = redact_failure_reason(&e);
            engine.set_index_rebuild_failed(true);
            engine.record_index_build_failure(reason.clone());
            let recoverable = previous.status == IndexStatus::Ready;
            let (loaded, total, failed, model) = if recoverable {
                (
                    previous.loaded,
                    previous.total,
                    previous.failed,
                    previous.model,
                )
            } else {
                (0, 0, 0, None)
            };
            engine.set_index_status(
                IndexStatus::Degraded,
                loaded,
                total,
                failed,
                model,
                Some(reason),
            );
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

    // ---- 1. 视图收集（锁外 I/O）：起点先切 Building，首次构建的收集期也读作"构建中" ----
    engine.set_index_status(IndexStatus::Building, 0, 0, 0, None, None);
    let mut l1_views: Vec<L1DocView> = Vec::new();
    let mut l2_views: Vec<L2DocView> = Vec::new();
    let mut utt_blocks: Vec<ramaria_core::types::UttBlock> = Vec::new();
    let mut event_relations: Vec<ramaria_core::types::EventRelation> = Vec::new();

    for persona in storage.list_personas().await? {
        for l1 in storage.list_unabsorbed_l1(&persona.uid).await? {
            l1_views.push(l1_view(&l1));
        }
        // L2 事件：按 `[index].l2_load_limit` 决定全量分页 / 显式截断；
        // 失败降级为空（单个 persona 的事件读取失败不阻塞整体构建）
        match load_persona_events(engine, storage, &persona.uid).await {
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

    // ---- 2. 向量规划：开关 / provider / 指纹任一不满足即走无向量路径 ----
    let vector_plan = vectors::VectorPlan::detect(engine);
    let vector_docs_total = if vector_plan.is_some() {
        l1_views.len() + l2_views.len() + utt_blocks.len()
    } else {
        0
    };
    let mut vector_map: HashMap<vectors::VectorKey, Vec<f32>> = HashMap::new();
    let mut missing: Vec<vectors::MissingVector> = Vec::new();
    if let Some(plan) = &vector_plan {
        match storage.load_memory_vectors(&plan.fingerprint).await {
            Ok(rows) => vector_map = vectors::load(rows, plan.dimension),
            Err(e) => {
                tracing::warn!(error = %e, "读取记忆向量失败，本次按缺失口径补算");
            }
        }
        missing = vectors::collect_missing(&l1_views, &l2_views, &utt_blocks, &vector_map);
    }
    let mut loaded = vector_docs_total.saturating_sub(missing.len());
    let model_label = vector_plan.as_ref().map(|plan| plan.model_id.clone());

    // ---- 3. BM25 词典增强迁移决策（词表 + 分词版本标记） ----
    let migration = prepare_bm25_migration(engine).await;

    // ---- 4. 临时实例构建（检索配置 + 词典 + 文档索引 + 可选向量） ----
    let mut fresh = Retriever::new();
    *fresh.config_mut() = RetrieverConfig::from_retrieval_config(&engine.config().retrieval);
    if migration.apply_dictionary {
        // 空词典 = 纯 bigram 等价口径：重建总是显式注入，保证口径可预期
        fresh.set_bm25_dictionary(&migration.dictionary);
    }
    for doc in &l1_views {
        fresh.index_l1_with_vector(
            doc,
            vectors::lookup(&vector_map, VECTOR_DOC_L1, &doc.id.to_string()),
        );
    }
    for doc in &l2_views {
        fresh.index_l2_with_vector(
            doc,
            vectors::lookup(&vector_map, VECTOR_DOC_L2, &doc.id.to_string()),
        );
    }
    for block in &utt_blocks {
        fresh.index_utt_block(
            block,
            vectors::lookup(&vector_map, VECTOR_DOC_UTT, &block.id.to_string()),
        );
    }

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

    // ---- 8. 缺失向量批补算（每批后推进进度；单条失败不拖垮批次） ----
    let mut failed = 0usize;
    if let Some(plan) = &vector_plan {
        if !missing.is_empty() {
            let (backfilled, failed_count) = vectors::backfill(
                engine,
                plan,
                &missing,
                &mut loaded,
                vector_docs_total,
                started,
            )
            .await;
            failed = failed_count;
            tracing::info!(
                missing = missing.len(),
                backfilled,
                failed = failed_count,
                "缺失向量补算完成"
            );
        }
    }

    // ---- 9. 记录代次快照与构建完成时间 ----
    engine.record_index_stamp(stamp);
    // 记"完成时间"：冷却窗口按两次重建之间的实际间隔计算（含本次构建耗时）
    engine.record_index_build_time(now_ms());

    tracing::info!(
        l1 = l1_views.len(),
        l2 = l2_views.len(),
        utt = utt_blocks.len(),
        graph_nodes = graph_nodes.len(),
        graph_edges = graph_edges.len(),
        vectors = loaded,
        vector_docs = vector_docs_total,
        elapsed_ms = now_ms().saturating_sub(started),
        bm25_version = stamp.bm25_version,
        "检索索引已加载（共 {total} 条文档）"
    );
    engine.set_index_progress(
        IndexStatus::Ready,
        loaded,
        vector_docs_total,
        failed,
        model_label,
        now_ms().saturating_sub(started) as u64,
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
    // 语义层 provider：开关关闭时不传 provider（零权重加载、零嵌入调用），
    // 关键词通道退化为字面 + 别名归一；文档向量通道由 enable_vector 单独控制。
    let provider = if engine.config().retrieval.enable_keyword_fuzzy {
        engine.embedding_ref()
    } else {
        tracing::debug!("关键词语义层已关闭，词条不做嵌入");
        None
    };
    let fuzzy = KeywordService::build_fuzzy(&terms, provider.as_deref()).await;
    {
        let mirror = engine.keyword_mirror();
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.set_fuzzy(fuzzy);
    }
}
