//! crates/ramaria-app/src/app_retriever.rs - 检索器重建模块
//!
//! 从 `app.rs` 提取。
//! 职责: 从存储层加载 L1/L2 数据，构建内存检索索引（BM25 + 向量 + 图谱）。

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{BM25_INDEX_VERSION_CURRENT, BM25_INDEX_VERSION_LEGACY};
use ramaria_memory::VectorIndex;
use ramaria_memory::retriever::{L1DocView, L2DocView};

use super::app::App;

impl App {
    /// 从存储层重建检索器索引。
    ///
    /// 说明:
    /// - 加载所有 L1 记忆条目和 L2 事件，转换为视图并索引到 Retriever。
    /// - 如果嵌入模型可用，为文档生成向量索引（向量通道）。
    /// - 此操作会清空现有索引并重建。
    /// - 建议在应用启动和后台定期执行。
    ///
    /// 返回:
    /// - 成功时返回索引的文档总数（L1 + L2）。
    pub async fn rebuild_retriever(&self) -> RamariaResult<usize> {
        // 1. 获取所有 persona
        let personas = self.storage.list_personas().await?;

        // 2. 从存储层收集所有 L1 数据（在锁外执行 I/O）
        let mut all_l1: Vec<L1DocView> = Vec::new();
        let mut all_l2: Vec<L2DocView> = Vec::new();
        let mut all_utt: Vec<ramaria_core::types::UttBlock> = Vec::new();

        for persona in &personas {
            // L1
            let l1_list = self.storage.list_unabsorbed_l1(&persona.uid).await?;
            for l1 in &l1_list {
                all_l1.push(L1DocView {
                    id: l1.id,
                    summary: l1.summary.clone(),
                    keywords: l1.keywords.clone(),
                    salience: l1.salience,
                    created_at: l1.created_at,
                    persona_uid: l1.persona_uid.clone(),
                    last_accessed_at: l1.last_accessed_at,
                });
            }

            // L2 events
            let events = self
                .storage
                .list_events_by_persona(&persona.uid, 0, 1000)
                .await
                .unwrap_or_default();
            for ev in &events {
                all_l2.push(L2DocView {
                    id: ev.id,
                    title: ev.title.clone(),
                    summary: ev.summary.clone(),
                    keywords: ev.keywords.clone(),
                    attitude: ev.attitude.clone(),
                    paraphrase: ev.paraphrase.clone(),
                    persona_uid: ev.persona_uid.clone(),
                    share: ev.share,
                    confidence: ev.confidence,
                    created_at: ev.created_at,
                    salience: ev.salience,
                });
            }

            // utt 话语块（v1.4 原文通道；失败降级记 warn，不阻塞重建）
            match self.storage.list_utt_blocks_by_persona(&persona.uid).await {
                Ok(blocks) => all_utt.extend(blocks),
                Err(e) => {
                    tracing::warn!(persona_uid = %persona.uid, %e, "读取 utt 块失败，跳过该 persona");
                }
            }
        }

        let utt_count = all_utt.len();

        // 2.5 无主 L1（persona_uid IS NULL）：导入产生的 L1 不绑定画像，
        //     但检索侧对 NULL persona 文档不做过滤，任何画像可命中，必须一并加载。
        //     否则导入数据在对话链路中永不可检索。
        match self.storage.list_unabsorbed_l1_unbound().await {
            Ok(unbound) => all_l1.extend(unbound.into_iter().map(|l1| L1DocView {
                id: l1.id,
                summary: l1.summary.clone(),
                keywords: l1.keywords.clone(),
                salience: l1.salience,
                created_at: l1.created_at,
                persona_uid: l1.persona_uid.clone(),
                last_accessed_at: l1.last_accessed_at,
            })),
            Err(e) => {
                tracing::warn!(%e, "读取无主 L1 失败，跳过（导入摘要可能不可检索）");
            }
        }
        let total = all_l1.len() + all_l2.len();

        // 3. 生成向量（如果嵌入模型可用）
        let embeddings_available = self.is_embedding_available();
        let mut l1_vectors: Vec<(uuid::Uuid, Vec<f32>, i64)> = Vec::new();
        let mut l2_vectors: Vec<(i64, Vec<f32>, i64)> = Vec::new();

        if embeddings_available {
            let emb = self.embedding_provider();
            if let Some(ref provider) = emb {
                // 批量生成 L1 摘要向量
                let l1_texts: Vec<&str> = all_l1.iter().map(|d| d.summary.as_str()).collect();
                if !l1_texts.is_empty() {
                    match provider.embed_batch(&l1_texts).await {
                        Ok(vectors) => {
                            for (doc, vec) in all_l1.iter().zip(vectors) {
                                l1_vectors.push((doc.id, vec, doc.created_at));
                            }
                            tracing::info!(count = l1_vectors.len(), "L1 批量向量化完成");
                        }
                        Err(e) => {
                            tracing::warn!(%e, "L1 批量向量化失败，向量通道将不可用");
                        }
                    }
                }

                // 批量生成 L2 标题向量
                let l2_texts: Vec<&str> = all_l2.iter().map(|d| d.title.as_str()).collect();
                if !l2_texts.is_empty() {
                    match provider.embed_batch(&l2_texts).await {
                        Ok(vectors) => {
                            for (doc, vec) in all_l2.iter().zip(vectors) {
                                l2_vectors.push((doc.id, vec, doc.created_at));
                            }
                            tracing::info!(count = l2_vectors.len(), "L2 批量向量化完成");
                        }
                        Err(e) => {
                            tracing::warn!(%e, "L2 批量向量化失败，向量通道将不可用");
                        }
                    }
                }
            }
        }

        // 3.5 BM25 词典增强分词迁移准备（锁外 I/O；任一步失败降级，不阻塞重建）
        let migration = self.prepare_bm25_migration().await;

        // 4. 锁定检索器并批量索引（RwLock::write() 用于索引写入）
        {
            let mut retriever = self.retriever.write().unwrap_or_else(|e| {
                tracing::error!("Retriever lock poisoned during rebuild: {e}");
                e.into_inner()
            });
            retriever.clear();

            // BM25 词典增强：词典加载成功则应用（空词典 = 纯 bigram 等价口径）；
            // 加载失败保留既有分词器（已迁移的索引不因一次读取失败而回退口径）
            if migration.apply_dictionary {
                retriever.set_bm25_dictionary(&migration.dictionary);
            }

            // BM25 + 内存文档索引
            for doc in &all_l1 {
                retriever.index_l1(doc);
            }
            for doc in &all_l2 {
                retriever.index_l2(doc);
            }

            // utt 原文通道（v1.4）：块向量在构建时已生成并存 BLOB，此处直接复用
            // （无向量的块仍入内存文档，检索走子串降级）
            for block in &all_utt {
                retriever.index_utt_block(block);
            }

            // 向量索引（label 统一 make_vector_label，与增量路径一致；
            // parse_doc_label 按 "L1:"/"L2:" 前缀解析，大小写均已兼容）
            if embeddings_available {
                for (id, vec, created_at) in &l1_vectors {
                    let label = ramaria_memory::vector::make_vector_label("l1", &id.to_string());
                    retriever.vector_mut().add(&label, vec.clone(), *created_at);
                }
                for (id, vec, created_at) in &l2_vectors {
                    let label = ramaria_memory::vector::make_vector_label("l2", &id.to_string());
                    retriever.vector_mut().add(&label, vec.clone(), *created_at);
                }
                tracing::info!(
                    l1 = l1_vectors.len(),
                    l2 = l2_vectors.len(),
                    "向量索引构建完成"
                );
            } else {
                tracing::info!("嵌入模型不可用，跳过向量索引");
            }
        } // MutexGuard 在此释放

        // 旧版本 + 词典已就绪 → 词典增强重建完成后升级版本标记（写库失败记 warn，
        // 不阻塞主流程：索引已是词典增强口径，下次重建会自动重试写标记）
        if migration.mark_v2 {
            match self
                .storage
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

        // 5. 关键词服务镜像与重建视图同步（镜像侧增强，不改变既有检索行为）：
        //    装载 keyword_pool 词典池 + 重置倒排文档 + 构建语义层。
        //    任一步失败记 warn 不阻塞重建（服务镜像保持空或旧值，M4 接入前不影响检索）。
        self.sync_keyword_service(&all_l1, &all_l2).await;

        tracing::info!(
            total,
            utt = utt_count,
            personas = personas.len(),
            embeddings_available,
            "检索器索引重建完成"
        );
        Ok(total)
    }

    /// 关键词服务镜像与重建视图同步（词典池装载 + 倒排文档重置 + 语义层重建）。
    ///
    /// 职责:
    /// - 词典池：读取 keyword_pool 全量词条装载为 `KeywordPool`（三态）。
    /// - 倒排文档：把本次重建加载的 L1/L2 视图全量喂给 `CompositeIndex`
    ///   （与 Retriever 同源，保证镜像与加载文档一致）。
    /// - 语义层：embedding 可用时按规范词构建 Fuzzy 索引（锁外构建避免写锁跨 await）。
    ///
    /// 降级（不阻塞重建）:
    /// - keyword_pool 读取失败 → warn，词典池为空（语义层跳过）。
    /// - embedding 不可用 / 构建失败 → Fuzzy 置 None（"精确 + 子串"两层降级）。
    async fn sync_keyword_service(&self, all_l1: &[L1DocView], all_l2: &[L2DocView]) {
        // 1. 读取 keyword_pool 全量词条（锁外 I/O；失败 warn 不阻塞）
        let rows = match self.storage.list_keyword_pool_entries().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "加载 keyword_pool 词条失败，关键词服务词典为空");
                Vec::new()
            }
        };

        // 2. 装载词典 + 重置倒排文档（写锁内同步维护）
        {
            let mut guard = self.keyword_service.write().unwrap_or_else(|e| {
                tracing::error!("keyword_service lock poisoned during rebuild: {e}");
                e.into_inner()
            });
            guard.load_pool_entries(&rows);
            guard.reset_docs_from_views(all_l1, all_l2);
            tracing::info!(
                docs = guard.doc_count(),
                pool = guard.pool_len(),
                "关键词服务镜像已随重建装载"
            );
        }

        // 3. 语义层重建（锁外构建，避免 std 写锁跨 await）
        self.rebuild_keyword_fuzzy().await;
    }

    /// 重建关键词服务语义层（Fuzzy）。
    ///
    /// 流程:
    /// 1. 读锁取规范词（词典池装载已完成）；
    /// 2. 规范词为空 / embedding 不可用 → 保持两层（Fuzzy 已由重置清空）；
    /// 3. 锁外 await 词向量构建；
    /// 4. 写锁挂载或置 None（构建失败静默降级，记 warn 不阻塞重建）。
    async fn rebuild_keyword_fuzzy(&self) {
        let service = self.keyword_service();
        let terms = match service.read() {
            Ok(g) => g.canonical_terms(),
            Err(e) => {
                tracing::warn!("keyword_service lock poisoned during fuzzy rebuild: {e}");
                return;
            }
        };
        if terms.is_empty() {
            return;
        }
        let Some(provider) = self.embedding_provider() else {
            return; // embedding 不可用：Fuzzy 保持 None（两层降级）
        };

        let fuzzy = match ramaria_memory::keyword::FuzzyKeywordIndex::build(
            &terms,
            provider.as_ref(),
        )
        .await
        {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    entry_count = terms.len(),
                    "关键词语义层构建失败（两层降级，不阻塞重建）"
                );
                None
            }
        };
        match service.write() {
            Ok(mut guard) => guard.set_fuzzy(fuzzy),
            Err(e) => {
                tracing::warn!("keyword_service lock poisoned during fuzzy mount: {e}");
            }
        }
    }

    /// 读取 BM25 词典增强分词迁移决策。
    ///
    /// 说明:
    /// - settings 键 `bm25_index_version` 缺失/不可解析视为旧版本（=1）。
    /// - keyword_pool 规范词加载失败 → 记 warn 并返回 `apply_dictionary=false`，
    ///   调用方保留既有的已注入分词器（已迁移索引不因一次读取失败回退口径）。
    /// - 仅当版本为旧版、且词典加载成功且非空时返回 `mark_v2=true`
    ///   （词典为空时重建与旧版等价，无需升级标记）。
    async fn prepare_bm25_migration(&self) -> Bm25MigrationPlan {
        let current_version = match self.storage.get_bm25_index_version().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "读取 BM25 分词版本失败，按旧版本处理并尝试迁移");
                BM25_INDEX_VERSION_LEGACY
            }
        };

        let dictionary = match self.storage.list_canonical_keywords().await {
            Ok(kws) => kws,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "加载 keyword_pool 规范词失败，本轮重建保留既有分词口径"
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
}

