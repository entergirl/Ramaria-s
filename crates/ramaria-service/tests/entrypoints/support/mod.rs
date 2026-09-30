//! crates/ramaria-service/tests/entrypoints/support/mod.rs - 多入口装配测试支撑
//!
//! 设计特点:
//! - 复用平行对照测试的离线基建（错误类型 / 可选日志 / 脚本化 LLM / 预计算嵌入 / 造数），
//!   不重复实现 mock 依赖
//! - `TestDb`：一个临时真实 SQLite 库上按需装配任意多个引擎（每引擎独立连接池），
//!   模拟"桌面 + CLI + MCP"多进程同库场景；连接池统一登记供清理
//! - `drain_stream`：消费流式事件流并汇总完成 / 错误 / 文本，供写入类用例断言与等待
//! - `wait_until`：轮询等待异步条件（避免固定 sleep 抖动，超时给出等待目标）

#![allow(dead_code)]

#[path = "../../parity/support/error.rs"]
pub mod error;
#[path = "../../parity/support/fixtures.rs"]
pub mod fixtures;
#[path = "../../parity/support/log.rs"]
pub mod log;
#[path = "../../parity/support/mocks.rs"]
pub mod mocks;

pub use error::{ParityError, ParityResult};
pub use mocks::{DeterministicEmbedding, ScriptedLlm};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::now_ms;
use ramaria_service::{ChatStreamHandle, ChatStreamRequest, Engine, StreamEvent};
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;
use uuid::Uuid;

// =========================================================
// 测试库（临时 SQLite + 多引擎装配）
// =========================================================

/// 测试数据库：临时目录内的真实 SQLite 库，可装配多个引擎（各自独立连接池）。
///
/// 职责:
/// - 提供库文件路径与连接池创建（含全量 migration）；
/// - 按注入依赖装配引擎（`Engine::from_parts` 路径，不读配置文件、不写回配置）；
/// - 登记全部连接池，`cleanup` 时统一关闭并删除临时目录。
pub struct TestDb {
    dir: PathBuf,
    db_path: PathBuf,
    pools: Mutex<Vec<SqlitePool>>,
}

impl TestDb {
    /// 创建测试库描述（目录与库文件在首次打开连接池时创建）。
    ///
    /// 参数:
    /// - `tag`: 环境标签（进入临时目录名，便于排查残留目录）。
    pub fn new(tag: &str) -> Self {
        log::init_parity_log();
        let dir = unique_temp_dir(tag);
        Self {
            dir: dir.clone(),
            db_path: dir.join("assistant.db"),
            pools: Mutex::new(Vec::new()),
        }
    }

    /// 库文件路径（供"第二个进程"视角打开连接池）。
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 打开一个新连接池（含全量 migration）并登记清理。
    pub async fn open_pool(&self) -> ParityResult<SqlitePool> {
        let pool = ramaria_storage::database::init_pool(Some(self.db_path.clone()))
            .await
            .map_err(|e| ParityError::env("初始化测试库", e))?;
        self.pools
            .lock()
            .expect("测试库连接池登记锁不应中毒")
            .push(pool.clone());
        Ok(pool)
    }

    /// 以注入依赖装配引擎（同库独立连接池）。
    ///
    /// 参数:
    /// - `llm` / `embedding` / `config`: 直接注入的依赖（嵌入 `None` = 向量通道降级）。
    ///
    /// 返回:
    /// - 引擎与对应存储句柄（存储句柄供造数与结果断言）。
    pub async fn open_engine(
        &self,
        llm: Arc<dyn LlmProvider>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        config: RamariaConfig,
    ) -> ParityResult<(Arc<Engine>, Arc<SqliteStorage>)> {
        let pool = self.open_pool().await?;
        let storage = Arc::new(SqliteStorage::new(pool));
        let engine = Engine::from_parts(
            Arc::clone(&storage) as Arc<dyn StorageBackend>,
            llm,
            embedding,
            config,
        );
        Ok((Arc::new(engine), storage))
    }

    /// 清理：关闭全部登记连接池并删除临时目录（失败只记录，不影响测试结论）。
    pub async fn cleanup(self) {
        let pools = {
            let mut guard = self.pools.lock().expect("测试库连接池登记锁不应中毒");
            std::mem::take(&mut *guard)
        };
        for pool in pools {
            pool.close().await;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            tracing::debug!(dir = %self.dir.display(), error = %e, "删除测试库临时目录失败（忽略）");
        }
    }
}

/// 生成唯一临时目录路径（不创建；由连接池初始化创建父目录）。
fn unique_temp_dir(tag: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ramaria-entrypoints-{tag}-{nanos}-{sequence}"))
}

// =========================================================
// 流式生成辅助
// =========================================================

/// 流式生成的汇总结果。
///
/// 字段约定:
/// - `session_id`: 事件流句柄给出的会话标识；
/// - `text`: 全部增量拼接文本；
/// - `done`: 是否收到完成事件；
/// - `error`: 流内错误或流项错误的文本（无错误为 None）。
pub struct StreamSummary {
    pub session_id: Uuid,
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
}

/// 消费事件流直到结束，汇总会话标识 / 增量文本 / 完成与错误标记。
pub async fn drain_stream(handle: ChatStreamHandle) -> StreamSummary {
    let session_id = handle.session_id;
    let mut events = handle.events;
    let mut text = String::new();
    let mut done = false;
    let mut error = None;

    while let Some(item) = events.next().await {
        match item {
            Ok(event) => {
                match &event {
                    StreamEvent::Delta { content, .. } => text.push_str(content),
                    StreamEvent::Done { .. } => done = true,
                    StreamEvent::Error { error: message, .. } => error = Some(message.clone()),
                    // 事件枚举为非穷尽：新增变体按"仅忽略、不影响汇总口径"处理
                    _ => {}
                }
            }
            Err(e) => error = Some(e.to_string()),
        }
    }

    StreamSummary {
        session_id,
        text,
        done,
        error,
    }
}

/// 构造流式生成请求（无预置上文、无配置覆盖）。
pub fn stream_request(message: &str, persona: &str) -> ChatStreamRequest {
    ChatStreamRequest {
        message: message.to_string(),
        persona: Some(persona.to_string()),
        session_id: None,
        seed_history: Vec::new(),
        config_override: None,
    }
}

// =========================================================
// 等待与造数
// =========================================================

/// 轮询等待异步条件成立（每 25ms 检查一次；超时 panic 并给出等待目标）。
pub async fn wait_until<F, Fut>(what: &str, timeout: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await {
            return;
        }
        if Instant::now() >= deadline {
            panic!("等待超时（{}s）：{what}", timeout.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// 造一个"最后消息已超时"的活跃会话（最后消息固定在空闲阈值之外的偏移）。
///
/// 参数:
/// - `storage`: 目标存储；
/// - `persona`: 会话归属人格；
/// - `idle_minutes`: 空闲阈值（分钟；最后消息取该值 + 10 分钟之前）。
pub async fn seed_timed_out_session(
    storage: &SqliteStorage,
    persona: &str,
    idle_minutes: u32,
) -> ParityResult<Uuid> {
    let base_ts = now_ms() - (idle_minutes as i64 + 10) * 60_000;
    fixtures::seed_active_session(storage, persona, 2, base_ts).await
}
