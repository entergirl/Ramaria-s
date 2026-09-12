//! crates/ramaria-memory/src/retriever/utt.rs — Retriever 的 utt 原文检索通道
//!
//! 设计特点:
//! - 管理 utt 话语块的索引写入/移除/查询（原文高敏感数据通道）
//! - 检索按 `persona_uid` 严格隔离，不做跨 persona 共享
//! - 向量优先，无向量时退化为 BM25 分词子串命中计数打分
//! - utt 块按 `utt_max_entries` 参与容量治理，超出上限的 110% 时驱逐最旧块

use crate::bm25::tokenize;
use crate::utt::decode_embedding;
use crate::vector::{VectorHit, VectorIndex, make_vector_label, parse_vector_label};

use super::Retriever;
use super::index::EVICT_SLACK_DIVISOR;
use super::types::{UttDocView, UttHit};

impl Retriever {
    // =========================================================
    // utt 原文通道（v1.4）
    // =========================================================

    /// 将 utt 块视图加入索引（内存文档 + 可选向量）。
    ///
    /// 向量 label 格式: `L0:{utt_block_id}`（与 L1:/L2: 前缀共存于 BruteForceIndex）。
    /// 现有三通道检索解析不到 `L0:` 前缀时自然跳过，互不干扰。
    ///
    /// 容量治理: 插入后检查 `utt_max_entries`，超限时驱逐最旧块（含 `L0:{id}` 向量）。
    ///
    /// 参数:
    /// - `doc`: 块视图。
    /// - `vector`: 可选的块向量（None 表示无 embedding，检索走子串降级）。
    pub fn index_utt(&mut self, doc: &UttDocView, vector: Option<Vec<f32>>) {
        self.utt_docs.insert(doc.id, doc.clone());
        if let Some(v) = vector {
            self.vector_index.add(
                &make_vector_label("l0", &doc.id.to_string()),
                v,
                doc.created_at,
            );
        }
        self.evict_utt_if_needed();
    }