/// BM25 词典增强分词迁移计划（`rebuild_retriever` 内部使用）。
struct Bm25MigrationPlan {
    /// 词典词条（空 = 纯 bigram 口径）
    dictionary: Vec<String>,
    /// 是否把词典应用到本次重建（词典加载失败时为 false → 保留既有分词器）
    apply_dictionary: bool,
    /// 重建成功后是否写入 `bm25_index_version = 当前版本`
    mark_v2: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::traits::StoreInfrastructure;
    use ramaria_core::types::MemoryL1;
    use ramaria_memory::retriever::{SearchRequest, SearchResult};
    use std::sync::Arc;
    use uuid::Uuid;

    /// 构造一个 L1 记录（persona_uid 可变）。
    fn make_l1(persona_uid: Option<String>, summary: &str) -> MemoryL1 {
        MemoryL1 {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            summary: summary.to_string(),
            keywords: Some(summary.split_whitespace().map(|s| s.to_string()).collect()),
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 1.0,
            absorbed: false,
            created_at: 1_700_000_000_000,
            last_accessed_at: None,
            persona_uid,
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        }
    }

    /// 无主 L1（persona_uid IS NULL）必须被 rebuild_retriever 加载并可检索。
    ///
    /// 验证: 导入产生的 L1 不绑定画像（0/None），但 rebuild 后仍进入检索索引
    /// （否则导入数据在对话链路中永不可检索）。
    #[tokio::test]
    async fn rebuild_loads_unbound_l1() {
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        // 无主 L1：用空字符串键预填充（mock 的 unbound 查询读取该键）
        storage.add_l1_summaries(
            "",
            vec![make_l1(None, "用户喜欢喝咖啡，每天上午必点一杯拿铁")],
        );

        let llm = crate::stages::test_utils::MockLlm::local();
        let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
        let config = ramaria_core::config::RamariaConfig::default();
        let app = App::new_without_embedding(
            storage as Arc<dyn ramaria_core::traits::StorageBackend>,
            Arc::new(llm),
            config,
            keychain,
        );

        let total = app.rebuild_retriever().await.unwrap();
        assert!(total >= 1, "无主 L1 必须被加载进索引，实际 total={total}");
        // 检索器中的文档数应 ≥1（无主 L1 已入索引）
        let guard = app.retriever.read().unwrap_or_else(|e| e.into_inner());
        assert!(guard.doc_count() >= 1, "检索器 doc_count 应为 ≥1");
    }

