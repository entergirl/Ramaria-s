//! crates/ramaria-service/src/engine/mod.rs - 服务层引擎装配与用例挂载点
//!
//! 设计特点:
//! - `Engine` 自持依赖装配：storage（连接池 + migration）→ 配置（快照）→ LLM → 嵌入（可选）→ 检索占位
//! - 与传输无关：不依赖 app / cli / desktop / tauri，不持有界面或协议概念
//! - 配置纪律：config.toml 为配置权威源（双写以文件为准）；装配路径只读，
//!   写入只经配置用例（双写 / 热重载，见 [`Engine::save_config`]）
//! - 热更新：LLM / 嵌入 provider 与配置以 `RwLock` 持有快照，变更经用例整体替换；
//!   读取路径取克隆后在锁外使用，异步代码不跨 `.await` 持锁
//! - 降级链：嵌入模型缺失 → 向量通道不可用（BM25 + 关键词镜像继续工作），不阻塞装配
//! - 懒加载：检索索引在首次召回时构建，本层仅持有占位槽（避免进程启动即加载大库）；
//!   占位槽未加载期间产生的 L1 增量会置脏标记，保证下次加载重建不漏（见 `index_dirty`）
//! - 显式重建：`rebuild_index` 强制全量重建（跳过早退与冷却窗口），构建失败保留旧索引
//!   并置"重建失败"告警位与失败原因记录（`is_index_rebuild_failed` / `index_build_failure`，
//!   供诊断展示与宿主告警）
//! - 重建节流：跨进程代次变化触发的重建受 `[index].refresh_interval_seconds` 约束
//!   （0 = 不节流，见 `index_rebuild_cooldown_elapsed`）
//! - 宿主后台任务：进程内空闲检查循环由入口层拉起（`spawn_idle_loop`），退出时优雅关停
//! - 用例挂载点：recall / ingest / seal / tick_idle / history / persona / 模型管理均由用例实现接入
//! - 底层连接池：装配时保留 `SqlitePool` 句柄，供导入等以 `&SqlitePool` 为入口的用例使用
//!   （注入构造可经 `attach_sqlite_pool` 附着）
//!
//! 模块划分:
//! - `assemble`：装配路径（`open` / `open_with` / `from_parts`）、依赖访问器、策略与钩子注入、
//!   provider 热更新与装配辅助（配置只读加载 / provider 构造 / 嵌入恢复）；
//! - `usecases_memory`：记忆域用例（召回 / 生成 / 写入 / 封存 / 空闲检查 / 会话与历史 /
//!   索引加载与重建 / L1-L2-L3 手动触发与 L1 补扫）；
//! - `usecases_browse`：浏览域用例（L1 / L2 / L3 / 事实 / 证据链 / 会话列表与消息）；
//! - `usecases_persona`：人格域用例（人格列表 / 卡片 / 更新 / 导入 / 重生成，行为规则与表达风格）；
//! - `usecases_proactive`：主动对话名单用例（状态读取 / 开关写入）；
//! - `usecases_ops`：运维域用例（导出 / utt / 关键词 / 宿主后台任务 / 设置与元信息 / 隐私 /
//!   配置双写 / 首次配置 / 模型管理 / 诊断导出）；
//! - `index_state`：索引状态机字段读写（脏标记 / 代次 / 失败告警 / 构建时间与冷却窗口）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex, RwLock};

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::AppState;
use ramaria_llm::keychain::Keychain;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::keyword::KeywordService;
use ramaria_memory::retriever::Retriever;
use sqlx::SqlitePool;

use crate::index::{IndexBuildFailure, IndexStamp};
use crate::proactive::ProactiveSink;
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;
use crate::vision::VisionProbeState;

mod assemble;
mod index_state;
mod usecases_browse;
mod usecases_memory;
mod usecases_ops;
mod usecases_persona;
mod usecases_proactive;

// 装配辅助 re-export：模型管理用例（`crate::model`）复用同一 provider 构造口径
pub(crate) use assemble::build_llm_provider;

