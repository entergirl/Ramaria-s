//! crates/ramaria-service/src/engine/assemble/accessors.rs - Ramaria 引擎依赖访问器
//!
//! 设计特点:
//! - 统一返回已脱离锁的句柄：Arc 克隆（provider 快照）或引用（存储 / 锁槽）
//! - 异步路径不跨 await 持锁；读取方取到的是完整快照，无中间态
//! - crate 内用例与入口层共用同一组访问器，不各自持锁

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::config::RamariaConfig;
use ramaria_core::lock::{lock_recover, read_recover, write_recover};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::AppState;
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::keyword::KeywordService;
use ramaria_memory::retriever::Retriever;
use sqlx::SqlitePool;

use crate::engine::Engine;
use crate::vision::VisionProbeState;

// =========================================================
// 内部依赖访问器（crate 内用例实现使用）
// =========================================================
//
// 说明:
// - 统一返回 Arc 克隆（provider 快照）或引用（存储 / 锁槽），
//   用例实现拿到的都是"已脱离锁"的句柄，异步路径不会跨 `.await` 持锁。

impl Engine {
    /// 存储后端（crate 内用例实现使用）。
    pub(crate) fn storage_ref(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// LLM provider 快照（crate 内用例实现使用）。
    pub(crate) fn llm_ref(&self) -> Arc<dyn LlmProvider> {
        self.llm()
    }

    /// 嵌入 provider 快照（crate 内用例实现使用）。
    pub(crate) fn embedding_ref(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedding()
    }

    /// 行为层待定池（crate 内编排与测试使用）。
    pub(crate) fn behavior_pending_ref(&self) -> &Arc<Mutex<PendingPool>> {
        &self.behavior_pending
    }

    /// 图片理解能力探测缓存槽（crate 内用例使用）。
    pub(crate) fn vision_probe_slot(&self) -> &tokio::sync::Mutex<Option<VisionProbeState>> {
        &self.vision_probe
    }
}
// =========================================================
// 依赖访问器
// =========================================================

impl Engine {
    /// 存储后端引用（用例实现与诊断使用）。
    pub fn storage(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// 当前 LLM provider 快照（读锁内克隆 Arc，调用方在锁外使用，可安全移入异步任务）。
    pub fn llm(&self) -> Arc<dyn LlmProvider> {
        read_recover(&self.llm, "engine.llm").clone()
    }

    /// 嵌入 provider 快照（None = 向量通道降级）。
    pub fn embedding(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        read_recover(&self.embedding, "engine.embedding").clone()
    }

    /// 向量通道是否可用（嵌入模型已加载且自测可用）。
    ///
    /// 说明:
    /// - 判定包含 `is_available`（模型文件完整且未被标记降级），
    ///   与桌面 / CLI 对"嵌入可用"的口径一致；
    /// - 不发起推理调用，可高频调用（设置页轮询 / 每次召回前判定均可）。
    pub fn is_embedding_available(&self) -> bool {
        read_recover(&self.embedding, "engine.embedding")
            .as_ref()
            .is_some_and(|provider| provider.is_available())
    }

    /// OS keychain 引用（线上 provider 的 API key 读写）。
    pub fn keychain(&self) -> &Keychain {
        &self.keychain
    }

    /// OS keychain（Arc 克隆，供 provider 构造移入异步任务）。
    pub fn keychain_arc(&self) -> Arc<Keychain> {
        Arc::clone(&self.keychain)
    }

    /// 当前 LLM 响应精确缓存（None = `[cache].enabled=false`）。
    ///
    /// 用途:
    /// - 热更新 provider 时复用同一实例，切换后端后既有缓存不失效。
    pub fn llm_cache(&self) -> Option<Arc<dyn LlmResponseCache>> {
        read_recover(&self.llm_cache, "engine.llm_cache").clone()
    }

    /// 当前应用状态（首次配置 → 索引构建 → 就绪 / 降级）。
    pub fn current_state(&self) -> AppState {
        *lock_recover(&self.state, "engine.state")
    }

    /// 设置应用状态（状态变更记 info 日志，便于诊断流程卡点）。
    pub fn set_state(&self, state: AppState) {
        let old = {
            let mut guard = lock_recover(&self.state, "engine.state");
            let old = *guard;
            *guard = state;
            old
        };
        if old != state {
            tracing::info!(from = %old, to = %state, "应用状态变更");
        }
    }

    /// 生效配置快照（读锁内克隆 Arc，调用方在锁外使用）。
    pub fn config(&self) -> Arc<RamariaConfig> {
        read_recover(&self.config, "engine.config").clone()
    }

    /// 配置加载回退告警（装配时 `config.toml` 读取 / 解析失败的脱敏摘要）。
    ///
    /// 语义:
    /// - `Some(..)` = 装配时未能采用磁盘配置，引擎按默认配置运行；供入口层在
    ///   启动自检与门禁提示中携带可诊断原因（如 MCP 总开关「未开启」可能并非用户本意）；
    /// - 配置正常加载 / 文件缺失（属正常默认路径）时为 None；
    /// - 配置用例成功替换快照后清除（磁盘配置已按用例结果采用）。
    pub fn config_warning(&self) -> Option<String> {
        read_recover(&self.config_warning, "engine.config_warning").clone()
    }

    /// 实际使用的配置文件路径（`from_parts` 构造时为空路径）。
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// 数据库文件路径（`from_parts` 构造时为空路径）。
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 底层 SQLite 连接池（未附着时为 None）；读锁内克隆句柄后释放锁。
    ///
    /// 用法:
    /// - 以 `&SqlitePool` 为入口的用例（如导入）在入口层取句柄使用；
    /// - `open_with` 装配路径自动携带，`from_parts` 注入路径需先
    ///   [`Engine::attach_sqlite_pool`]。
    pub fn sqlite_pool(&self) -> Option<SqlitePool> {
        read_recover(&self.pool, "engine.pool").clone()
    }

    /// 附着底层 SQLite 连接池（宿主 / 测试已持有连接池时复用）。
    ///
    /// 用法:
    /// - 注入构造（`from_parts`）不创建连接池；需要导入等连接的用例时，
    ///   由持有方在本方法附着（`open_with` 装配路径无需调用）。
    pub fn attach_sqlite_pool(&self, pool: SqlitePool) {
        let mut guard = write_recover(&self.pool, "engine.pool");
        *guard = Some(pool);
    }

    /// 标记会话撤回处理在途（键 = 会话 id 文本）。
    ///
    /// 返回:
    /// - `true`: 键此前不在途（本次处理获得触发资格）；
    /// - `false`: 已有在途处理（连撤通知去重，调用方放弃触发）。
    pub(crate) fn recall_inflight_begin(&self, key: &str) -> bool {
        lock_recover(&self.recall_inflight, "engine.recall_inflight").insert(key.to_string())
    }

    /// 释出会话撤回在途标记（与 `recall_inflight_begin` 配对调用）。
    pub(crate) fn recall_inflight_end(&self, key: &str) {
        lock_recover(&self.recall_inflight, "engine.recall_inflight").remove(key);
    }

    /// 检索索引是否已加载（懒加载占位状态）。
    pub fn is_retriever_loaded(&self) -> bool {
        read_recover(&self.retriever, "engine.retriever").is_some()
    }

    /// 检索器槽句柄（探针与诊断的只读访问使用）。
    ///
    /// 语义:
    /// - 返回懒加载槽的共享句柄（`Arc` 克隆，可在锁外跨任务使用）；
    ///   槽内为 `None` 表示索引尚未构建，调用方按只读用途适配
    ///   （如探针读取文档数 / 执行自检查询）；
    /// - 读写锁纪律：调用方在锁内只做同步操作，不跨 `.await` 持锁。
    pub fn retriever_slot(&self) -> Arc<RwLock<Option<Retriever>>> {
        Arc::clone(&self.retriever)
    }

    /// 关键词镜像句柄（探针与诊断的只读访问使用）。
    ///
    /// 语义:
    /// - 返回关键词镜像（倒排 + 词典池）的共享句柄（`Arc` 克隆，可在锁外跨任务使用）；
    ///   镜像内容随索引重建与 L1 增量维护，写入路径由索引用例持有；
    /// - 读写锁纪律：调用方在锁内只做同步操作，不跨 `.await` 持锁。
    pub fn keyword_mirror(&self) -> Arc<RwLock<KeywordService>> {
        Arc::clone(&self.keyword_mirror)
    }
}