    /// BM25 词典增强分词迁移：旧版本 + 词典注入 → 重建升级版本标记、口径切为词典增强。
    ///
    /// 验证步骤（分步模拟迁移前后，覆盖"重建期可检索"验收）:
    /// 1. 版本缺失（视为旧版）+ 词典为空 → rebuild 后不写版本；旧索引（纯 bigram）
    ///    仍可检索（噪声查询 "作压" 命中）——重建前旧口径可用。
    /// 2. 注入规范词 "工作压力" → 再次 rebuild：版本写入 2；索引可检索；
    ///    跨词噪声 "作压" 不再命中（token 口径为词典增强）。
    #[tokio::test]
    async fn bm25_dictionary_migration_upgrades_and_removes_noise() {
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        storage.add_l1_summaries("", vec![make_l1(None, "最近工作压力很大常常加班")]);

        let llm = crate::stages::test_utils::MockLlm::local();
        let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
        let config = ramaria_core::config::RamariaConfig::default();
        let app = App::new_without_embedding(
            storage.clone() as Arc<dyn ramaria_core::traits::StorageBackend>,
            Arc::new(llm),
            config,
            keychain,
        );

        let search = |app: &App, query: &str| -> Vec<SearchResult> {
            let guard = app.retriever.read().unwrap_or_else(|e| e.into_inner());
            guard.search(
                &SearchRequest {
                    query: query.to_string(),
                    persona_uid: None,
                    top_k: 10,
                    filter_share: false,
                },
                None,
            )
        };

        // 1) 迁移前：词典为空、版本缺失 → 不升级；纯 bigram 旧口径可检索
        app.rebuild_retriever().await.unwrap();
        let setting_before = storage
            .get_setting(ramaria_core::traits::SETTING_BM25_INDEX_VERSION)
            .await
            .unwrap();
        assert!(
            setting_before.is_none(),
            "词典为空时不应写入版本标记，实际 {setting_before:?}"
        );
        assert!(
            !search(&app, "作压").is_empty(),
            "迁移前旧索引（纯 bigram）应可检索：'作压' 噪声命中为旧口径基线"
        );

        // 2) 词典就绪 → 再次 rebuild 触发迁移
        storage.seed_canonical_keyword("工作压力");
        app.rebuild_retriever().await.unwrap();
        let setting_after = storage
            .get_setting(ramaria_core::traits::SETTING_BM25_INDEX_VERSION)
            .await
            .unwrap();
        assert_eq!(
            setting_after.as_deref(),
            Some("2"),
            "迁移完成后版本标记应为当前版本 2"
        );
        assert!(
            search(&app, "工作压力")
                .iter()
                .any(|r| r.doc_summary.contains("工作压力")),
            "词典整词查询应命中文档（索引可检索）"
        );
        assert!(
            search(&app, "作压").is_empty(),
            "词典口径下跨词噪声 '作压' 不应命中"
        );
    }

