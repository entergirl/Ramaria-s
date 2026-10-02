//! crates/ramaria-service/src/test_support/engine.rs - Ramaria 服务层测试用引擎装配模块
//!
//! 设计特点:
//! - 以真实 SQLite（临时文件库 + 全量 migration）装配引擎，用例测试直接验证存储交互；
//! - 覆盖无回复 / 固定回复 / 恒失败 / 共享句柄 / 脚本化等 LLM 口径与嵌入注入；
//! - 统一装配入口附着连接池句柄，对齐生产装配形态（以 `&SqlitePool` 为入口的用例可直接使用）；
//! - 支持在已存在库路径上装配第二台引擎（多进程并发场景的服务层等价物）。

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_storage::SqliteStorage;

use super::{FailableStorage, MockLlm, ScriptedLlm, temp_dir};
use crate::engine::{Engine, EngineOptions};

// =========================================================
// 引擎装配
// =========================================================

/// 装配一套"真实 SQLite + 无回复 mock LLM + 无嵌入"的引擎。
pub(crate) async fn engine_with_db(tag: &str) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm(tag, MockLlm::local()).await
}

/// 装配一套"真实 SQLite + 可注入失败的存储包装 + 无回复 mock LLM + 无嵌入"的引擎。
///
/// 返回:
/// - 引擎（存储为 [`FailableStorage`]，可注入重建路径失败）；
/// - 真实存储句柄（造数与结果断言用）；
/// - 失败注入句柄（切换失败开关）；
/// - 临时目录（调用方负责清理）。
pub(crate) async fn engine_with_failable_storage(
    tag: &str,
) -> (Engine, Arc<SqliteStorage>, Arc<FailableStorage>, PathBuf) {
    let dir = temp_dir(tag);
    let db_path = dir.join("assistant.db");
    // 先做一次装配（建库 + migration），再以同一路径构造测试可见的存储句柄
    let _ = Engine::open_with(EngineOptions::new(db_path.clone()))
        .await
        .expect("引擎装配应成功");
    let storage = Arc::new(SqliteStorage::new(
        ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("测试库初始化应成功"),
    ));
    let failable = Arc::new(FailableStorage::new(Arc::clone(&storage)));
    let engine = Engine::from_parts(
        Arc::clone(&failable) as Arc<dyn StorageBackend>,
        Arc::new(MockLlm::local()),
        None,
        RamariaConfig::default(),
    );
    (engine, storage, failable, dir)
}

/// 装配一套"真实 SQLite + 固定回复 mock LLM + 无嵌入"的引擎（LLM 可用路径）。
pub(crate) async fn engine_with_l1_reply(
    tag: &str,
    reply: &str,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm(tag, MockLlm::with_reply(reply)).await
}

/// 以指定 LLM 装配引擎（测试统一的装配入口）。
async fn engine_with_llm(tag: &str, llm: MockLlm) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm_and_config(tag, llm, RamariaConfig::default()).await
}

/// 装配一套"真实 SQLite + 恒失败 LLM + 无嵌入"的引擎（LLM 不可用降级路径）。
pub(crate) async fn engine_with_failing_llm(tag: &str) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm(tag, MockLlm::failing()).await
}

/// 以指定 LLM 与生效配置装配引擎（供需要覆盖阈值 / 开关等配置项的用例）。
pub(crate) async fn engine_with_llm_and_config(
    tag: &str,
    llm: MockLlm,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    assemble_engine(tag, Arc::new(llm), None, config).await
}

/// 以指定 LLM 与嵌入 provider 装配引擎（覆盖向量通道 / 状态机等用例）。
pub(crate) async fn engine_with_llm_config_and_embedding(
    tag: &str,
    llm: MockLlm,
    config: RamariaConfig,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    assemble_engine(tag, Arc::new(llm), embedding, config).await
}

/// 以共享句柄注入 LLM mock（调用计数等断言用），并可指定嵌入 provider。
pub(crate) async fn engine_with_shared_llm(
    tag: &str,
    llm: Arc<MockLlm>,
    config: RamariaConfig,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    let provider: Arc<dyn LlmProvider> = llm;
    assemble_engine(tag, provider, embedding, config).await
}

/// 以共享句柄注入脚本化 LLM（多步 LLM 链路的调用序列断言用），并可指定嵌入 provider。
pub(crate) async fn engine_with_shared_scripted_llm(
    tag: &str,
    llm: Arc<ScriptedLlm>,
    config: RamariaConfig,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    let provider: Arc<dyn LlmProvider> = llm;
    assemble_engine(tag, provider, embedding, config).await
}

/// 统一装配实现：建库（含 migration）→ 以注入依赖构造引擎 → 附着连接池句柄。
///
/// 说明:
/// - 连接池附着对齐生产装配（`open_with`）的形态：以 `&SqlitePool` 为入口的
///   用例（导入 / 通道概览等）在测试引擎上可直接使用。
async fn assemble_engine(
    tag: &str,
    llm: Arc<dyn LlmProvider>,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    let dir = temp_dir(tag);
    let db_path = dir.join("assistant.db");
    // 先做一次装配（建库 + migration），再以同一路径构造测试可见的存储句柄
    let _ = Engine::open_with(EngineOptions::new(db_path.clone()))
        .await
        .expect("引擎装配应成功");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("测试库初始化应成功");
    let storage = Arc::new(SqliteStorage::new(pool.clone()));
    let engine = Engine::from_parts(
        storage.clone() as Arc<dyn StorageBackend>,
        llm,
        embedding,
        config,
    );
    engine.attach_sqlite_pool(pool);
    (engine, storage, dir)
}

/// 在**已存在的库路径**上装配第二台引擎（多进程并发场景的服务层等价物）。
///
/// 职责:
/// - 模拟"桌面 + MCP 并存"：同一库文件、独立连接池与内存索引，各自持有 LLM provider；
/// - 用于验证抢占幂等（同一会话只生成一份 L1）与写锁争用（并发写不报 locked）。
///
/// 参数:
/// - `db_path`: 已由首台引擎建好并执行过 migration 的库文件路径。
/// - `llm` / `config`: 第二台引擎的 LLM provider 与生效配置。
pub(crate) async fn engine_on_existing_db(
    db_path: &std::path::Path,
    llm: MockLlm,
    config: RamariaConfig,
) -> Engine {
    let pool = ramaria_storage::database::init_pool(Some(db_path.to_path_buf()))
        .await
        .expect("第二连接池应可创建");
    let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));
    Engine::from_parts(storage, Arc::new(llm), None, config)
}
