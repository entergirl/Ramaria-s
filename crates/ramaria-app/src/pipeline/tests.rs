//! crates/ramaria-app/src/pipeline/tests.rs - 在线消息管线单元测试
//!
//! 设计特点:
//! - 覆盖 PipelineError 分类/转换、PipelineContext/PipelineData 状态构建。
//! - 使用 mock storage/LLM，不依赖真实 LLM/embedding。
use super::*;
use futures::stream;
use ramaria_core::types::MessageRole;
use ramaria_core::types::{
    ClusterSnapshot, EventRelation, LlmProvider as LlmProviderKind, MemoryEvent, MemoryL1,
    ModelCapability, Persona, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent,
    ProfileField, TraitEvidence, TraitStatus,
};
use std::pin::Pin;

// =========================================================
// 测试 Mock: 最小化 StorageBackend 实现
// =========================================================

/// 测试用 Mock StorageBackend——所有方法返回 Ok(default)。
///
/// 设计:
/// - 不维护任何状态，仅满足 trait 编译要求
/// - 编排器测试中的 Mock Stage 不会调用任何 Storage 方法
struct TestStorage;

#[async_trait::async_trait]
impl ramaria_core::traits::StoreCrud for TestStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        Ok(Session::with_persona(persona_uid.map(|s| s.to_string())))
    }
    async fn close_session(&self, _id: Uuid) -> RamariaResult<()> {
        Ok(())
    }
    async fn get_session(&self, _id: Uuid) -> RamariaResult<Option<Session>> {
        Ok(None)
    }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        Ok(Vec::new())
    }
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        Ok(Vec::new())
    }
    async fn delete_session(&self, _id: Uuid) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_message(&self, _msg: &ramaria_core::types::Message) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_messages(&self, _id: Uuid) -> RamariaResult<Vec<ramaria_core::types::Message>> {
        Ok(Vec::new())
    }
    async fn list_messages_by_persona(
        &self,
        _uid: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
        Ok(Vec::new())
    }
    async fn save_memory_l1(&self, _m: &MemoryL1) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_memory_l1(&self, _id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }
    async fn get_memory_l1(&self, _id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        Ok(None)
    }
    async fn mark_l1_absorbed(&self, _ids: &[Uuid]) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_unabsorbed_l1(&self, _uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }
    async fn create_persona(&self, _p: &Persona) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn get_persona_by_uid(&self, _uid: &str) -> RamariaResult<Option<Persona>> {
        Ok(None)
    }
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        Ok(Vec::new())
    }
    async fn update_persona(
        &self,
        _uid: &str,
        _name: &str,
        _avatar: Option<&str>,
        _config: Option<&str>,
        _desc: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_event(&self, _e: &MemoryEvent) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_events_by_persona(
        &self,
        _uid: &str,
        _offset: i64,
        _limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(Vec::new())
    }
    async fn list_unabsorbed_events(&self, _uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(Vec::new())
    }
    async fn mark_events_absorbed(&self, _event_ids: &[i64]) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_event_relation(&self, _r: &EventRelation) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn save_event_source(&self, _eid: i64, _l1: Uuid, _w: f64) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_fact(&self, _f: &PersonaFact) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_facts_by_persona(
        &self,
        _uid: &str,
        _field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }
    async fn save_trait(&self, _t: &PersonalityTrait) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_traits_by_persona(&self, _uid: &str) -> RamariaResult<Vec<PersonalityTrait>> {
        Ok(Vec::new())
    }
    async fn update_trait_confidence(
        &self,
        _id: i64,
        _c: f64,
        _e: f64,
        _cons: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn update_trait_status(&self, _id: i64, _s: TraitStatus) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_evidence(&self, _e: &TraitEvidence) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_evidence_by_trait(&self, _id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        Ok(Vec::new())
    }
    async fn save_example(&self, _e: &PersonaExample) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_selected_examples(&self, _uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        Ok(Vec::new())
    }
    async fn save_cluster_snapshot(&self, _s: &ClusterSnapshot) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn get_current_snapshots(
        &self,
        _uid: &str,
        _cat: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        Ok(Vec::new())
    }
    async fn upsert_keyword(&self, _k: &str) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::StoreInfrastructure for TestStorage {
    async fn save_privacy_consent(&self, _c: &PrivacyConsent) -> RamariaResult<()> {
        Ok(())
    }
    async fn get_privacy_consent(
        &self,
        _p: &str,
        _b: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        Ok(None)
    }
    async fn save_backend_config(&self, _c: &BackendConfig) -> RamariaResult<()> {
        Ok(())
    }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        Ok(None)
    }
    async fn get_schema_version(&self) -> RamariaResult<i32> {
        Ok(1)
    }
    async fn get_index_version(&self) -> RamariaResult<i32> {
        Ok(1)
    }
    async fn set_index_version(&self, _v: i32) -> RamariaResult<()> {
        Ok(())
    }
    async fn create_background_job(&self, _t: &str, _p: Option<&str>) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn update_job_status(&self, _id: i64, _s: &str, _e: Option<&str>) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        Ok(Vec::new())
    }
    async fn create_conflict(
        &self,
        _f: &str,
        _t: &str,
        _o: Option<&str>,
        _n: Option<&str>,
        _d: Option<&str>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_pending_conflicts(&self) -> RamariaResult<Vec<(i64, String, String, String)>> {
        Ok(Vec::new())
    }
    async fn resolve_conflict(&self, _id: i64) -> RamariaResult<()> {
        Ok(())
    }
    async fn get_setting(&self, _k: &str) -> RamariaResult<Option<String>> {
        Ok(None)
    }
    async fn set_setting(&self, _k: &str, _v: &str) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    async fn insert_graph_node(&self, _n: &str, _t: &str, _l: Option<Uuid>) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn get_graph_node(&self, _n: &str) -> RamariaResult<Option<(i64, String, String)>> {
        Ok(None)
    }
    async fn insert_graph_edge(
        &self,
        _s: i64,
        _t: i64,
        _r: &str,
        _d: Option<&str>,
        _l: Option<Uuid>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_graph_edges(&self, _s: i64) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
        Ok(Vec::new())
    }
    async fn insert_keyword_ref(
        &self,
        _keyword_id: &str,
        _doc_type: &str,
        _doc_id: &str,
        _persona_uid: &str,
        _weight: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn find_refs_by_keyword(
        &self,
        _keyword_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        Ok(vec![])
    }
    async fn find_refs_by_doc(
        &self,
        _doc_type: &str,
        _doc_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        Ok(vec![])
    }
}

// =========================================================
// 测试 Mock: 最小化 LlmProvider 实现
// =========================================================

/// 测试用 Mock LlmProvider——返回空回复。
struct TestLlm {
    config: BackendConfig,
    capability: ModelCapability,
}

impl TestLlm {
    fn new() -> Self {
        Self {
            config: BackendConfig::lm_studio_default(),
            capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "test-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for TestLlm {
    async fn chat(&self, _req: &ChatRequest) -> RamariaResult<String> {
        Ok(String::new())
    }
    async fn chat_stream(
        &self,
        _req: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>>
    {
        Ok(Box::pin(stream::iter(vec![Ok(StreamDelta {
            content: String::new(),
            done: true,
            metadata: Some("stop".into()),
        })])))
    }
    fn capability(&self) -> &ModelCapability {
        &self.capability
    }
    fn config(&self) -> &BackendConfig {
        &self.config
    }
    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }
    fn name(&self) -> &'static str {
        "TestLlm"
    }
}

// =========================================================
// 测试辅助: 构建 PipelineContext
// =========================================================

/// 构建测试用 PipelineContext。
///
/// 使用 TestStorage + TestLlm，检索器为空，配置为默认值。
fn test_context() -> PipelineContext {
    let storage: Arc<dyn StorageBackend> = Arc::new(TestStorage);
    let llm: Arc<dyn LlmProvider> = Arc::new(TestLlm::new());
    let config = RamariaConfig::default();
    let retriever = Arc::new(RwLock::new(Retriever::new()));
    let keychain = Arc::new(Keychain::new());
    let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));

    PipelineContext::new(storage, llm, None, config, retriever, keychain, lifecycle)
}

// =========================================================
// 测试辅助: Mock Stage 实现
// =========================================================

/// 透传 Stage——不做任何修改，直接返回输入数据。
struct PassThroughStage {
    stage_name: &'static str,
}

#[async_trait]
impl PipelineStage for PassThroughStage {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        self.stage_name
    }

    async fn execute(
        &self,
        _ctx: &PipelineContext,
        input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        Ok(input)
    }
}