    /// 重建后关键词服务镜像与加载文档一致（计数级断言）+ embedding 不可用时 Fuzzy 为 None。
    ///
    /// 验证:
    /// - rebuild 装载 L1/L2 视图后，KeywordService 镜像 doc_count 与加载总数一致。
    /// - 镜像维护不影响 Retriever 检索结果（对照搜索仍可命中）。
    /// - 重复 rebuild 幂等：镜像文档数保持与最新视图一致。
    #[tokio::test]
    async fn rebuild_syncs_keyword_service_mirror_and_preserves_search() {
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        // 两条无主 L1（rebuild 经 unbound 通道加载；keywords 含中文逗号便于镜像解析）
        storage.add_l1_summaries(
            "",
            vec![
                make_l1(None, "用户喜欢喝咖啡，每天上午必点一杯拿铁"),
                make_l1(None, "用户最近工作压力很大，常常加班到深夜"),
            ],
        );

        let llm = crate::stages::test_utils::MockLlm::local();
        let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
        let config = ramaria_core::config::RamariaConfig::default();
        let app = App::new_without_embedding(
            storage as Arc<dyn ramaria_core::traits::StorageBackend>,
            Arc::new(llm),
            config,
            keychain,
        );

        let total = app.rebuild_retriever().await.unwrap();
        assert!(total >= 2, "应加载 ≥2 条无主 L1，实际 {total}");

        // 服务镜像与加载文档一致（doc_count 级）
        let service = app.keyword_service();
        {
            let guard = service.read().unwrap_or_else(|e| e.into_inner());
            assert_eq!(guard.doc_count(), total, "镜像文档数应与重建加载数一致");
            assert!(
                guard.composite().fuzzy().is_none(),
                "embedding 不可用时 Fuzzy 层应为 None（两层降级）"
            );
        }

        // 镜像维护不影响既有检索：镜像操作前后 search 结果一致
        let search = |app: &App| -> Vec<String> {
            let guard = app.retriever.read().unwrap_or_else(|e| e.into_inner());
            guard
                .search(
                    &SearchRequest {
                        query: "工作压力".to_string(),
                        persona_uid: None,
                        top_k: 10,
                        filter_share: false,
                    },
                    None,
                )
                .into_iter()
                .map(|r| r.doc_summary.clone())
                .collect()
        };
        let before = search(&app);
        assert!(!before.is_empty(), "对照搜索应命中既有 L1");
        {
            let mut guard = service.write().unwrap_or_else(|e| e.into_inner());
            guard.clear_docs(); // 模拟镜像被外部误操作清空
        }
        let after = search(&app);
        assert_eq!(before, after, "镜像操作不得改变 Retriever 检索结果");

        // 再次 rebuild → 镜像恢复与视图一致（幂等收敛）
        let total2 = app.rebuild_retriever().await.unwrap();
        assert_eq!(total2, total);
        {
            let guard = service.read().unwrap_or_else(|e| e.into_inner());
            assert_eq!(guard.doc_count(), total2);
        }
    }

