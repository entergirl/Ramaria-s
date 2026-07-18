//! rust/crates/ramaria-app/src/app_retriever.rs - 检索器重建模块
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

        // 4. 锁定检索器并批量索引（v1.3 P-3: RwLock::write() 用于索引写入）
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

            // 向量索引
            if embeddings_available {
                for (id, vec, created_at) in &l1_vectors {
                    let label = format!("L1:{}", id);
                    retriever.vector_mut().add(&label, vec.clone(), *created_at);
                }
                for (id, vec, created_at) in &l2_vectors {
                    let label = format!("L2:{}", id);
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
            personas = personas.len(),
            embeddings_available,
            "检索器索引重建完成"
        );
        Ok(total)
    }
}