/// 失败 Stage——始终返回 Fatal 错误。
struct FailStage {
    stage_name: &'static str,
}

#[async_trait]
impl PipelineStage for FailStage {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        self.stage_name
    }

    async fn execute(
        &self,
        _ctx: &PipelineContext,
        _input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        Err(PipelineError::fatal(
            self.stage_name,
            RamariaError::validation("stage deliberately failed for testing"),
        ))
    }
}

/// 标记 Stage——在 PipelineData 中写入标记值，验证 Stage 确实被执行。
struct MarkStage {
    stage_name: &'static str,
}

#[async_trait]
impl PipelineStage for MarkStage {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        self.stage_name
    }

    async fn execute(
        &self,
        _ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        // 在 system_prompt 字段追加标记，证明此 Stage 被执行
        let mark = input.system_prompt.unwrap_or_default();
        input.system_prompt = Some(format!("{mark}+{stage}", stage = self.stage_name));
        Ok(input)
    }
}

// =========================================================
// PipelineError 测试
// =========================================================

/// PipelineError 构造与 stage/source 访问验证。
#[test]
fn pipeline_error_construction_cases() {
    // retryable 构造
    let err = PipelineError::retryable("CallLlm", RamariaError::llm("connection timeout"));
    assert!(err.is_retryable());
    assert_eq!(err.stage(), "CallLlm");
    assert_eq!(err.source_error().category(), "llm");
    // fatal 构造
    let err = PipelineError::fatal(
        "CheckState",
        RamariaError::validation("app in fatal error state"),
    );
    assert!(!err.is_retryable());
    assert_eq!(err.stage(), "CheckState");
    assert_eq!(err.source_error().category(), "validation");
    // stage 名在不同变体间独立
    let retryable = PipelineError::retryable("A", RamariaError::llm("x"));
    let fatal = PipelineError::fatal("B", RamariaError::storage("y"));
    assert_eq!(retryable.stage(), "A");
    assert_eq!(fatal.stage(), "B");
}