    /// embedding 可用 + 词典非空 → rebuild 后关键词服务挂载 Fuzzy 层（可用分支）。
    #[tokio::test]
    async fn rebuild_with_embedding_and_pool_mounts_fuzzy() {
        use crate::stages::test_utils::MockEmbedding;

        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        storage.add_l1_summaries(
            "",
            vec![make_l1(None, "用户最近工作压力很大，常常加班到深夜")],
        );
        storage.seed_canonical_keyword("工作压力");

        let llm = crate::stages::test_utils::MockLlm::local();
        let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
        let config = ramaria_core::config::RamariaConfig::default();
        let embedding: Option<Arc<dyn ramaria_core::traits::EmbeddingProvider>> =
            Some(Arc::new(MockEmbedding::new()));
        let app = App::new(
            storage as Arc<dyn ramaria_core::traits::StorageBackend>,
            Arc::new(llm),
            embedding,
            config,
            keychain,
        );

        app.rebuild_retriever().await.unwrap();
        let service = app.keyword_service();
        let guard = service.read().unwrap_or_else(|e| e.into_inner());
        assert!(guard.pool_len() >= 1, "词典池应装载注入的规范词");
        let fuzzy = guard
            .composite()
            .fuzzy()
            .expect("embedding 可用时应挂载 Fuzzy 层");
        assert!(fuzzy.is_ready());
    }
}
