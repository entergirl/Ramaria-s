//! crates/ramaria-memory/src/retriever/index.rs — Retriever 的索引构建与维护
//!
//! 设计特点:
//! - 管理 L1/L2 文档的 BM25 与向量索引写入、删除与全量重建
//! - 内置 LRU 驱逐策略：超过 `lru_max_entries` 时按创建时间同步清理
//! - 仅依赖 `Retriever` 的私有字段与配置开关，不引入新的对外能力

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryL1;

use crate::bm25::{DocId, tokenize_fields};
use crate::vector::{VectorIndex, make_vector_label};

use super::Retriever;
use super::types::{L1DocView, L2DocView};

impl Retriever {
    /// 将 L1 文档添加到所有启用通道的索引中。
    ///
    /// 接受引用以避免调用方不必要的 clone；内部仅在存入 HashMap 时做一次 clone。
    ///
    /// LRU 驱逐: 添加后若总文档数超过 `lru_max_entries`，按 `created_at` 驱逐最旧文档。
    ///
    /// 向量通道说明（接线）:
    /// - 本方法不生成向量（同步路径无 embedding provider），仅 BM25 + 内存文档；
    /// - 向量由调用方在 embedding 可用时通过 [`index_l1_with_vector`] 写入，
    ///   使 L1 文档真实进入向量索引（此前仅 rebuild 全量路径写入）。
    pub fn index_l1(&mut self, doc: &L1DocView) {
        // BM25 索引
        if self.config.enable_bm25 {
            let tokens = tokenize_fields(&[&doc.summary, doc.keywords.as_deref().unwrap_or("")]);
            self.bm25_index.add(DocId::L1(doc.id), tokens);
        }

        self.l1_docs.insert(doc.id, doc.clone());

        // LRU 驱逐: 总文档数超过上限时，从 BM25 和内存 HashMap 中同步清理最早文档
        self.evict_if_needed();
    }

    /// 将 L1 文档连同向量加入索引（L1 embedding 真实入向量索引）。
    ///
    /// 参数:
    /// - `doc`: L1 文档视图。
    /// - `vector`: 文档向量（None = 无 embedding，仅入 BM25/内存，检索走 BM25）。
    ///
    /// 说明:
    /// - label 统一 `make_vector_label("l1", uuid)`（大写 `L1:`，与 `parse_doc_label` 匹配）。
    /// - 向量维度由索引首个条目确定；后续条目维度不一致会被 `BruteForceIndex::add` 拒绝并记 warn。
    pub fn index_l1_with_vector(&mut self, doc: &L1DocView, vector: Option<Vec<f32>>) {
        self.index_l1(doc);
        if let Some(v) = vector {
            self.vector_index.add(
                &make_vector_label("l1", &doc.id.to_string()),
                v,
                doc.created_at,
            );
        }
    }

    /// 将 `MemoryL1` 记录转换为 `L1DocView` 并增量添加到所有启用通道的索引中。
    ///
    /// 职责:
    /// - 供 `SessionLifecycle` 在 L1 摘要生成成功后立即调用，
    ///   确保新生成的 L1 文档无需等待全量 `rebuild_retriever` 即可被 Stage 5 RAG 检索命中。
    /// - 将 `MemoryL1`（来自 `ramaria-core` 的业务类型）转为内部 `L1DocView` 后委托给 [`index_l1`]。
    ///
    /// 参数:
    /// - `record`: 刚生成的 L1 摘要记录。
    ///
    /// 返回:
    /// - `Ok(())`: 索引添加成功（即使 BM25 分词为空也是成功）。
    ///
    /// 说明:
    /// - 本方法总是返回 `Ok(())`——转换和 BM25 索引添加均为纯内存操作，不可失败。
    /// - 向量索引暂不更新（需 EmbeddingProvider 生成 query 向量，由后续 rebuild 路径处理）。
    pub fn index_l1_record(&mut self, record: &MemoryL1) -> RamariaResult<()> {
        let doc = L1DocView {
            id: record.id,
            summary: record.summary.clone(),
            keywords: record.keywords.clone(),
            persona_uid: record.persona_uid.clone(),
            created_at: record.created_at,
            salience: record.salience,
            last_accessed_at: record.last_accessed_at,
        };
        self.index_l1(&doc);
        tracing::info!(
            l1_id = %record.id,
            persona_uid = ?record.persona_uid,
            "L1 记录已增量加入 Retriever 索引"
        );
        Ok(())
    }

    /// 将 L2 事件添加到所有启用通道的索引中。
    ///
    /// 接受引用以避免调用方不必要的 clone；内部仅在存入 HashMap 时做一次 clone。
    /// 同时消除了之前因 borrow checker 限制而产生的临时 String 分配。
    ///
    /// LRU 驱逐: 添加后若总文档数超过 `lru_max_entries`，按 `created_at` 驱逐最旧文档。
    ///
    /// 向量通道说明（接线）:
    /// - 本方法不生成向量（同步路径无 embedding provider），仅 BM25 + 内存文档；
    /// - 向量由调用方在 embedding 可用时通过 [`index_l2_with_vector`] 写入。
    pub fn index_l2(&mut self, doc: &L2DocView) {
        // BM25 索引
        if self.config.enable_bm25 {
            let mut fields: Vec<&str> = vec![&doc.title, &doc.summary];
            if let Some(ref kw) = doc.keywords {
                fields.push(kw.as_str());
            }
            if let Some(ref att) = doc.attitude {
                fields.push(att.as_str());
            }
            if let Some(ref par) = doc.paraphrase {
                fields.push(par.as_str());
            }
            let tokens = tokenize_fields(&fields);
            self.bm25_index.add(DocId::L2(doc.id), tokens);
        }

        self.l2_docs.insert(doc.id, doc.clone());

        // LRU 驱逐
        self.evict_if_needed();
    }

