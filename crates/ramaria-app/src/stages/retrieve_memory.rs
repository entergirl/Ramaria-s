//! rust/crates/ramaria-app/src/stages/retrieve_memory.rs - Stage 5: 记忆检索 + RAG
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 5: 记忆检索 + Persona-Aware RAG
//! - 三通道检索：向量 + BM25 + 图谱，RRF 融合
//! - 嵌入模型不可用时降级为 BM25 + 图谱（向量通道权重=0）
//! - 检索结果应用 Ebbinghaus 时间衰减，使近期记忆排序优于旧记忆
//! - Persona-Aware 过滤：按 persona_uid + share 阈值双重过滤
//! - 空检索结果时返回 None（Block C2 显示"暂无相关历史记忆"）

use async_trait::async_trait;
use ramaria_core::types::{PersonaKind, now_ms};
use ramaria_memory::decay::{DecayConfig, calc_decay_r};
use ramaria_memory::rag::{RagConfig, filter_by_persona, format_context_text};
use ramaria_memory::retriever::SearchRequest;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

/// Stage 5: 记忆检索 + Persona-Aware RAG。
///
/// 职责:
/// - 尝试使用嵌入模型生成查询向量（不可用时降级）
/// - 执行三通道检索（向量 + BM25 + 图谱），RRF 融合
/// - 对检索结果应用 Ebbinghaus 时间衰减
/// - 按 persona_kind + share 阈值进行 Persona-Aware 过滤
/// - 格式化为上下文文本，写入 PipelineData.memory_context
///
/// 降级策略:
/// - 嵌入模型未配置 → query_vec = None，仅 BM25 + 图谱
/// - 嵌入模型不可用 → query_vec = None
/// - 查询向量生成失败 → query_vec = None，warn 日志
/// - 检索器锁中毒 → memory_context = None（不阻塞对话）
/// - 检索结果为空 → memory_context = None
///
/// 安全约束:
/// - 检索器使用 RwLock::read()（search 为 &self），允许多读并发
/// - 检索器操作为纯同步，不持有锁跨 .await
/// - 查询文本不记日志（仅记录维度和结果数量）
pub struct StageRetrieveMemory;

impl StageRetrieveMemory {
    /// 创建 StageRetrieveMemory 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageRetrieveMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageRetrieveMemory {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "RetrieveMemory"
    }

    /// 执行记忆检索 + RAG。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文（读取 embedding、retriever、config）。
    /// - `input`: 管线数据，读取 `user_input`（作为查询）和 `persona_uid`（用于过滤）。
    ///
    /// 返回:
    /// - `Ok(data)`: 检索完成，`data.memory_context` 为 Some(context) 或 None。
    /// - `Err(Fatal)`: 从不返回——所有检索失败均降级为 None 或空结果。
    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        let query = &input.user_input;
        let persona_uid = input.persona_uid.as_deref();

        // ---- 5.1 尝试生成查询向量 ----
        // 先 clone Arc 出锁再 await，避免 MutexGuard 跨 .await
        let query_vec: Option<Vec<f32>> = match &ctx.embedding {
            Some(provider) if provider.is_available() => match provider.embed(query).await {
                Ok(vec) => {
                    tracing::debug!(dim = vec.len(), "查询向量已生成");
                    Some(vec)
                }
                Err(e) => {
                    tracing::warn!(%e, "查询向量生成失败，向量通道降级");
                    None
                }
            },
            Some(_) => {
                tracing::debug!("嵌入模型不可用，跳过向量通道");
                None
            }
            None => {
                tracing::debug!("嵌入模型未配置，跳过向量通道");
                None
            }
        };

        // ---- 5.2 执行三通道检索（RwLock::read() 允许多读并发） ----
        let mut results = {
            let retriever = match ctx.retriever.read() {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!(error = %e, "Retriever lock poisoned");
                    input.memory_context = None;
                    return Ok(input);
                }
            };

            let request = SearchRequest {
                query: query.to_string(),
                persona_uid: persona_uid.map(|s| s.to_string()),
                top_k: ctx.config.retrieval.l1_retrieve_top_k as usize,
                filter_share: true,
            };

            match &query_vec {
                Some(qv) => retriever.search(&request, Some(qv)),
                None => retriever.search(&request, None),
            }
        };

        if results.is_empty() {
            tracing::debug!("无记忆上下文");
            input.memory_context = None;
            return Ok(input);
        }

        // ---- 5.3 时间衰减：rrf_score × Ebbinghaus decay ----
        let now = now_ms();
        let decay_config_l1 = DecayConfig::from_core(&ctx.config.decay, "l1");
        let decay_config_l2 = DecayConfig::from_core(&ctx.config.decay, "l2");

        for r in &mut results {
            let decay_config = if r.layer == "l2" {
                &decay_config_l2
            } else {
                &decay_config_l1
            };

            // salience: SearchResult 不携带此字段，使用中性值 0.5
            let salience = 0.5;
            let decay_factor = calc_decay_r(r.created_at, now, salience, decay_config);
            r.rrf_score *= decay_factor;

            tracing::trace!(
                doc_id = %r.doc_id,
                layer = %r.layer,
                decay_factor = format!("{:.4}", decay_factor),
                rrf_adjusted = format!("{:.4}", r.rrf_score),
                "时间衰减已应用"
            );
        }

        // 重新按衰减后 rrf_score 降序排序
        results.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ---- 5.4 Persona-Aware 过滤 + 格式化 ----
        let persona_kind = persona_uid
            .map(PersonaKind::from_uid)
            .unwrap_or(PersonaKind::Rama);

        let rag_config = RagConfig::default();
        let filtered = filter_by_persona(&results, persona_kind, &rag_config);

        if filtered.is_empty() {
            tracing::debug!("Persona-Aware 过滤后无结果");
            input.memory_context = None;
            return Ok(input);
        }

        let context = format_context_text(&filtered, &rag_config);

        tracing::debug!(
            total_results = results.len(),
            filtered = filtered.len(),
            context_chars = context.chars().count(),
            "记忆上下文已组装（含时间衰减）"
        );

        input.memory_context = Some(context);
        Ok(input)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockEmbedding, MockLlm, MockStorage, test_context};
    use ramaria_core::types::AppState;
    use std::sync::Arc;

    fn make_data(query: &str, persona_uid: Option<&str>) -> PipelineData {
        let mut data = PipelineData::new(
            query.to_string(),
            persona_uid.map(|s| s.to_string()),
            None,
            uuid::Uuid::new_v4(),
        )
        .with_app_state(AppState::Ready);
        data.session = Some(ramaria_core::types::Session::new());
        data
    }

    #[tokio::test]
    async fn empty_retriever_returns_none() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageRetrieveMemory::new();
        let data = make_data("你好", Some("rama-0001"));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("should succeed");
        assert!(output.memory_context.is_none());
    }

    #[tokio::test]
    async fn with_embedding_still_succeeds() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        let stage = StageRetrieveMemory::new();
        let data = make_data("测试查询", Some("rama-0001"));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        // 空检索器 → memory_context = None
        let output = result.expect("should succeed");
        assert!(output.memory_context.is_none());
    }

    #[tokio::test]
    async fn no_persona_uid_uses_rama_default() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageRetrieveMemory::new();
        let data = make_data("你好", None);

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn user_persona_uid_works() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageRetrieveMemory::new();
        let data = make_data("你好", Some("user-0001"));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stage_name_is_correct() {
        let stage = StageRetrieveMemory::new();
        assert_eq!(stage.name(), "RetrieveMemory");
    }
}
