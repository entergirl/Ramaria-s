//! crates/ramaria-app/src/app_retriever.rs - 检索器重建模块
//!
//! 从 `app.rs` 提取。
//! 职责: 从存储层加载 L1/L2 数据，构建内存检索索引（BM25 + 向量 + 图谱）。

use ramaria_core::error::RamariaResult;
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

        // 4. 锁定检索器并批量索引（RwLock::write() 用于索引写入）
        {
            let mut retriever = self.retriever.write().unwrap_or_else(|e| {
                tracing::error!("Retriever lock poisoned during rebuild: {e}");
                e.into_inner()
            });
            retriever.clear();

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

        tracing::info!(
            total,
            utt = utt_count,
            personas = personas.len(),
            embeddings_available,
            "检索器索引重建完成"
        );
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::MemoryL1;
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
}
