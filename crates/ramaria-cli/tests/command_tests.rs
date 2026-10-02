//! tests/command_tests.rs - CLI 命令集成测试
//!
//! 覆盖:
//! - session: list / show / delete (含空数据 / 有数据 / 不存在)
//! - config: list / get / set (含 API key 遮蔽 / 未知配置项 / 自定义 setting)
//! - memory: L1 / L2 / L3 (含空数据 / 有数据 / 未知 layer)
//! - export: JSON / Markdown (含空数据 / 有会话+消息)
//! - index_cmd: rebuild (mock retriever)
//!
//! 安全约束:
//! - 所有测试使用 MockStorage + MockLlm，不调用真实 LLM
//! - 不访问 OS keychain（MockLlm 使用 LM Studio provider，无需 keychain）
//! - 文件写入仅限系统临时目录（config 双写 / export 落盘），不触碰仓库文件

mod common;

#[path = "command_tests/cli_envelope.rs"]
mod cli_envelope;
#[path = "command_tests/fact_import.rs"]
mod fact_import;
#[path = "command_tests/memory_export.rs"]
mod memory_export;
#[path = "command_tests/session_config.rs"]
mod session_config;

use async_trait::async_trait;
use common::{
    MockStorage, build_test_engine, make_assistant_message, make_test_event, make_test_l1,
    make_test_trait, make_user_message,
};
use futures::Stream;
use ramaria_core::error::RamariaError;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::ChatRequest;
use ramaria_core::traits::LlmProvider;
use ramaria_core::traits::StorageBackend;
use ramaria_core::traits::StreamDelta;
use ramaria_core::types::BackendConfig;
use ramaria_core::types::LlmProvider as LlmProviderKind;
use ramaria_core::types::ModelCapability;
use ramaria_core::types::PersonaFact;
use std::pin::Pin;
use std::sync::Arc;
use uuid::Uuid;

/// 构造一个有数据的测试引擎（含 2 个 session + 消息 + L1 + L2 + L3 + settings）
async fn build_engine_with_data() -> (Arc<ramaria_service::Engine>, Arc<MockStorage>) {
    let (engine, storage) = build_test_engine();

    // Session 1: 有消息
    let sid1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    storage.create_session_with_messages(
        sid1,
        vec![
            make_user_message(sid1, "你好"),
            make_assistant_message(sid1, "你好！有什么我可以帮你的？"),
        ],
    );

    // Session 2: 无消息（已结束）
    let sid2 = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    storage.create_ended_session(sid2);

    // Session 3: 有大量消息
    let sid3 = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    storage.create_session_with_messages(
        sid3,
        vec![
            make_user_message(sid3, "今天天气真好"),
            make_assistant_message(sid3, "是的！适合出门走走。"),
            make_user_message(sid3, "有什么推荐的活动吗？"),
            make_assistant_message(sid3, "可以去公园散步，或者骑自行车。"),
        ],
    );

    // L1 记忆
    storage.add_l1(sid1, make_test_l1(sid1, "用户与AI打招呼，氛围友好"));
    storage.add_l1(
        sid3,
        make_test_l1(sid3, "用户询问户外活动建议，AI推荐了散步和骑行"),
    );

    // L2 事件
    storage.add_event("user-0001", make_test_event(1, "初次问候"));
    storage.add_event("user-0001", make_test_event(2, "户外活动咨询"));

    // L3 性格标签
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("友好", ramaria_core::types::TraitLayer::Base),
    );
    storage.add_personality_trait(
        "user-0001",
        make_test_trait("好奇心强", ramaria_core::types::TraitLayer::Primary),
    );

    // Settings
    storage.add_setting("theme", "dark");
    storage.add_setting("language", "zh-CN");

    (engine, storage)
}

/// 用指定 LLM provider 构造 ready 状态的测试引擎。
fn build_engine_with_llm(
    llm: Arc<dyn LlmProvider>,
) -> (Arc<ramaria_service::Engine>, Arc<MockStorage>) {
    use ramaria_core::config::RamariaConfig;
    use ramaria_service::Engine;

    let storage = Arc::new(MockStorage::new());
    let config = RamariaConfig::default();
    let engine = Engine::from_parts(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        None,
        config,
    );
    engine.set_state(ramaria_core::types::AppState::Ready);
    (Arc::new(engine), storage)
}

/// 恒失败的 Mock LLM（验证错误链保留 RamariaError source，退出码不退化）。
struct FailingLlm {
    model_capability: ModelCapability,
    config: BackendConfig,
}

impl FailingLlm {
    fn new() -> Self {
        let config = BackendConfig::lm_studio_default();
        Self {
            model_capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "failing-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config,
        }
    }
}

#[async_trait]
impl LlmProvider for FailingLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Err(RamariaError::llm("mock: LLM 恒失败"))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Err(RamariaError::llm("mock: LLM 恒失败"))
    }

    fn capability(&self) -> &ModelCapability {
        &self.model_capability
    }

    fn config(&self) -> &BackendConfig {
        &self.config
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "FailingLlm"
    }
}

// =========================================================
// Session 命令测试
// =========================================================

/// 构造带指定配置目录的测试引擎（配置双写测试需要真实库与配置文件；
/// 注入构造不携带库路径，无法执行配置读写用例，故此处装配真实临时库）。
async fn build_config_test_engine(dir: &std::path::Path) -> Arc<ramaria_service::Engine> {
    let engine = ramaria_service::Engine::open_with(
        ramaria_service::EngineOptions::new(dir.join("assistant.db"))
            .with_config_path(dir.join("config.toml")),
    )
    .await
    .expect("配置双写测试引擎装配应成功");
    Arc::new(engine)
}

/// 创建唯一临时测试目录（自动清理）。
fn temp_config_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-cli-config-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 关闭引擎持有的连接池句柄（Windows 下删除临时目录前需释放文件句柄）。
async fn close_engine_pool(engine: &ramaria_service::Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
}

/// 临时 DB 目录序号（避免并行测试共享目录）。
static DB_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 以真实二进制运行 CLI（临时 DB，进程退出后清理）。
fn run_cli(args: &[&str]) -> std::process::Output {
    let seq = DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db_dir = std::env::temp_dir().join(format!(
        "ramaria_cli_contract_{}_{}",
        std::process::id(),
        seq
    ));
    let _ = std::fs::create_dir_all(&db_dir);
    let db = db_dir.join("contract.db");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(&db)
        .output()
        .expect("运行 ramaria 二进制失败");
    let _ = std::fs::remove_dir_all(&db_dir);
    out
}

/// 构造一条测试 PersonaFact（默认 active/stable）。
fn make_test_fact(
    persona_uid: &str,
    field: ramaria_core::types::ProfileField,
    content: &str,
) -> PersonaFact {
    use ramaria_core::types::{FactSource, FactTier};
    let mut fact = PersonaFact::new(
        persona_uid.to_string(),
        field,
        content.to_string(),
        FactSource::Event,
    );
    fact.tier = FactTier::Stable;
    fact.keyword_hint = Some("测试,关键词".to_string());
    fact
}
