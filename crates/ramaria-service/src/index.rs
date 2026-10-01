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
/// 2. 锁外收集文档视图（各 persona 的未吸收 L1 / L2 事件 / utt 块 + 无主 L1）；
/// 3. 评估 BM25 词典增强迁移决策（已确认词表 + 分词版本标记）；
/// 4. 临时检索器构建（检索配置 + 词典 + 文档索引 + 可选向量）；
/// 5. 整体替换懒加载槽（读者不读半成品）；
/// 6. 词典增强迁移完成后写回分词版本标记；
/// 7. 关键词镜像装载（词典池 + 倒排文档 + 语义层）；
/// 8. 记录代次快照与构建完成时间，写回索引版本（失败只记日志）。
///
/// 返回:
/// - `Ok(total)`: 构建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
/// - `Err(..)`: 关键读取失败（旧索引保持可用），并置"重建失败"告警位。
///
/// 降级:
/// - persona 事件 / utt 块 / 词典 / 向量读取失败按降级链处理，不阻塞构建；
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

    // ---- 4. 整体替换懒加载槽（此后读者要么见旧索引，要么见新索引）----
    {
        let slot = engine.retriever_slot();
        let mut guard = write_recover(&*slot, "index.retriever_slot");
        *guard = Some(fresh);
    }

    // ---- 5. 词典增强迁移版本写回（索引已是词典增强口径；失败下次重建自动重试）----
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

    // ---- 6. 关键词镜像装载（词典池 + 倒排文档 + 语义层） ----
    sync_keyword_mirror(engine, &l1_views, &l2_views).await;

    // ---- 7. 记录代次快照与构建完成时间 ----
    engine.record_index_stamp(stamp);
    // 记"完成时间"：冷却窗口按两次重建之间的实际间隔计算（含本次构建耗时）
    engine.record_index_build_time(now_ms());

    // ---- 8. 标记索引已构建（判定只看 `== 0`，写 1 表"已构建"）----
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
/// - 关键词镜像 → 倒排增量 + 词典池累积（语义层不随增量重建，保持既有方言）。
///
/// 降级:
/// - 向量生成失败 → 仅 BM25（记 debug，不阻塞）。
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
        let mirror = engine.keyword_mirror();
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.index_l1(&doc);
        guard.upsert_pool_tokens(&tokens, now);
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
mod tests {
    use super::*;
    use crate::test_support::{
        DeterministicEmbedding, MockLlm, engine_with_db, engine_with_failable_storage,
        engine_with_llm_and_config, engine_with_llm_config_and_embedding, seed_l1 as seed_l1_raw,
        seed_persona,
    };
    use crate::types::{RecallLayer, RecallRequest, RecallResult};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::lock::{read_recover, write_recover};
    use ramaria_core::traits::{
        EmbeddingProvider, SETTING_BM25_INDEX_VERSION, StoreCrud, StoreInfrastructure,
    };
    use ramaria_core::types::{Message, MessageRole, MessageSource};
    use ramaria_memory::retriever::{SearchRequest, SearchResult};
    use ramaria_storage::SqliteStorage;
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    /// 造一条 L1（带"工作压力"关键词，便于关键词镜像/BM25 命中）。
    async fn seed_l1(storage: &SqliteStorage, persona: &str, summary: &str) -> Uuid {
        seed_l1_raw(storage, persona, summary, Some("工作压力"), 1_000).await
    }

