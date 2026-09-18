//! crates/ramaria-app/src/stages/retrieve_memory.rs - Stage 5: 记忆检索 + RAG
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 5: 记忆检索 + Persona-Aware RAG
//! - 检索装配（多通道检索 / 衰减重排 / Persona-Aware 过滤 / 段落渲染 / utt 渲染）
//!   统一位于 `ramaria_memory::recall::assemble_recall`：本 Stage 只做配置映射与结果搬运，
//!   保证在线管线与服务层 recall 用例召回同源（同一份实现，两个入口）
//! - 多通道检索：向量 + BM25 + 图谱（RRF 融合），关键词镜像作为第四通道
//! - 嵌入模型不可用时降级为 BM25 + 图谱（向量通道缺席）
//! - 空检索结果时返回 None（Block C2 显示"暂无相关历史记忆"）
//! - 索引重建失败时告警"记忆注入可能不完整"（宿主侧健康标志，不进共用实现）

use async_trait::async_trait;
use ramaria_core::types::now_ms;
use ramaria_memory::recall::{RecallGates, RecallInput, RecallMemoryLayers, assemble_recall};

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
/// - 检索器锁中毒 → warn 日志后恢复内部数据继续检索（不阻塞对话）
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
        // 索引健康提示：最近一次重建失败时，本轮检索基于旧索引（记忆可能不完整）。
        // 不改变检索行为——旧索引仍可检索，仅提升可观测性；且仅在摘要路检索真正生效时提示
        // （双闸门全关的探针场景不检索，避免无意义告警）。
        if ctx.config.injection.memory_rag
            && ctx
                .retriever_rebuild_failed
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!("检索索引最近一次重建失败，本轮基于旧索引检索（记忆注入可能不完整）");
        }

        // 召回装配统一走共用实现（与 MCP 服务层 recall 用例同一份代码，保证召回同源）。
        let output = assemble_recall(RecallInput {
            retriever: &*ctx.retriever,
            keyword_mirror: &*ctx.keyword_service,
            storage: ctx.storage.as_ref(),
            embedding: ctx.embedding.as_deref(),
            query: &input.user_input,
            persona_uid: input.persona_uid.as_deref(),
            retrieval: &ctx.config.retrieval,
            decay: &ctx.config.decay,
            utt: &ctx.config.utt,
            gates: RecallGates {
                memory_rag: ctx.config.injection.memory_rag,
                // utt 通道：注入闸门与配置开关双闸门合成（与既有行为一致）
                utt: ctx.config.injection.utt && ctx.config.utt.enabled,
            },
            // 在线管线：摘要路两层（L1 + L2）全开（与既有行为一致）
            memory_layers: RecallMemoryLayers::default(),
            now_ms: now_ms(),
        })
        .await;

        // 结果搬运：无命中 / 闸门关闭 / 白名单外均为 None，与既有 Stage 语义一致。
        input.memory_context = output.memory_context;
        input.memory_doc_labels = output.doc_labels;
        input.utt_context = output.utt_context;
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
    use ramaria_memory::retriever::SearchRequest;
    use std::sync::Arc;
    use uuid::Uuid;

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

    // =========================================================
    // 注入闸门测试（探针消融 B0 / F3）
    // =========================================================

    /// 探针消融 B0：RAG 与 utt 双闸门关闭 → 直接返回，
    /// memory_context / utt_context 均 None（不触发 embedding/检索）。
    #[tokio::test]
    async fn injection_gate_off_skips_all_retrieval() {
        let mut ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        ctx.config.injection.memory_rag = false;
        ctx.config.injection.utt = false;
        let stage = StageRetrieveMemory::new();
        let data = make_data("你好", Some("rama-0001"));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("应成功");
        assert!(
            output.memory_context.is_none(),
            "RAG 闸门关闭无 memory_context"
        );
        assert!(output.utt_context.is_none(), "utt 闸门关闭无 utt_context");
    }

    /// RAG 闸门开但 utt 闸门关（如 F3 −表达层）：RAG 路径正常执行（空检索器 → None），
    /// utt 原文检索被跳过。
    #[tokio::test]
    async fn injection_rag_on_utt_off_runs_rag_only() {
        let mut ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        ctx.config.injection.utt = false; // memory_rag 保持默认开启
        let stage = StageRetrieveMemory::new();
        let data = make_data("测试查询", Some("rama-0001"));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("应成功");
        // 空检索器 + utt 关闭 → 两通道均无输出，但 RAG 分支已执行（不报错）
        assert!(output.memory_context.is_none());
        assert!(output.utt_context.is_none());
    }

    /// F3 −表达层（utt 关闭）但检索器含 utt 块 → utt_context 不被填充。
    #[tokio::test]
    async fn injection_utt_off_ignores_seeded_utt_blocks() {
        let mut ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            Some(Arc::new(MockEmbedding::new())),
        );
        // 注入一个 utt 块（若闸门不生效会被检索到）
        seed_utt(
            &ctx,
            1,
            "rama-0001",
            "目标角色说过的原话内容",
            Some(vec![0.1; 8]),
        );
        ctx.config.injection.utt = false;
        let stage = StageRetrieveMemory::new();
        let data = make_data("目标角色", Some("rama-0001"));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("应成功");
        assert!(
            output.utt_context.is_none(),
            "utt 闸门关闭时不应填充原文片段"
        );
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

    // =========================================================
    // touch 接线测试（v1.7，决策 D-V17-006 / 备忘 §二 7）
    // =========================================================

    /// 检索命中 L1 后必须调用 storage.touch_l1 刷新访问时间（激活 recent_boost_*）。
    #[tokio::test]
    async fn retrieval_hit_touches_l1_last_accessed() {
        let storage = Arc::new(MockStorage::new());
        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        // 注入一条可检索的 L1 文档（persona-0001）
        {
            let mut retriever = ctx.retriever.write().expect("retriever 锁可用");
            retriever.index_l1(&ramaria_memory::retriever::L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: "用户讨论了Rust编程语言".to_string(),
                keywords: Some("Rust,编程".to_string()),
                persona_uid: Some("persona-0001".to_string()),
                created_at: 1000,
                salience: 0.8,
                last_accessed_at: None,
            });
        }

        let stage = StageRetrieveMemory::new();
        let data = make_data("Rust", Some("persona-0001"));

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(output.memory_context.is_some(), "L1 检索应命中并组装上下文");

        let touched = storage.last_touched_l1_ids();
        assert!(
            !touched.is_empty(),
            "检索命中后必须调用 touch_l1（接线访问加成）"
        );
    }

    /// 检索无命中时不调用 touch_l1（无访问时间可刷新）。
    #[tokio::test]
    async fn retrieval_no_hit_does_not_touch() {
        let storage = Arc::new(MockStorage::new());
        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        // 空检索器 → 无命中
        let stage = StageRetrieveMemory::new();
        let data = make_data("完全不相关的内容", Some("persona-0001"));

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(output.memory_context.is_none());
        assert!(
            storage.last_touched_l1_ids().is_empty(),
            "无命中不应触发 touch"
        );
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

    // =========================================================
    // 关键词镜像通道（第四通道）测试
    // =========================================================

    /// 构造同时写入 retriever 与关键词镜像的 L1 文档。
    fn seed_l1_both(ctx: &PipelineContext, doc_id: Uuid, summary: &str, keywords: Option<&str>) {
        use ramaria_memory::retriever::L1DocView;
        let view = L1DocView {
            id: doc_id,
            summary: summary.to_string(),
            keywords: keywords.map(|s| s.to_string()),
            persona_uid: Some("persona-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
            last_accessed_at: None,
        };
        {
            let mut retriever = ctx.retriever.write().expect("retriever 锁可用");
            retriever.index_l1(&view);
        }
        {
            let mut svc = ctx.keyword_service.write().expect("keyword_service 锁可用");
            svc.reset_docs_from_views(&[view], &[]);
        }
    }

    /// 关键词镜像经别名归一可恢复 BM25 字面未命中的记忆（融合真实生效）。
    #[tokio::test]
    async fn keyword_channel_recovers_doc_via_alias_when_bm25_misses() {
        use ramaria_core::keyword::KeywordPoolRow;
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None, // embedding 不可用 → 两层镜像 + 无向量通道（纯字面融合）
        );
        let doc_id = Uuid::new_v4();
        // 摘要刻意不含"职业倦怠/最近"等查询 bigram，确保 BM25 无法命中
        seed_l1_both(
            &ctx,
            doc_id,
            "用户跟我说加班到深夜真的很累",
            Some("工作压力,加班"),
        );

        // 词典池：工作压力(canonical) + 职业倦怠(alias → 工作压力)
        {
            let mut svc = ctx.keyword_service.write().expect("keyword_service 锁可用");
            svc.load_pool_entries(&[
                KeywordPoolRow {
                    rowid: 1,
                    keyword: "工作压力".to_string(),
                    use_count: 5,
                    created_at: 0,
                    alias_status: None,
                    canonical_id: None,
                    canonical_keyword: None,
                },
                KeywordPoolRow {
                    rowid: 2,
                    keyword: "职业倦怠".to_string(),
                    use_count: 2,
                    created_at: 0,
                    alias_status: Some("alias".to_string()),
                    canonical_id: Some(1),
                    canonical_keyword: Some("工作压力".to_string()),
                },
            ]);
        }

        // 前置确认：纯 BM25 无法命中口语别名说法（无共享 bigram）
        {
            let guard = ctx.retriever.read().expect("retriever 锁可用");
            let req = SearchRequest {
                query: "最近职业倦怠怎么办".to_string(),
                persona_uid: Some("persona-0001".to_string()),
                top_k: 5,
                filter_share: true,
            };
            let bm25_only = guard.search(&req, None);
            assert!(
                bm25_only.is_empty(),
                "BM25 字面无共享 bigram，应无法命中（关键词通道的补足前提）"
            );
        }

        let stage = StageRetrieveMemory::new();
        let data = make_data("最近职业倦怠怎么办", Some("persona-0001"));
        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(
            output.memory_context.is_some(),
            "关键词镜像经别名归一应命中并组装 RAG 上下文"
        );
    }

    /// 关键词镜像通道开关关闭 → 检索回退三通道（BM25 字面命中仍正常）。
    #[tokio::test]
    async fn keyword_channel_disabled_keeps_bm25_behavior() {
        let mut ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        ctx.config.retrieval.enable_keyword_channel = false;
        let doc_id = Uuid::new_v4();
        seed_l1_both(
            &ctx,
            doc_id,
            "用户最近工作压力很大常常加班",
            Some("工作压力,加班"),
        );

        let stage = StageRetrieveMemory::new();
        let data = make_data("工作压力", Some("persona-0001"));
        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(
            output.memory_context.is_some(),
            "关闭关键词通道后 BM25 字面命中仍应组装上下文"
        );
    }

    // =========================================================
    // RAG 覆盖文档 label 传播（memory_doc_labels）
    // =========================================================

    /// 检索命中 L1 → memory_doc_labels 记录实际注入文本的 L1 label（`L1:{uuid}`）。
    #[tokio::test]
    async fn rag_hit_records_l1_doc_label() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let doc_id = Uuid::new_v4();
        {
            let mut retriever = ctx.retriever.write().expect("retriever 锁可用");
            retriever.index_l1(&ramaria_memory::retriever::L1DocView {
                id: doc_id,
                summary: "用户讨论了Rust编程语言".to_string(),
                keywords: Some("Rust,编程".to_string()),
                persona_uid: Some("persona-0001".to_string()),
                created_at: 1000,
                salience: 0.8,
                last_accessed_at: None,
            });
        }
        let stage = StageRetrieveMemory::new();
        let data = make_data("Rust", Some("persona-0001"));

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(output.memory_context.is_some(), "L1 检索应命中");
        assert_eq!(
            output.memory_doc_labels,
            vec![format!("L1:{doc_id}")],
            "RAG 覆盖集合应含实际注入的 L1 label"
        );
    }

    /// 检索命中 L2 事件 → memory_doc_labels 记录 `L2:{id}`。
    #[tokio::test]
    async fn rag_hit_records_l2_doc_label() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        // 注入 L2 事件（share 高，persona-0001 按 Char 类阈值 0.5 通过）
        ctx.retriever.write().expect("retriever 锁可用").index_l2(
            &ramaria_memory::retriever::L2DocView {
                id: 42,
                title: "完成Rust项目".to_string(),
                summary: "用户完成了第一个Rust项目".to_string(),
                keywords: Some("Rust,项目".to_string()),
                attitude: Some("满意".to_string()),
                paraphrase: None,
                persona_uid: "persona-0001".to_string(),
                share: 0.9,
                confidence: 0.9,
                created_at: 1000,
                salience: 0.8,
            },
        );
        let stage = StageRetrieveMemory::new();
        let data = make_data("Rust", Some("persona-0001"));

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(output.memory_context.is_some(), "L2 检索应命中");
        assert_eq!(
            output.memory_doc_labels,
            vec!["L2:42".to_string()],
            "覆盖集合应含实际注入的 L2 label: {:?}",
            output.memory_doc_labels
        );
    }

    /// RAG 闸门关闭（探针消融 B0）：memory_context 置空，覆盖集合也为空。
    #[tokio::test]
    async fn rag_gate_off_yields_empty_doc_labels() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        {
            let mut retriever = ctx.retriever.write().expect("retriever 锁可用");
            retriever.index_l1(&ramaria_memory::retriever::L1DocView {
                id: Uuid::new_v4(),
                summary: "用户讨论了Rust编程语言".to_string(),
                keywords: Some("Rust,编程".to_string()),
                persona_uid: Some("persona-0001".to_string()),
                created_at: 1000,
                salience: 0.8,
                last_accessed_at: None,
            });
        }
        let mut ctx = ctx;
        ctx.config.injection.memory_rag = false;
        ctx.config.injection.utt = false; // 关闭双闸门 → 跳过检索
        let stage = StageRetrieveMemory::new();
        let data = make_data("Rust", Some("persona-0001"));

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(output.memory_context.is_none());
        assert!(
            output.memory_doc_labels.is_empty(),
            "RAG 闸门关闭 → 覆盖集合应为空"
        );
    }
}