// =========================================================
// 装配选项
// =========================================================

/// 引擎装配选项。
///
/// 字段约定:
/// - `db_path`: 数据库文件路径（不存在时自动创建目录与库并执行 migration）。
/// - `config_path`: 配置文件路径；缺省取数据库同目录 `config.toml`。
#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub db_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

impl EngineOptions {
    /// 以数据库路径创建选项（配置路径缺省）。
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            config_path: None,
        }
    }

    /// 指定配置文件路径（链式调用）。
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }
}

// =========================================================
// 引擎
// =========================================================

/// 记忆与对话服务引擎。
///
/// 职责:
/// - 持有一整套与传输无关的依赖：存储后端、LLM provider、嵌入 provider、生效配置、检索器占位。
/// - 为入口层（MCP / CLI / 桌面 / 未来社交通道）提供统一用例入口。
///
/// 生命周期:
/// - 由入口层在进程启动时装配（[`Engine::open`]），进程退出时随 Arc 释放。
/// - 存储连接池（含 WAL 与 migration）随引擎创建，无需调用方管理。
///
/// 并发约定:
/// - `Engine` 自身为 `Send + Sync`，可放入 `Arc` 跨任务共享。
/// - 检索器槽为 `RwLock<Option<..>>`：检索读多写少，懒加载在首次召回时写入。
/// - LLM / 嵌入 provider 为 `RwLock` 快照：热更新整体替换，读取方取克隆后在锁外使用
///   （异步路径不持有锁跨 `.await`）。
/// - 生效配置同为 `RwLock` 快照：配置用例（双写 / 热重载）整体替换，读取方取克隆后在锁外使用。
pub struct Engine {
    /// 存储后端（业务 CRUD + 基础设施）。
    storage: Arc<dyn StorageBackend>,
    /// 当前 LLM provider 快照（按 DB 侧 backend_config 装配；后端配置变更时整体替换）。
    llm: RwLock<Arc<dyn LlmProvider>>,
    /// 嵌入模型 provider 快照（None = 向量通道降级，BM25 + 关键词镜像继续可用）。
    embedding: RwLock<Option<Arc<dyn EmbeddingProvider>>>,
    /// OS keychain（线上 provider 的 API key 来源；首次配置与后端配置热更新共用同一实例）。
    keychain: Arc<Keychain>,
    /// LLM 响应精确缓存（`[cache].enabled=false` 时为 None）。
    /// 热更新 provider 时复用同一实例，保证切换后端后既有缓存不失效。
    llm_cache: RwLock<Option<Arc<dyn LlmResponseCache>>>,
    /// 应用状态机（首次配置 → 索引构建 → 就绪 / 降级）。
    state: Mutex<AppState>,
    /// 生效配置快照（config.toml 为权威源；经配置用例（双写 / 热重载）整体替换 Arc）。
    ///
    /// 快照语义与 LLM / 嵌入 provider 一致：读取方在锁内克隆内层 Arc 后释放锁，
    /// 异步路径不持有锁跨 `.await`；写入方整体替换内层 Arc。
    config: RwLock<Arc<RamariaConfig>>,
    /// 配置加载回退告警（装配时 config.toml 读取 / 解析失败的脱敏摘要）。
    ///
    /// 语义:
    /// - `Some(..)` = 装配时未能采用磁盘配置，引擎按默认配置运行；供入口层在
    ///   启动自检与门禁提示中携带可诊断原因（见 [`Engine::config_warning`]）；
    /// - 配置用例成功替换快照后清除（磁盘配置已按用例结果采用）。
    config_warning: RwLock<Option<String>>,
    /// 实际使用的配置文件路径（`from_parts` 构造时为空路径）。
    config_path: PathBuf,
    /// 数据库文件路径（诊断与客户端配置片段展示用）。
    db_path: PathBuf,
    /// 底层 SQLite 连接池句柄（导入等以 `&SqlitePool` 为入口的用例使用；未附着时为 None）。
    ///
    /// 语义:
    /// - `open_with` 装配时自动附着；注入构造（`from_parts`）默认未附着，
    ///   由宿主 / 测试经 [`Engine::attach_sqlite_pool`] 附着；
    /// - 句柄为连接池的克隆（内部共享），读取方取克隆后在锁外使用。
    pool: RwLock<Option<SqlitePool>>,
    /// 内存检索器槽（懒加载：首次召回时由 `ensure_index_loaded` 构建并整体替换）。
    retriever: Arc<RwLock<Option<Retriever>>>,
    /// 关键词镜像（倒排 + 词典池）：召回第四通道，L1 生成后增量维护。
    keyword_mirror: Arc<RwLock<KeywordService>>,
    /// 索引脏标记：L1 增量镜像时检索器尚未加载 → 置脏，
    /// 保证下次加载（`ensure_index_loaded`）会重建，不在进程生命周期内漏掉该 L1。
    index_dirty: Arc<AtomicBool>,
    /// 索引代次快照（构建时记录；召回前与库内比对 → 其他进程写入后刷新内存索引）。
    index_stamp: Arc<RwLock<Option<IndexStamp>>>,
    /// 内存索引最近一次构建完成时间（Unix 毫秒；0 = 尚未构建）。
    /// 用途：`[index].refresh_interval_seconds` 生效时限制两次重建的最小间隔
    /// （写入密集期抑制整库重建风暴，代价是刷新延迟不超过该间隔）。
    last_index_build_ms: Arc<AtomicI64>,
    /// 检索索引"重建失败"告警位：最近一次构建尝试失败且旧索引未刷新。
    ///
    /// 语义:
    /// - `true` = 最近一次构建失败，共享检索器保留的是旧索引（仍可检索，但未刷新）；
    /// - 构建成功后复位；供诊断展示与宿主告警（记忆注入可能不完整）。
    index_rebuild_failed: Arc<AtomicBool>,
    /// 检索索引最近一次构建失败的原因记录（脱敏原因文本 + 记录时间）。
    ///
    /// 语义:
    /// - `Some(..)` = 最近一次构建失败；构建成功后清除；
    /// - 供诊断导出与宿主提示携带可诊断原因；读取方取克隆后在锁外使用
    ///   （异步路径不持锁跨 `.await`）。
    index_build_failure: Arc<RwLock<Option<IndexBuildFailure>>>,
    /// 行为层待定池（跨会话内存态：行为增量编排的归簇状态）。
    behavior_pending: Arc<Mutex<PendingPool>>,
    /// 召回隐私与边界策略（装配时按配置闸门映射缺省；入口层可按需注入覆盖）。
    recall_policy: Arc<RwLock<RecallPolicy>>,
    /// 封存钩子（行为 / 风格 / L2 触发；未注册则跳过，见 [`SealHooks`]）。
    seal_hooks: Arc<RwLock<SealHooks>>,
    /// 封存许可（服务层策略）：`false` 时禁止一切封存与摘要生成。
    ///
    /// 说明:
    /// - 语义对应入口层配置（如 `[mcp].allow_seal`，D-V21-009：「只写不封存」）；
    /// - 默认 `true`：桌面 / CLI / 测试路径行为不变；
    /// - 门禁落在 `seal` 用例与空闲检查入口，避免「只写不封存」被惰性体检
    ///   （续写超时会话先封存）或空闲循环绕过。
    seal_allowed: AtomicBool,
    /// 主动消息投递接收端（宿主注册制；未注册时调度静默丢弃）。
    proactive_sink: Arc<RwLock<Option<Arc<dyn ProactiveSink>>>>,
    /// 图片理解能力探测缓存（按 model_id + base_url 键控；None = 本进程尚未探测）。
    vision_probe: tokio::sync::Mutex<Option<VisionProbeState>>,
}

#[cfg(test)]
mod tests;