/// PipelineError Display 输出验证。
#[test]
fn pipeline_error_display_cases() {
    let err = PipelineError::retryable("RetrieveMemory", RamariaError::storage("index locked"));
    let msg = err.to_string();
    assert!(msg.contains("RetrieveMemory"));
    assert!(msg.contains("retryable"));
    assert!(msg.contains("index locked"));
    let err = PipelineError::fatal("ResolveSession", RamariaError::validation("session closed"));
    let msg = err.to_string();
    assert!(msg.contains("ResolveSession"));
    assert!(msg.contains("fatal"));
    assert!(msg.contains("session closed"));
}

#[test]
fn pipeline_error_source_error_preserves_category() {
    let err = PipelineError::retryable("CallLlm", RamariaError::privacy("not confirmed"));
    assert_eq!(err.source_error().category(), "privacy");
    assert_eq!(err.source_error().context(), "not confirmed");
}

/// PipelineError → RamariaError 转换验证。
#[test]
fn pipeline_error_to_ramaria_error_cases() {
    let original = RamariaError::llm("timeout");
    let pipeline_err = PipelineError::retryable("CallLlm", original);
    let ramaria_err: RamariaError = pipeline_err.into();
    assert_eq!(ramaria_err.category(), "llm");
    assert!(ramaria_err.context().contains("timeout"));
    let original = RamariaError::validation("bad state");
    let pipeline_err = PipelineError::fatal("CheckState", original);
    let ramaria_err: RamariaError = pipeline_err.into();
    assert_eq!(ramaria_err.category(), "validation");
}

// =========================================================
// PipelineData 测试
// =========================================================

