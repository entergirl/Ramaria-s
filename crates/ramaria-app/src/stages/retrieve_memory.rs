//! crates/ramaria-app/src/stages/retrieve_memory.rs - Stage 5: 记忆检索 + RAG
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

        // 注：RAG 未命中不提前返回——utt 原文通道（5.5）独立于 RAG，仍需执行
        if !results.is_empty() {
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
            } else {
                let context = format_context_text(&filtered, &rag_config);

                tracing::debug!(
                    total_results = results.len(),
                    filtered = filtered.len(),
                    context_chars = context.chars().count(),
                    "记忆上下文已组装（含时间衰减）"
                );

                input.memory_context = Some(context);
            }
        } else {
            tracing::debug!("无记忆上下文（utt 通道继续）");
            input.memory_context = None;
        }

        // ---- 5.5 utt 原文块检索（v1.4，原文通道） ----
        // 开关与白名单双闸门：关闭 / persona 类型不在白名单 → 不检索（行为回退 v1.3）。
        // 原文是最高敏感层：仅按 persona_uid 精确隔离检索，不跨 persona 共享。
        let utt_cfg = &ctx.config.utt;
        if utt_cfg.enabled {
            if let Some(puid) = persona_uid {
                let kind = PersonaKind::from_uid(puid);
                if utt_cfg.persona_kind_whitelist.contains(&kind) {
                    let hits = {
                        let retriever = match ctx.retriever.read() {
                            Ok(guard) => guard,
                            Err(e) => {
                                tracing::error!(error = %e, "Retriever lock poisoned during utt search");
                                input.utt_context = None;
                                return Ok(input);
                            }
                        };
                        retriever.search_utt(
                            query,
                            query_vec.as_deref(),
                            utt_cfg.retrieve_top_k as usize,
                            Some(puid),
                        )
                    };
                    if !hits.is_empty() {
                        let rendered = ramaria_memory::prompt::builder::render_utt_context(
                            &hits,
                            utt_cfg.max_block_chars as usize,
                        );
                        if !rendered.is_empty() {
                            tracing::debug!(
                                persona_uid = %puid,
                                hits = hits.len(),
                                budget_chars = utt_cfg.max_block_chars,
                                "utt 原文片段已渲染（不记录内容）"
                            );
                            input.utt_context = Some(rendered);
                        }
                    } else {
                        tracing::debug!(persona_uid = %puid, "utt 原文块无命中，跳过注入");
                    }
                } else {
                    tracing::debug!(
                        persona_uid = %puid,
                        kind = %kind.as_str(),
                        "persona 类型不在原文白名单，跳过原文注入（等同 v1.3）"
                    );
                }
            }
        } else {
            tracing::debug!("utt 配置关闭，跳过原文检索（等同 v1.3）");
        }

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

    // =========================================================
    // utt 原文通道测试（v1.4）
    // =========================================================

    /// 向测试检索器注入一个 utt 块。
    fn seed_utt(
        ctx: &PipelineContext,
        id: i64,
        persona_uid: &str,
        text: &str,
        vector: Option<Vec<f32>>,
    ) {
        use ramaria_memory::retriever::UttDocView;
        let mut retriever = ctx.retriever.write().expect("retriever 锁可用");
        retriever.index_utt(
            &UttDocView {
                id,
                persona_uid: persona_uid.to_string(),
                session_id: uuid::Uuid::new_v4(),
                block_text: text.to_string(),
                msg_count: 2,
                created_at: 1000,
            },
            vector,
        );
    }

    #[tokio::test]
    async fn utt_injected_for_whitelisted_persona() {
        // 角色类 persona（白名单内）且有命中 → 注入原文片段
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        seed_utt(&ctx, 1, "char-0001", "今天天气真好我们一起去公园", None);
        let stage = StageRetrieveMemory::new();
        let data = make_data("公园", Some("char-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert!(output.utt_context.is_some(), "白名单内应注入原文");
        let text = output.utt_context.unwrap();
        assert!(text.contains("公园"), "原文内容保留");
    }

    #[tokio::test]
    async fn utt_not_injected_for_rama_persona() {
        // 回归红线：助手类 persona（白名单外）不注入原文，prompt 与 v1.3 等价
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        seed_utt(&ctx, 1, "rama-0001", "rama 的原文", None);
        let stage = StageRetrieveMemory::new();
        let data = make_data("原文", Some("rama-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert!(output.utt_context.is_none(), "白名单外不注入原文");
    }

    #[tokio::test]
    async fn utt_disabled_skips_retrieval() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        seed_utt(&ctx, 1, "char-0001", "今天天气真好", None);
        let mut ctx = ctx;
        ctx.config.utt.enabled = false; // 开关关闭 → 行为回退 v1.3
        let stage = StageRetrieveMemory::new();
        let data = make_data("天气", Some("char-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert!(output.utt_context.is_none(), "开关关闭不注入");
    }

    #[tokio::test]
    async fn utt_no_hit_returns_none() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageRetrieveMemory::new();
        let data = make_data("完全不相关的内容", Some("char-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert!(output.utt_context.is_none(), "无命中不注入");
    }

    #[tokio::test]
    async fn utt_other_persona_invisible() {
        // 原文严格按 persona_uid 隔离：char-0001 检索不到 char-0002 的块
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        seed_utt(&ctx, 1, "char-0002", "别人的秘密原文", None);
        let stage = StageRetrieveMemory::new();
        let data = make_data("秘密原文", Some("char-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert!(output.utt_context.is_none(), "跨 persona 不可见");
    }

    #[tokio::test]
    async fn utt_budget_trims_low_score_blocks() {
        // 预算裁剪：高相似度块保留，低分块整块丢弃
        // 得分构造：query="命中话题散步"（3 tokens）；块1 命中 2 个、块2 命中 3 个 → 块2 确定排前
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        seed_utt(&ctx, 1, "char-0001", "命中话题的第一块内容", None);
        seed_utt(&ctx, 2, "char-0001", "命中话题散步的第二块内容", None);
        let mut ctx = ctx;
        ctx.config.utt.max_block_chars = 12; // 只够最高分的一块（12 字符）
        let stage = StageRetrieveMemory::new();
        let data = make_data("命中话题散步", Some("char-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        let text = output.utt_context.expect("有命中应注入");
        assert!(text.contains("第二块"), "高分块保留");
        assert!(!text.contains("第一块"), "超预算整块丢弃");
    }

    #[tokio::test]
    async fn utt_vector_channel_used_when_embedding_available() {
        // 块有向量 + query 向量可用 → 向量通道命中
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        // 非零块向量（MockEmbedding 返回零向量 query；余弦为 0 但命中成立）
        seed_utt(&ctx, 1, "char-0001", "向量检索目标块", Some(vec![1.0; 128]));
        let stage = StageRetrieveMemory::new();
        let data = make_data("任意查询", Some("char-0001"));

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert!(output.utt_context.is_some(), "向量通道应命中");
    }
}