    /// 将 L2 事件连同向量加入索引（L2 embedding 真实入向量索引）。
    ///
    /// 参数:
    /// - `doc`: L2 事件视图。
    /// - `vector`: 事件向量（None = 无 embedding，仅入 BM25/内存）。
    ///
    /// 说明:
    /// - label 统一 `make_vector_label("l2", id)`（大写 `L2:`，与 `parse_doc_label` 匹配）。
    pub fn index_l2_with_vector(&mut self, doc: &L2DocView, vector: Option<Vec<f32>>) {
        self.index_l2(doc);
        if let Some(v) = vector {
            self.vector_index.add(
                &make_vector_label("l2", &doc.id.to_string()),
                v,
                doc.created_at,
            );
        }
    }

    /// 从整个检索器中移除一个 L1 文档（BM25 + HashMap）。
    ///
    /// 用于会话删除、记忆清理等场景，保持内存和索引一致性。
    pub fn remove_l1(&mut self, doc_id: &uuid::Uuid) {
        let bm25_doc_id = DocId::L1(*doc_id);
        self.bm25_index.remove(&bm25_doc_id);
        self.l1_docs.remove(doc_id);
    }

    /// 从整个检索器中移除一个 L2 文档（BM25 + HashMap）。
    ///
    /// 用于事件删除、记忆清理等场景，保持内存和索引一致性。
    pub fn remove_l2(&mut self, doc_id: &i64) {
        let bm25_doc_id = DocId::L2(*doc_id);
        self.bm25_index.remove(&bm25_doc_id);
        self.l2_docs.remove(doc_id);
    }

    /// LRU 驱逐: 当总文档数超过 `lru_max_entries` 时，按 `created_at` 驱逐最早创建的文档。
    ///
    /// 策略:
    /// - 从所有文档中按 created_at 升序排列，移除最早的条目
    /// - 同时从 BM25 索引和 HashMap 中同步删除，保持一致性
    /// - 每次只驱逐超出部分（(l1 + l2) - lru_max_entries 条）
    /// - `lru_max_entries == 0` 时跳过驱逐（无限制模式）
    ///
    /// 复杂度: O(n log n) 其中 n = l1_docs.len + l2_docs.len。
    /// 仅在高文档数且超出上限时触发，性能影响可控。
    fn evict_if_needed(&mut self) {
        if self.lru_max_entries == 0 {
            return; // 无限制模式
        }

        let total = self.l1_docs.len() + self.l2_docs.len();
        let evict_count = total.saturating_sub(self.lru_max_entries);

        if evict_count == 0 {
            return;
        }

        // 收集所有 (doc_id_string, created_at, is_l1, key_L1_uuid, key_L2_i64) 并按时间排序
        let mut entries: Vec<(i64, bool, uuid::Uuid, i64)> = Vec::with_capacity(total);

        for (uid, doc) in self.l1_docs.iter() {
            entries.push((doc.created_at, true, *uid, 0));
        }
        for (id, doc) in self.l2_docs.iter() {
            entries.push((doc.created_at, false, uuid::Uuid::nil(), *id));
        }

        // 按 created_at 升序排列，驱逐最早创建的
        entries.sort_by_key(|e| e.0);

        let to_evict = if evict_count >= entries.len() {
            &entries[..]
        } else {
            &entries[..evict_count]
        };

        // 同步驱逐：BM25 + HashMap
        for (_, is_l1, l1_uid, l2_id) in to_evict {
            if *is_l1 {
                self.bm25_index.remove(&DocId::L1(*l1_uid));
                self.l1_docs.remove(l1_uid);
            } else {
                self.bm25_index.remove(&DocId::L2(*l2_id));
                self.l2_docs.remove(l2_id);
            }
        }

        tracing::warn!(
            l1_remaining = self.l1_docs.len(),
            l2_remaining = self.l2_docs.len(),
            evicted = to_evict.len(),
            "Retriever LRU 驱逐完成——文档数超过容量上限"
        );
    }

    /// 从 BM25 索引中移除一篇文档。
    pub fn remove_from_bm25(&mut self, doc_id: &DocId) {
        self.bm25_index.remove(doc_id);
    }

    /// 重建 BM25 索引。
    ///
    /// 清空现有索引，从 l1_docs 和 l2_docs 重新构建。
    ///
    /// 接线候选：desktop index rebuild 命令（v1.6 核查）
    pub fn rebuild_bm25(&mut self) {
        self.bm25_index.clear();
        let l1_snapshot: Vec<L1DocView> = self.l1_docs.values().cloned().collect();
        let l2_snapshot: Vec<L2DocView> = self.l2_docs.values().cloned().collect();

        for doc in &l1_snapshot {
            self.index_l1(doc);
        }
        for doc in &l2_snapshot {
            self.index_l2(doc);
        }
    }
}