#[test]
fn pipeline_data_new_sets_input_fields() {
    let request_id = Uuid::new_v4();
    let data = PipelineData::new(
        "你好".to_string(),
        Some("rama-0001".to_string()),
        Some(Uuid::new_v4()),
        request_id,
    );
    assert_eq!(data.user_input, "你好");
    assert_eq!(data.persona_uid.as_deref(), Some("rama-0001"));
    assert!(data.session_id.is_some());
    assert_eq!(data.request_id, request_id);
}

#[test]
fn pipeline_data_new_defaults_stage_outputs() {
    let data = PipelineData::new("test".to_string(), None, None, Uuid::new_v4());
    // Stage 1-3 产出应为 None
    assert!(data.app_state.is_none());
    assert!(data.backend_config.is_none());
    assert!(data.session.is_none());

    // Stage 4 集合字段应为空
    assert!(data.history_messages.is_empty());
    assert!(data.recent_summaries.is_empty());
    assert!(data.last_active_at.is_none());

    // Stage 5-8 可选字段应为 None
    assert!(data.memory_context.is_none());
    assert!(data.system_prompt.is_none());
    assert!(data.budgeted_system_prompt.is_none());
    assert!(data.budgeted_memory_context.is_none());
    assert!(data.chat_request.is_none());

    // Stage 7 数值字段应为 0
    assert_eq!(data.estimated_tokens, 0);
    assert!(data.budgeted_history.is_empty());

    // Stage 9-10 流字段应为 None
    assert!(data.llm_stream.is_none());
    assert!(data.output_stream.is_none());
}

#[test]
fn pipeline_data_fields_are_writable() {
    let mut data = PipelineData::new("hello".to_string(), None, None, Uuid::new_v4());

    // 模拟 Stage 1 写入
    data.app_state = Some(AppState::Ready);
    assert_eq!(data.app_state, Some(AppState::Ready));

    // 模拟 Stage 3 写入
    let session = Session::new();
    data.session = Some(session.clone());
    assert_eq!(data.session.as_ref().unwrap().id, session.id);

    // 模拟 Stage 4 写入
    data.history_messages.push(ChatMessage {
        role: MessageRole::User,
        content: "历史消息".into(),
    });
    assert_eq!(data.history_messages.len(), 1);

    // 模拟 Stage 6 写入
    data.system_prompt = Some("System prompt".into());
    assert_eq!(data.system_prompt.as_deref(), Some("System prompt"));
}

// =========================================================
// SendMessagePipeline 编排器测试
// =========================================================

#[tokio::test]
async fn pipeline_empty_returns_data_unchanged() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![]);
    let request_id = Uuid::new_v4();
    let data = PipelineData::new("test".into(), None, None, request_id);

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("empty pipeline should succeed");
    assert_eq!(output.user_input, "test");
    assert_eq!(output.request_id, request_id);
}

#[tokio::test]
async fn pipeline_empty_stage_count_zero() {
    let pipeline = SendMessagePipeline::new(vec![]);
    assert_eq!(pipeline.stage_count(), 0);
}

#[tokio::test]
async fn pipeline_single_pass_through() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![Box::new(PassThroughStage {
        stage_name: "OnlyStage",
    })]);

    let data = PipelineData::new("hello".into(), None, None, Uuid::new_v4());
    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    assert_eq!(pipeline.stage_count(), 1);
}

#[tokio::test]
async fn pipeline_multiple_pass_through_preserves_data() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![
        Box::new(PassThroughStage {
            stage_name: "Stage1",
        }),
        Box::new(PassThroughStage {
            stage_name: "Stage2",
        }),
        Box::new(PassThroughStage {
            stage_name: "Stage3",
        }),
    ]);

    let data = PipelineData::new(
        "pipeline test".into(),
        Some("rama-0001".into()),
        None,
        Uuid::new_v4(),
    );
    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("all pass-through should succeed");
    assert_eq!(output.user_input, "pipeline test");
    assert_eq!(output.persona_uid.as_deref(), Some("rama-0001"));
    assert_eq!(pipeline.stage_count(), 3);
}

