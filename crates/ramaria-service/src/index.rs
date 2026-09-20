//! crates/ramaria-service/src/index.rs - 检索索引懒加载与增量镜像
//!
//! 设计特点:
//! - 懒加载：首次召回前构建一次（进程启动不为大库付加载代价），构建完成后整体替换
//! - 原子替换：新索引在临时实例上完整构建，成功后一次写入替换；读者要么见旧索引，
//!   要么见新索引，不会读到半成品
//! - 数据来源与在线管线一致：各 persona 的未吸收 L1 + L2 事件 + utt 块，另加无主 L1
//! - 降级链：嵌入模型缺失 / 批量向量化失败 → 仅 BM25 + 关键词镜像（不阻塞构建）；
//!   关键词词表读取失败 → 空词典（纯 bigram 口径，行为可预期）
//! - 增量镜像：L1 生成后同步进检索器与关键词镜像（不重建整库）
//! - 代次刷新：召回前比对库内语料戳与 BM25 分词代次，其他进程写入 / 词典升级后
//!   重建内存索引（同进程脏标记保留为加载窗口内的兜底）
//! - 边界：本模块只做"内存索引维护"，不写数据库（除 L2/L3 无关的索引版本标记外）

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::IndexCorpusStamp;
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
            ramaria_core::traits::BM25_INDEX_VERSION_LEGACY
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
// 懒加载
// =========================================================

/// 确保检索索引已加载且与库内语料同代（懒加载 + 代次刷新）。
///
/// 流程:
/// 1. 读取库内代次快照（语料统计 + BM25 分词代次）；
/// 2. 已加载、无脏标记、代次一致 → 直接返回 false；
/// 3. 锁外收集文档视图（各 persona 的 L1/L2/utt + 无主 L1）；
/// 4. 构建临时检索器（BM25 词典 + 文档索引 + 可选向量）；
/// 5. 整体替换懒加载槽并记录代次；
/// 6. 关键词镜像装载（词典池 + 倒排文档 + 可选语义层）。
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
/// - 代次协同（跨进程）：记录的是构建前读取的快照；构建窗口内其他进程的新写入
///   会让下次比对不等 → 再刷新一次（收敛，不漏新记忆）。
pub(crate) async fn ensure_loaded(engine: &Engine) -> RamariaResult<bool> {
    let stamp = read_stamp(engine).await;

    // 已加载且无脏标记 → 只需确认代次是否仍一致；一致则免构建
    if engine.is_retriever_loaded() && !engine.index_dirty() {
        if engine.index_stamp() == Some(stamp) {
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

    let storage = engine.storage_ref().as_ref();
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

    // ---- 2. 临时实例构建（含 BM25 词典） ----
    let mut fresh = Retriever::new();
    *fresh.config_mut() = RetrieverConfig::from_retrieval_config(&engine.config().retrieval);
    match storage.list_established_keywords().await {
        Ok(dictionary) => {
            if !dictionary.is_empty() {
                fresh.set_bm25_dictionary(&dictionary);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "加载 BM25 词典失败，本次按纯 bigram 口径构建");
        }
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

    // ---- 3. 向量索引（嵌入可用时批量生成；失败降级为仅 BM25 + 关键词） ----
    let vectors_built = build_vectors(engine, &mut fresh, &l1_views, &l2_views).await;

    // ---- 4. 整体替换懒加载槽 ----
    {
        let slot = engine.retriever_slot();
        let mut guard = write_recover(slot, "index.retriever_slot");
        *guard = Some(fresh);
    }

    // ---- 5. 关键词镜像装载（词典池 + 倒排文档 + 语义层） ----
    sync_keyword_mirror(engine, &l1_views, &l2_views).await;

    // ---- 6. 记录代次快照（构建前读取；构建窗口内的新写入会在下次比对时触发再刷新）----
    engine.record_index_stamp(stamp);

    tracing::info!(
        l1 = l1_views.len(),
        l2 = l2_views.len(),
        utt = utt_blocks.len(),
        vectors = vectors_built,
        elapsed_ms = now_ms().saturating_sub(started),
        bm25_version = stamp.bm25_version,
        "检索索引已加载（共 {total} 条文档）"
    );
    Ok(true)
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
        let mirror = engine.keyword_mirror_ref();
        let mut guard = write_recover(mirror, "index.keyword_mirror");
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
        let mirror = engine.keyword_mirror_ref();
        let guard = read_recover(mirror, "index.keyword_mirror");
        guard
            .pool()
            .established_terms()
            .into_iter()
            .cloned()
            .collect()
    };
    let provider = engine.embedding_ref();
    let fuzzy = KeywordService::build_fuzzy(&terms, provider.map(|e| e.as_ref())).await;
    {
        let mirror = engine.keyword_mirror_ref();
        let mut guard = write_recover(mirror, "index.keyword_mirror");
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
        let mut guard = write_recover(slot, "index.retriever_slot");
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
        let mirror = engine.keyword_mirror_ref();
        let mut guard = write_recover(mirror, "index.keyword_mirror");
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
    use crate::test_support::{engine_with_db, seed_l1 as seed_l1_raw, seed_persona};
    use crate::types::{RecallLayer, RecallRequest};
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{Message, MessageRole, MessageSource};
    use ramaria_storage::SqliteStorage;
    use uuid::Uuid;

    /// 造一条 L1（带"工作压力"关键词，便于关键词镜像/BM25 命中）。
    async fn seed_l1(storage: &SqliteStorage, persona: &str, summary: &str) -> Uuid {
        seed_l1_raw(storage, persona, summary, Some("工作压力"), 1_000).await
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
        let result = engine
            .recall(RecallRequest {
                query: Some("夜跑".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回成功");
        assert!(
            result.items.iter().any(|i| i.text.contains("夜跑")),
            "跨进程写入的新 L1 应可检索: {:?}",
            result.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
