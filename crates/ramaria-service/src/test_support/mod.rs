//! crates/ramaria-service/src/test_support/mod.rs - Ramaria 服务层测试脚手架模块
//!
//! 设计特点:
//! - 目录化拆分：LLM / 嵌入 mock、可失败存储包装、引擎装配、造数按职责分文件；
//! - 对调用方保持 `crate::test_support::X` 路径不变：本文件仅做子模块声明与 `pub(crate) use` 转出；
//! - 真实 SQLite（临时文件库 + 全量 migration）：用例测试直接验证存储交互，不用 mock 顶替；
//! - 最小 LLM mock：满足 `LlmProvider` 契约但不发起任何网络调用（CI 无外网依赖）；
//! - 统一脚手架：临时目录、引擎装配、persona / L1 / 消息造数在各用例测试间复用，
//!   避免每个模块各写一套 mock 导致口径漂移；
//! - 调用方负责删除临时目录（`std::fs::remove_dir_all`）。

mod embedding;
mod engine;
mod llm;
mod seed;
mod storage;

pub(crate) use embedding::DeterministicEmbedding;
pub(crate) use engine::{
    engine_on_existing_db, engine_with_db, engine_with_failable_storage, engine_with_failing_llm,
    engine_with_l1_reply, engine_with_llm_and_config, engine_with_llm_config_and_embedding,
    engine_with_shared_llm, engine_with_shared_scripted_llm,
};
pub(crate) use llm::{MockLlm, ScriptedLlm};
pub(crate) use seed::{
    seed_channel_session, seed_closed_session_with_messages, seed_l1, seed_messages, seed_persona,
    seed_session_with_messages, seed_utt_block, temp_dir,
};
pub(crate) use storage::FailableStorage;

/// 符合 L1 摘要 JSON 契约的固定 LLM 响应（供封存用例测试"LLM 可用"路径）。
pub(crate) const L1_JSON_REPLY: &str = r#"{
  "summary": "用户最近工作压力很大，聊到常常加班到深夜。",
  "keywords": "工作压力,加班",
  "time_period": "夜间",
  "atmosphere": "疲惫",
  "valence": -0.5,
  "salience": 0.8,
  "situation_strength": 4
}"#;