    /// 从存储层 `UttBlock` 直接索引（解码 f32 BLOB 向量）。
    ///
    /// 说明:
    /// - embedding BLOB 解码失败（数据损坏）→ 记 warn，仅入内存文档（子串降级可用）。
    pub fn index_utt_block(&mut self, block: &ramaria_core::types::UttBlock) {
        let doc = UttDocView {
            id: block.id,
            persona_uid: block.persona_uid.clone(),
            session_id: block.session_id,
            block_text: block.block_text.clone(),
            msg_count: block.msg_count,
            created_at: block.created_at,
        };
        let vector = match block.embedding.as_deref() {
            Some(blob) => match decode_embedding(blob) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(block_id = block.id, %e, "utt 块 embedding 解码失败，仅入内存文档");
                    None
                }
            },
            None => None,
        };
        self.index_utt(&doc, vector);
    }

    /// 从索引移除一个 utt 块（内存文档 + 向量）；供容量驱逐与会话/记忆清理调用。
    pub fn remove_utt(&mut self, id: i64) {
        self.utt_docs.remove(&id);
        self.vector_index
            .remove(&make_vector_label("l0", &id.to_string()));
    }

    /// utt 通道容量驱逐：超过上限的 110% 时按创建时间淘汰最旧块（内存视图 + `L0:{id}` 向量一并清理）。
    ///
    /// 说明:
    /// - 与 L1/L2 驱逐同口径：超过上限 110% 才触发，一次驱逐"超出上限"的全部条目；
    /// - 清理统一走 [`Self::remove_utt`]，保证内存视图与向量索引一致。
    fn evict_utt_if_needed(&mut self) {
        if self.utt_max_entries == 0 {
            return; // 无限制模式
        }

        // 触发水位：超过上限的 110%
        let trigger = self
            .utt_max_entries
            .saturating_add(self.utt_max_entries / EVICT_SLACK_DIVISOR);
        let total = self.utt_docs.len();
        if total <= trigger {
            return;
        }

        let excess = total.saturating_sub(self.utt_max_entries);
        let mut entries: Vec<(i64, i64)> = self
            .utt_docs
            .values()
            .map(|doc| (doc.created_at, doc.id))
            .collect();
        entries.sort_by_key(|e| e.0);
        entries.truncate(excess);

        for (_, id) in entries {
            self.remove_utt(id);
        }

        tracing::warn!(
            evicted = excess,
            remaining = self.utt_docs.len(),
            "utt 原文块超出容量上限，已驱逐最旧块"
        );
    }

    /// 当前内存中的 utt 块数量。
    pub fn utt_doc_count(&self) -> usize {
        self.utt_docs.len()
    }

    /// 检索 utt 原文块（v1.4 原文通道）。
    ///
    /// 通道与降级:
    /// - 向量优先：`query_vec` 可用时在 BruteForceIndex 的 `L0:` label 上检索。
    /// - 子串降级：无向量 / 向量索引空 / 维度不符时，按 query 分词 token
    ///   在块文本中的出现次数打分（BM25 子串匹配）。
    /// - 两通道均无命中 → 空列表（等同 v1.3，不注入原文）。
    ///
    /// 安全约束:
    /// - `persona_uid` 为 None（未指定目标）→ 恒返回空（原文严格按 persona 隔离）。
    /// - 仅返回 `persona_uid` 精确匹配的块，不做跨 persona 共享。
    ///
    /// 参数:
    /// - `query`: 查询文本（子串降级用）。
    /// - `query_vec`: 查询向量（None 时跳过向量通道）。
    /// - `top_k`: 最大返回块数。
    /// - `persona_uid`: 目标 persona（原文隔离键）。
    ///
    /// 返回:
    /// - 按得分降序的命中列表（最多 top_k 条）。
    pub fn search_utt(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        top_k: usize,
        persona_uid: Option<&str>,
    ) -> Vec<UttHit> {
        let Some(target) = persona_uid else {
            return Vec::new();
        };

        // 严格隔离：只取目标 persona 的块
        let candidates: Vec<&UttDocView> = self
            .utt_docs
            .values()
            .filter(|d| d.persona_uid == target)
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }

        // ---- 向量通道 ----
        if let Some(qv) = query_vec {
            match self.vector_index.search(qv, &self.config.vector) {
                Ok(hits) => {
                    let mut out: Vec<UttHit> = hits
                        .into_iter()
                        .filter_map(|h: VectorHit| {
                            let (layer, id_str) = parse_vector_label(&h.doc_label)?;
                            if layer != "L0" {
                                return None;
                            }
                            let id = id_str.parse::<i64>().ok()?;
                            let doc = self.utt_docs.get(&id)?;
                            if doc.persona_uid != target {
                                return None; // 跨 persona 命中丢弃
                            }
                            Some(UttHit {
                                doc: doc.clone(),
                                score: h.adjusted_similarity,
                                channel: "vector",
                            })
                        })
                        .collect();
                    out.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    out.truncate(top_k);
                    if !out.is_empty() {
                        return out;
                    }
                    // 向量命中但均被隔离过滤（防御）→ 落入子串降级
                }
                Err(_) => {
                    // 空索引 / 维度不符 → 子串降级
                }
            }
        }

        // ---- 子串降级（BM25 分词 token 命中计数） ----
        self.search_utt_substring(query, &candidates, top_k)
    }

    /// 子串降级检索：query 分词 token 在块文本中的命中计数打分。
    fn search_utt_substring(
        &self,
        query: &str,
        candidates: &[&UttDocView],
        top_k: usize,
    ) -> Vec<UttHit> {
        let tokens = tokenize(query);
        let mut scored: Vec<UttHit> = Vec::new();

        for doc in candidates {
            let lower_text = doc.block_text.to_lowercase();
            let score: usize = if tokens.is_empty() {
                // 分词为空（如纯符号查询）→ 原始子串包含判定
                let q = query.trim().to_lowercase();
                if !q.is_empty() && lower_text.contains(&q) {
                    1
                } else {
                    0
                }
            } else {
                tokens
                    .iter()
                    .filter(|t| lower_text.contains(t.as_str()))
                    .count()
            };
            if score > 0 {
                scored.push(UttHit {
                    doc: (*doc).clone(),
                    score: score as f64,
                    channel: "substring",
                });
            }
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }
}