    /// 以 L1 层检索模式召回一句（测试统一口径：只关心 L1 是否命中）。
    async fn recall_l1(engine: &Engine, query: &str) -> RecallResult {
        engine
            .recall(RecallRequest {
                query: Some(query.to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功")
    }

    /// 直接读取已构建检索器上的检索结果（不经过 recall 的 persona 过滤与镜像通道）。
    fn search_docs(engine: &Engine, query: &str) -> Vec<SearchResult> {
        let slot = engine.retriever_slot();
        let guard = read_recover(&*slot, "index.retriever_slot");
        guard.as_ref().expect("索引应已构建").search(
            &SearchRequest {
                query: query.to_string(),
                persona_uid: None,
                top_k: 10,
                filter_share: false,
            },
            None,
        )
    }

    /// 懒加载：首次构建返回 true，重复调用返回 false；索引可命中。
    #[tokio::test]
    async fn ensure_loaded_builds_once_and_searches() {
        let (engine, storage, dir) = engine_with_db("index").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "用户最近工作压力很大，常常加班").await;

        assert!(engine.ensure_index_loaded().await.expect("加载成功"));
        assert!(!engine.ensure_index_loaded().await.expect("重复加载成功"));
        assert!(engine.is_retriever_loaded());

        let result = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(!result.items.is_empty(), "懒加载后应能检索到 L1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 索引版本写入：首次构建成功后写 `1`（供首次配置状态机判定"索引已构建"）；
    /// 沿用已加载索引的早退分支不写。
    #[tokio::test]
    async fn ensure_loaded_marks_index_version_after_build() {
        let (engine, storage, dir) = engine_with_db("index-version").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "用户最近工作压力很大，常常加班").await;

        // 显式置 0（"尚未构建"），与缺键口径一致
        storage
            .set_index_version(0)
            .await
            .expect("写入索引版本应成功");

        assert!(engine.ensure_index_loaded().await.expect("加载成功"));
        assert_eq!(
            storage
                .get_index_version()
                .await
                .expect("读取索引版本应成功"),
            1,
            "构建完成后应写入索引版本 1"
        );

        // 沿用已加载索引的早退分支不写版本
        storage
            .set_index_version(0)
            .await
            .expect("写入索引版本应成功");
        assert!(!engine.ensure_index_loaded().await.expect("重复加载成功"));
        assert_eq!(
            storage
                .get_index_version()
                .await
                .expect("读取索引版本应成功"),
            0,
            "早退分支不应写入索引版本"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 索引未加载时不报错：召回返回空结果（首次召回会自动加载，此处直接调用共用实现）。
    #[tokio::test]
    async fn recall_without_load_still_succeeds() {
        let (engine, storage, dir) = engine_with_db("index-empty").await;
        seed_persona(&storage, "char-0001").await;

        // 未显式加载 → recall 内部会懒加载（行为：能召回已入库内容）
        let result = engine
            .recall(RecallRequest {
                query: Some("任意".to_string()),
                persona: Some("char-0001".to_string()),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(result.items.is_empty(), "库中无记忆时返回空结果");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 增量镜像：加载后新写入的 L1 经 `index_l1_into_mirrors` 即可被检索命中。
    #[tokio::test]
    async fn incremental_l1_is_searchable() {
        let (engine, storage, dir) = engine_with_db("index-incremental").await;
        seed_persona(&storage, "char-0001").await;
        engine.ensure_index_loaded().await.expect("加载成功");

        // 新 L1：先写库，再走增量镜像（与封存路径一致）
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话");
        let mut l1 = ramaria_core::types::MemoryL1::new(
            session.id,
            "用户这周开始学习攀岩，周末去了岩馆".to_string(),
            None,
        );
        l1.persona_uid = Some("char-0001".to_string());
        l1.keywords = Some("攀岩".to_string());
        storage.save_memory_l1(&l1).await.expect("写入 L1");
        index_l1_into_mirrors(&engine, &l1).await;

        let result = engine
            .recall(RecallRequest {
                query: Some("攀岩".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(
            result.items.iter().any(|i| i.text.contains("攀岩")),
            "增量 L1 应立即可检索: {:?}",
            result.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 索引未加载期间产生的 L1：置脏标记 → 下次加载重建并找回该 L1（不漏检索）。
    #[tokio::test]
    async fn dirty_flag_forces_rebuild_after_incremental_write() {
        let (engine, storage, dir) = engine_with_db("index-dirty").await;
        seed_persona(&storage, "char-0001").await;

        // 未加载就发生增量（模拟"加载窗口内的封存"）
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话");
        let mut l1 = ramaria_core::types::MemoryL1::new(
            session.id,
            "用户这周开始学习攀岩".to_string(),
            None,
        );
        l1.persona_uid = Some("char-0001".to_string());
        l1.keywords = Some("攀岩".to_string());
        storage.save_memory_l1(&l1).await.expect("写入 L1");

        index_l1_into_mirrors(&engine, &l1).await;
        assert!(engine.index_dirty(), "未加载时的增量应置脏");
        assert!(!engine.is_retriever_loaded());

        // 因脏标记，加载必须构建（不能因"已加载"提前返回）→ L1 可检索
        assert!(
            engine.ensure_index_loaded().await.expect("加载成功"),
            "脏标记应触发构建"
        );
        assert!(!engine.index_dirty(), "构建后脏标记应清除");

        let result = engine
            .recall(RecallRequest {
                query: Some("攀岩".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(
            result.items.iter().any(|i| i.text.contains("攀岩")),
            "加载窗口内的 L1 不应漏检索: {:?}",
            result.items
        );

        // 无脏标记 → 不重复构建
        assert!(!engine.ensure_index_loaded().await.expect("重复加载成功"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无主 L1（persona_uid IS NULL）也会被加载（导入数据可检索）。
    #[tokio::test]
    async fn unbound_l1_is_loaded() {
        let (engine, storage, dir) = engine_with_db("index-unbound").await;
        seed_persona(&storage, "char-0001").await;
        // 无主 L1：不绑定 persona（persona_uid 保持 None）
        let session = storage.create_session(None).await.expect("创建会话");
        let l1 = ramaria_core::types::MemoryL1::new(
            session.id,
            "导入的聊天记录提到喜欢喝咖啡".to_string(),
            None,
        );
        storage.save_memory_l1(&l1).await.expect("写入 L1");

        // 会话消息（避免会话被视为空壳；无主 L1 由 unbound 通道加载）
        let message = Message::new(
            session.id,
            MessageRole::User,
            "导入消息".to_string(),
            MessageSource::Local,
        );
        storage.save_message(&message).await.expect("写入消息");

        engine.ensure_index_loaded().await.expect("加载成功");
        let result = engine
            .recall(RecallRequest {
                query: Some("咖啡".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(
            result.items.iter().any(|i| i.text.contains("咖啡")),
            "无主 L1 应可检索: {:?}",
            result.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 代次刷新（跨进程场景）：另一连接写入新 L1 后，本引擎的已加载索引能感知并刷新。
    #[tokio::test]
    async fn cross_process_write_refreshes_index() {
        let (engine, storage, dir) = engine_with_db("index-cross").await;
        seed_persona(&storage, "char-0001").await;
        engine.ensure_index_loaded().await.expect("加载成功");

        // 库内无变化 → 代次一致，不重复构建
        assert!(
            !engine.ensure_index_loaded().await.expect("重复加载成功"),
            "无变化时不应重建索引"
        );

        // 另一"进程"视角：同一库文件上的第二个存储句柄写入新 L1
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("第二连接池应可创建");
        let other_storage = SqliteStorage::new(pool);
        let session = other_storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话");
        let mut l1 = ramaria_core::types::MemoryL1::new(
            session.id,
            "用户最近迷上了夜跑，每周三次".to_string(),
            None,
        );
        l1.persona_uid = Some("char-0001".to_string());
        l1.keywords = Some("夜跑".to_string());
        other_storage.save_memory_l1(&l1).await.expect("写入 L1");

        // 本引擎召回：语料戳变化触发刷新，新记忆必须可见
        let result = recall_l1(&engine, "夜跑").await;
        assert!(
            result.items.iter().any(|i| i.text.contains("夜跑")),
            "跨进程写入的新 L1 应可检索: {:?}",
            result.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 降级链：无嵌入模型时不阻塞索引构建，BM25 / 关键词通道命中，向量通道缺席。
    #[tokio::test]
    async fn no_embedding_degrades_to_non_vector_channels() {
        let (engine, storage, dir) = engine_with_db("index-degraded").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "用户最近开始学习游泳，每周去两次").await;

        // 测试脚手架不注入嵌入模型：向量通道不可用（真实进程对应的"模型缺失"降级场景）
        assert!(
            !engine.is_embedding_available(),
            "无嵌入模型时应走降级链（不阻塞装配与索引构建）"
        );
        assert!(
            engine
                .ensure_index_loaded()
                .await
                .expect("降级路径也应构建成功"),
            "首次召回应完成懒加载构建"
        );
        assert!(engine.is_retriever_loaded());

        let result = recall_l1(&engine, "游泳").await;
        assert!(
            !result.items.is_empty(),
            "嵌入缺失时 BM25 / 关键词镜像应仍能命中: {:?}",
            result.items
        );
        assert_eq!(
            result.stats.channels.get("vector").copied().unwrap_or(0),
            0,
            "无嵌入模型时向量通道不得有命中（统计口径）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 刷新间隔：冷却窗口内检测到跨进程写入不重建（沿用现有索引），窗口过后的下次召回补上。
    #[tokio::test]
    async fn refresh_interval_defers_rebuild_within_cooldown() {
        // 冷却窗口取 1 秒（配置项单位秒；生产默认 0 = 不节流，即时可见）
        let mut config = RamariaConfig::default();
        config.index.refresh_interval_seconds = 1;
        let (engine, storage, dir) =
            engine_with_llm_and_config("index-refresh", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        assert!(
            engine.ensure_index_loaded().await.expect("首次加载成功"),
            "首次召回应完成懒加载构建"
        );
        // 首次懒加载不受冷却约束（构建照常发生），但构建完成后即进入冷却窗口
        assert!(
            !engine.index_rebuild_cooldown_elapsed(),
            "刚构建完成应处于冷却窗口内（窗口从构建完成起算）"
        );

        // 另一"进程"写入新 L1（语料代次变化）：必须在同一库文件的第二个连接上写入，
        // 模拟"桌面写、MCP 读"的跨进程场景
        let db_path = dir.join("assistant.db");
        let other = SqliteStorage::new(
            ramaria_storage::database::init_pool(Some(db_path))
                .await
                .expect("第二连接池应可创建"),
        );
        seed_l1_raw(
            &other,
            "char-0001",
            "用户最近迷上了夜跑",
            Some("夜跑"),
            2_000,
        )
        .await;
        assert!(
            !engine.index_rebuild_cooldown_elapsed(),
            "构建后 1 秒内应处于冷却窗口"
        );

        // 冷却窗口内：不重建 → 沿用现有索引，新 L1 尚不可见
        let within = recall_l1(&engine, "夜跑").await;
        assert!(
            within.items.is_empty(),
            "冷却窗口内应沿用现有索引（抑制重建风暴）: {:?}",
            within.items
        );

        // 窗口过后：下一次召回补上重建 → 新 L1 可见
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(
            engine.index_rebuild_cooldown_elapsed(),
            "超过刷新间隔后应允许重建"
        );
        let after = recall_l1(&engine, "夜跑").await;
        assert!(
            after.items.iter().any(|i| i.text.contains("夜跑")),
            "窗口过后的召回应重建并命中新 L1: {:?}",
            after.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 显式重建加载无主 L1（persona_uid IS NULL）：导入数据重建后进入索引。
    #[tokio::test]
    async fn rebuild_loads_unbound_l1() {
        let (engine, storage, dir) = engine_with_db("index-rebuild-unbound").await;
        seed_persona(&storage, "char-0001").await;
        // 无主 L1：不绑定 persona（persona_uid 保持 None）
        let session = storage.create_session(None).await.expect("创建会话");
        let l1 = ramaria_core::types::MemoryL1::new(
            session.id,
            "用户喜欢喝咖啡，每天上午必点一杯拿铁".to_string(),
            None,
        );
        storage.save_memory_l1(&l1).await.expect("写入 L1");

        let total = engine.rebuild_index().await.expect("重建应成功");
        assert!(total >= 1, "无主 L1 必须被加载进索引，实际 total={total}");
        let slot = engine.retriever_slot();
        let guard = read_recover(&*slot, "index.retriever_slot");
        assert!(
            guard.as_ref().expect("索引应已构建").doc_count() >= 1,
            "检索器 doc_count 应为 ≥1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 显式重建返回 L1 + L2 文档总数，并写回索引版本（供首次配置状态机判定）。
    #[tokio::test]
    async fn rebuild_index_returns_document_total() {
        let (engine, storage, dir) = engine_with_db("index-rebuild-total").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "用户喜欢喝咖啡").await;
        seed_l1(&storage, "char-0001", "用户最近开始学习游泳").await;

        // 显式置 0（"尚未构建"），与缺键口径一致
        storage
            .set_index_version(0)
            .await
            .expect("写入索引版本应成功");

        let total = engine.rebuild_index().await.expect("重建应成功");
        assert_eq!(total, 2, "返回值应为 L1 + L2 视图总数");
        assert_eq!(
            storage
                .get_index_version()
                .await
                .expect("读取索引版本应成功"),
            1,
            "重建完成后应写回索引版本 1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// BM25 词典增强分词迁移：词表就绪后重建切为词典增强口径并写回版本标记。
    ///
    /// 步骤:
    /// 1. 分词版本缺失 + 词表为空 → 重建后不写版本标记；纯 bigram 口径下跨词噪声
    ///    （"作压"）能命中（旧口径基线）；
    /// 2. 注入已确认规范词 → 再次重建：版本标记写为当前版本；整词查询命中；
    ///    跨词噪声不再命中。
    #[tokio::test]
    async fn bm25_dictionary_migration_upgrades_and_removes_noise() {
        let (engine, storage, dir) = engine_with_db("index-bm25-migration").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "最近工作压力很大常常加班").await;

        // 1) 词表为空：不升级版本标记（重建与旧版等价），纯 bigram 口径可检索
        engine.rebuild_index().await.expect("首次重建应成功");
        let setting_before = storage
            .get_setting(SETTING_BM25_INDEX_VERSION)
            .await
            .expect("读取设置应成功");
        assert!(
            setting_before.is_none(),
            "词表为空时不应写入版本标记，实际 {setting_before:?}"
        );
        assert!(
            !search_docs(&engine, "作压").is_empty(),
            "迁移前旧索引（纯 bigram）应可检索：'作压' 噪声命中为旧口径基线"
        );

        // 2) 词表就绪：再次重建触发迁移
        storage
            .upsert_keyword("工作压力")
            .await
            .expect("写入规范词应成功");
        engine.rebuild_index().await.expect("迁移重建应成功");
        let setting_after = storage
            .get_setting(SETTING_BM25_INDEX_VERSION)
            .await
            .expect("读取设置应成功");
        assert_eq!(
            setting_after.as_deref(),
            Some("2"),
            "迁移完成后版本标记应为当前版本 2"
        );
        assert!(
            search_docs(&engine, "工作压力")
                .iter()
                .any(|r| r.doc_summary.contains("工作压力")),
            "词典整词查询应命中文档（索引可检索）"
        );
        assert!(
            search_docs(&engine, "作压").is_empty(),
            "词典口径下跨词噪声 '作压' 不应命中"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重建后关键词镜像与加载文档一致；镜像维护不影响检索器：
    /// 镜像被外部清空不改变检索结果，再次重建恢复（幂等收敛）。
    #[tokio::test]
    async fn rebuild_syncs_keyword_service_mirror_and_preserves_search() {
        let (engine, storage, dir) = engine_with_db("index-mirror").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户喜欢喝咖啡，每天上午必点一杯拿铁",
        )
        .await;
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大，常常加班到深夜",
        )
        .await;

        let total = engine.rebuild_index().await.expect("重建应成功");
        assert!(total >= 2, "应加载 ≥2 条 L1，实际 {total}");

        // 镜像与加载文档一致（doc_count 级）
        let mirror = engine.keyword_mirror();
        {
            let guard = read_recover(&*mirror, "index.keyword_mirror");
            assert_eq!(guard.doc_count(), total, "镜像文档数应与重建加载数一致");
        }

        // 镜像维护不影响既有检索：镜像清空前后 search 结果一致
        let search_summaries = |engine: &Engine| -> Vec<String> {
            search_docs(engine, "工作压力")
                .into_iter()
                .map(|r| r.doc_summary)
                .collect()
        };
        let before = search_summaries(&engine);
        assert!(!before.is_empty(), "对照检索应命中既有 L1");
        {
            let mut guard = write_recover(&*mirror, "index.keyword_mirror");
            guard.clear_docs(); // 模拟镜像被外部误操作清空
        }
        let after = search_summaries(&engine);
        assert_eq!(before, after, "镜像操作不得改变 Retriever 检索结果");

        // 再次重建 → 镜像恢复与视图一致（幂等收敛）
        let total2 = engine.rebuild_index().await.expect("再次重建应成功");
        assert_eq!(total2, total);
        {
            let guard = read_recover(&*mirror, "index.keyword_mirror");
            assert_eq!(guard.doc_count(), total2, "再次重建后镜像文档数应恢复");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 嵌入可用 + 词典非空 → 重建后关键词镜像挂载 Fuzzy 语义层（可用分支）。
    #[tokio::test]
    async fn rebuild_with_embedding_and_pool_mounts_fuzzy() {
        let embedding: Option<Arc<dyn EmbeddingProvider>> =
            Some(Arc::new(DeterministicEmbedding::new()));
        let (engine, storage, dir) = engine_with_llm_config_and_embedding(
            "index-fuzzy",
            MockLlm::local(),
            RamariaConfig::default(),
            embedding,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大，常常加班到深夜",
        )
        .await;
        storage
            .upsert_keyword("工作压力")
            .await
            .expect("写入规范词应成功");

        engine.rebuild_index().await.expect("重建应成功");
        let mirror = engine.keyword_mirror();
        let guard = read_recover(&*mirror, "index.keyword_mirror");
        assert!(guard.pool_len() >= 1, "词典池应装载注入的规范词");
        let fuzzy = guard
            .composite()
            .fuzzy()
            .expect("嵌入可用时应挂载 Fuzzy 层");
        assert!(fuzzy.is_ready());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// core `[retrieval]` 配置经重建真实应用进内存检索器
    /// （RRF 融合参数 + 向量通道开关，默认配置下行为等价）。
    #[tokio::test]
    async fn rebuild_applies_core_retrieval_config() {
        let mut config = RamariaConfig::default();
        config.retrieval.rrf_k = 90;
        config.retrieval.bm25_weight = 0.5;
        config.retrieval.graph_weight = 0.4;
        config.retrieval.enable_vector = false;
        let (engine, storage, dir) =
            engine_with_llm_and_config("index-retrieval-config", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "用户喜欢喝咖啡").await;

        engine.rebuild_index().await.expect("重建应成功");
        let slot = engine.retriever_slot();
        let guard = read_recover(&*slot, "index.retriever_slot");
        let retriever = guard.as_ref().expect("索引应已构建");
        assert!(
            !retriever.config().enable_vector,
            "向量通道开关应随重建应用"
        );
        assert_eq!(retriever.config().rrf.k, 90.0, "RRF 平滑系数应随重建应用");
        assert_eq!(retriever.config().rrf.bm25_weight, 0.5);
        assert_eq!(retriever.config().rrf.graph_weight, 0.4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重建失败（存储读取错误）→ 旧索引保持不变且仍可检索，告警位置位；
    /// 恢复后重建成功 → 告警位复位。
    #[tokio::test]
    async fn rebuild_failure_keeps_old_index_searchable() {
        let (engine, storage, failable, dir) =
            engine_with_failable_storage("index-rebuild-failure").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户喜欢喝咖啡，每天上午必点一杯拿铁",
        )
        .await;

        // 1) 首次重建成功 → 索引可检索、告警位为 false
        engine.rebuild_index().await.expect("首次重建应成功");
        assert!(
            !engine.is_index_rebuild_failed(),
            "重建成功后告警位应为 false"
        );
        let hits_before = search_docs(&engine, "咖啡");
        assert!(!hits_before.is_empty(), "首次重建后应可检索");

        // 2) 注入存储读取失败 → 重建报错、告警位置位
        failable.set_fail_list_personas(true);
        let err = engine
            .rebuild_index()
            .await
            .expect_err("存储读取失败时重建应返回错误");
        assert!(!err.to_string().is_empty(), "错误信息不应为空");
        assert!(engine.is_index_rebuild_failed(), "重建失败应置位告警位");

        // 3) 旧索引原子保留：失败后检索结果与失败前一致（未清空、未半成品）
        let hits_after = search_docs(&engine, "咖啡");
        assert!(!hits_after.is_empty(), "重建失败后旧索引必须仍可检索");
        assert_eq!(hits_before.len(), hits_after.len(), "旧索引文档不应丢失");

        // 4) 恢复后重建成功 → 告警位复位
        failable.set_fail_list_personas(false);
        engine.rebuild_index().await.expect("恢复后重建应成功");
        assert!(!engine.is_index_rebuild_failed(), "重建恢复后告警位应复位");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 构建失败记录：失败置入脱敏原因（保留可诊断关键字、折叠为单行）；恢复成功后清除。
    #[tokio::test]
    async fn build_failure_record_tracks_and_clears() {
        let (engine, storage, failable, dir) =
            engine_with_failable_storage("index-failure-record").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户喜欢喝咖啡，每天上午必点一杯拿铁",
        )
        .await;

        // 首次成功：无失败记录
        engine.rebuild_index().await.expect("首次重建应成功");
        assert!(
            engine.index_build_failure().is_none(),
            "构建成功后不得保留失败记录"
        );

        // 失败：置入脱敏原因（保留可诊断关键字、折叠为单行、时间戳有效）
        failable.set_fail_list_personas(true);
        let err = engine
            .rebuild_index()
            .await
            .expect_err("存储读取失败时重建应报错");
        assert!(!err.to_string().is_empty(), "错误信息不应为空");
        let failure = engine.index_build_failure().expect("失败后应有失败记录");
        assert!(
            failure.reason.contains("list_personas"),
            "原因应保留可诊断信息: {}",
            failure.reason
        );
        assert!(
            !failure.reason.contains('\n') && !failure.reason.contains('\r'),
            "原因应折叠为单行: {}",
            failure.reason
        );
        assert!(failure.at_ms > 0, "记录时间应为有效时间戳");

        // 恢复成功：失败记录清除
        failable.set_fail_list_personas(false);
        engine.rebuild_index().await.expect("恢复后重建应成功");
        assert!(
            engine.index_build_failure().is_none(),
            "恢复后应清除失败记录"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