#[tokio::test]
async fn pipeline_stops_on_fatal_error() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![
        Box::new(PassThroughStage {
            stage_name: "BeforeFail",
        }),
        Box::new(FailStage {
            stage_name: "FailingStage",
        }),
        Box::new(PassThroughStage {
            stage_name: "AfterFail",
        }),
    ]);

    let data = PipelineData::new("error path".into(), None, None, Uuid::new_v4());
    let result = pipeline.execute(&ctx, data).await;

    let err = match result {
        Ok(_) => panic!("should fail at FailingStage"),
        Err(e) => e,
    };
    assert!(!err.is_retryable());
    assert_eq!(err.stage(), "FailingStage");
}

#[tokio::test]
async fn pipeline_stops_on_retryable_error() {
    let ctx = test_context();

    // 自定义 Retryable 失败 Stage
    struct RetryableFailStage;
    #[async_trait]
    impl PipelineStage for RetryableFailStage {
        type Input = PipelineData;
        type Output = PipelineData;
        fn name(&self) -> &'static str {
            "RetryableFail"
        }
        async fn execute(
            &self,
            _ctx: &PipelineContext,
            _input: Self::Input,
        ) -> Result<Self::Output, PipelineError> {
            Err(PipelineError::retryable(
                "RetryableFail",
                RamariaError::llm("temporary timeout"),
            ))
        }
    }

    let pipeline = SendMessagePipeline::new(vec![
        Box::new(PassThroughStage { stage_name: "Pass" }),
        Box::new(RetryableFailStage),
        Box::new(PassThroughStage {
            stage_name: "NeverReached",
        }),
    ]);

    let data = PipelineData::new("retry test".into(), None, None, Uuid::new_v4());
    let result = pipeline.execute(&ctx, data).await;

    let err = match result {
        Ok(_) => panic!("should fail at RetryableFail"),
        Err(e) => e,
    };
    assert!(err.is_retryable());
    assert_eq!(err.stage(), "RetryableFail");
}

#[tokio::test]
async fn pipeline_stages_executed_in_order() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![
        Box::new(MarkStage {
            stage_name: "Alpha",
        }),
        Box::new(MarkStage { stage_name: "Beta" }),
        Box::new(MarkStage {
            stage_name: "Gamma",
        }),
    ]);

    let data = PipelineData::new("order test".into(), None, None, Uuid::new_v4());
    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("mark stages should succeed");
    // MarkStage 在 system_prompt 中追加 "+StageName"
    // 执行顺序应为 Alpha → Beta → Gamma
    assert_eq!(output.system_prompt.as_deref(), Some("+Alpha+Beta+Gamma"));
}

#[tokio::test]
async fn pipeline_error_at_first_stage() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![Box::new(FailStage {
        stage_name: "FirstAndOnly",
    })]);

    let data = PipelineData::new("immediate fail".into(), None, None, Uuid::new_v4());
    let result = pipeline.execute(&ctx, data).await;

    let err = match result {
        Ok(_) => panic!("should fail immediately"),
        Err(e) => e,
    };
    assert_eq!(err.stage(), "FirstAndOnly");
}

#[tokio::test]
async fn pipeline_error_propagates_source_error() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![Box::new(FailStage {
        stage_name: "ValidationError",
    })]);

    let data = PipelineData::new("propagation".into(), None, None, Uuid::new_v4());
    let result = pipeline.execute(&ctx, data).await;

    let err = match result {
        Ok(_) => panic!("should fail"),
        Err(e) => e,
    };
    let source = err.source_error();
    assert_eq!(source.category(), "validation");
    assert!(source.context().contains("testing"));
}

#[tokio::test]
async fn pipeline_error_convertible_to_ramaria_error() {
    let ctx = test_context();
    let pipeline = SendMessagePipeline::new(vec![Box::new(FailStage {
        stage_name: "ConversionTest",
    })]);

    let data = PipelineData::new("conversion".into(), None, None, Uuid::new_v4());
    let result: Result<PipelineData, RamariaError> =
        pipeline.execute(&ctx, data).await.map_err(|e| e.into());

    assert!(result.is_err());
    let err = match result {
        Ok(_) => panic!("should convert to RamariaError"),
        Err(e) => e,
    };
    assert_eq!(err.category(), "validation");
}
