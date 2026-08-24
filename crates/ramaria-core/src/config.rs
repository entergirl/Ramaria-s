//! crates/ramaria-core/src/config.rs - Ramaria 应用配置类型模块
//!
//! 设计特点:
//! - 按职责拆分配置域: 路径、后端、检索、衰减、Session、索引、日志、隐私
//! - 每组配置提供稳定默认值，保证首次启动和测试环境有一致行为
//! - 支持 serde 序列化与反序列化，便于 CLI、GUI 和配置文件共享
//! - 非敏感配置才允许进入 config.toml，API key 始终由 OS keychain 管理
//! - 配置结构只描述数据，不负责读取文件、访问环境变量或写入磁盘
//! - 内建版本控制（version + schema_version），支持未来配置文件迁移

use serde::{Deserialize, Serialize};

use crate::types::{LlmProvider, PersonaKind};

// =========================================================
// 版本控制常量
// =========================================================

/// 当前配置文件 schema 版本号。
///
/// 用途:
/// - 写入 config.toml 的 `schema_version` 字段。
/// - 当配置结构发生不兼容变更时递增此值，加载层据此触发迁移。
///
/// 版本历史:
/// - 1: 初始 schema
const CURRENT_SCHEMA_VERSION: u32 = 1;

/// 当前 Ramaria 应用版本号（与 workspace Cargo.toml 保持同步）。
const CURRENT_APP_VERSION: &str = "1.5.0";

// =========================================================
// 应用配置根结构
// =========================================================

/// Ramaria 完整应用配置。
///
/// 职责:
/// - 聚合所有非敏感配置项，作为 CLI、Desktop 和 app 编排层的统一配置入口。
/// - 提供稳定默认值，确保首次启动、测试和开发环境有可预测行为。
/// - 通过 serde 支持配置文件读写，但不负责具体 I/O。
/// - 内建版本控制，支持未来配置结构升级时的迁移检测。
///
/// 结构:
/// - `version` / `schema_version`: 版本控制字段，写入 config.toml。
/// - `paths`: 数据库、日志、配置、向量索引目录。
/// - `backend`: 当前 LLM 与 embedding 选择。
/// - `retrieval` / `decay` / `thresholds`: 记忆检索和分层记忆参数。
/// - `logging` / `privacy`: 日志与线上隐私相关开关。
///
/// 版本约定:
/// - `version` 记录写入此配置的 Ramaria 版本，加载时用于日志记录和兼容性警告。
/// - `schema_version` 记录配置文件的数据结构版本，加载时用于判断是否需要迁移。
/// - 两个字段在 config.toml 中为顶级键，serde 反序列化时缺失则回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RamariaConfig {
    /// 写入此配置的 Ramaria 版本号（如 "1.0.0"）
    #[serde(default = "default_version")]
    pub version: String,

    /// 配置文件 schema 版本号（用于未来迁移，初始值为 1）
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// 数据与路径配置
    #[serde(default)]
    pub paths: PathConfig,

    /// 当前选用的 LLM 后端
    #[serde(default)]
    pub backend: BackendSelection,

    /// 记忆与检索参数
    #[serde(default)]
    pub retrieval: RetrievalConfig,

    /// 记忆衰减参数
    #[serde(default)]
    pub decay: DecayConfig,

    /// Session 管理参数
    #[serde(default)]
    pub session: SessionConfig,

    /// 记忆层触发阈值
    #[serde(default)]
    pub thresholds: ThresholdConfig,

    /// 索引与 BM25
    #[serde(default)]
    pub index: IndexConfig,

    /// 日志
    #[serde(default)]
    pub logging: LoggingConfig,

    /// L3 性格推断配置（Phase B/C）
    #[serde(default)]
    pub inference: InferenceConfig,

    /// L2 事件提取 LLM 参数
    #[serde(default)]
    pub event_extraction: EventExtractionConfig,

    /// utt 话语块（原文注入通道，v1.4 新增）
    #[serde(default)]
    pub utt: UttConfig,

    /// examples（Few-shot 示例激活，v1.4 新增）
    #[serde(default)]
    pub examples: ExamplesConfig,

    /// 跨会话桥接（v1.4 新增）
    #[serde(default)]
    pub bridge: BridgeConfig,

    /// 三层生成缓存
    #[serde(default)]
    pub cache: CacheConfig,

    /// 行为模型学习与驱动配置
    #[serde(default)]
    pub behavior: BehaviorConfig,
    /// 知识层配置（persona_facts 生命周期与事实卡片注入）。
    #[serde(default)]
    pub knowledge: KnowledgeConfig,

    /// 嵌入模型运行时配置（`[embedding]`，设备选择）。
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    /// 杂项（预留扩展位，当前无字段）
    #[serde(default)]
    pub misc: MiscConfig,
}

// =========================================================
// Serde default 辅助函数
// =========================================================

/// serde `#[serde(default)]` 辅助函数：默认版本号。
fn default_version() -> String {
    CURRENT_APP_VERSION.to_string()
}

/// serde `#[serde(default)]` 辅助函数：默认 schema 版本。
fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

impl Default for RamariaConfig {
    /// 创建默认配置。
    ///
    /// 返回:
    /// - 可直接用于首次启动向导之前的安全默认配置。
    /// - 不包含任何 API key 或用户隐私数据。
    /// - `version` 自动填充当前 Ramaria 版本。
    /// - `schema_version` 自动填充当前 schema 版本。
    fn default() -> Self {
        Self {
            version: CURRENT_APP_VERSION.to_string(),
            schema_version: CURRENT_SCHEMA_VERSION,
            paths: PathConfig::default(),
            backend: BackendSelection::default(),
            retrieval: RetrievalConfig::default(),
            decay: DecayConfig::default(),
            session: SessionConfig::default(),
            thresholds: ThresholdConfig::default(),
            index: IndexConfig::default(),
            logging: LoggingConfig::default(),
            inference: InferenceConfig::default(),
            event_extraction: EventExtractionConfig::default(),
            utt: UttConfig::default(),
            examples: ExamplesConfig::default(),
            bridge: BridgeConfig::default(),
            cache: CacheConfig::default(),
            behavior: BehaviorConfig::default(),
            knowledge: KnowledgeConfig::default(),
            embedding: EmbeddingConfig::default(),
            misc: MiscConfig::default(),
        }
    }
}

// =========================================================
// 路径配置
// =========================================================

/// 数据与路径配置。
///
/// 职责:
/// - 描述 Ramaria 的数据目录、配置目录、日志目录和向量索引目录。
/// - 只保存路径字符串，不负责解析 `%APPDATA%` 或环境变量。
///
/// 说明:
/// - 默认值为空字符串，由上层配置加载器根据平台和运行模式填充。
/// - 开发模式可由 `RAMARIA_DATA_DIR` 或测试夹具覆盖。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathConfig {
    /// SQLite 数据库路径。Windows 默认 `%APPDATA%\Ramaria\data\assistant.db`
    pub data_dir: String,
    /// 配置文件目录
    pub config_dir: String,
    /// 日志目录
    pub log_dir: String,
    /// 向量索引目录
    pub vector_index_dir: String,
}

impl Default for PathConfig {
    /// 创建空路径配置。
    ///
    /// 返回:
    /// - 所有路径字段为空，等待配置加载层填充平台默认路径。
    fn default() -> Self {
        Self {
            data_dir: String::new(),
            config_dir: String::new(),
            log_dir: String::new(),
            vector_index_dir: String::new(),
        }
    }
}

// =========================================================
// 后端选择
// =========================================================

/// 当前选用的 LLM 后端。
///
/// 职责:
/// - 保存当前 provider、模型 ID、base URL 和生成参数。
/// - 保存 embedding 模型选择结果。
/// - 控制线上 provider 是否允许注入记忆上下文。
///
/// 安全约束:
/// - API key 不属于此结构，必须通过 OS keychain 读取。
/// - `base_url` 变化会影响隐私确认粒度，上层应重新确认。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：`[backend]` 表只写部分键（v1.2/v1.3 模板
///   即注释掉 `embedding_model_id`）时，缺失字段回退各自默认值，
///   保证旧配置文件可解析、不丢失其余键。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendSelection {
    /// 当前 provider。
    pub provider: LlmProvider,
    /// 当前模型标识。
    pub model_id: String,
    /// OpenAI-compatible API 基础地址。
    pub base_url: String,
    /// embedding 模型标识
    pub embedding_model_id: Option<String>,
    /// 生成温度。
    pub temperature: f64,
    /// 最大输出 token 数。
    pub max_tokens: u32,
    /// 是否允许线上后端注入 L1/L2/L3 上下文
    pub online_memory_injection: bool,
}

impl Default for BackendSelection {
    /// 创建默认后端选择。
    ///
    /// 返回:
    /// - 默认 provider 为 LM Studio。
    /// - 默认 base URL 为 LM Studio OpenAI-compatible 端点。
    /// - 默认允许线上记忆注入，但实际启用前仍需隐私确认。
    fn default() -> Self {
        Self {
            provider: LlmProvider::LmStudio,
            model_id: String::new(),
            base_url: "http://localhost:1234/v1".to_string(),
            embedding_model_id: None,
            temperature: 0.3,
            max_tokens: 1024,
            online_memory_injection: true,
        }
    }
}

// =========================================================
// 检索配置
// =========================================================

/// 记忆检索参数。
///
/// 职责:
/// - 控制 L0/L1/L2 检索数量、RRF 融合参数和各通道权重。
/// - 为混合 RAG 提供可调默认值。
///
/// 说明:
/// - 具体检索算法在 `ramaria-storage` / `ramaria-memory` 中实现。
/// - 此结构只定义参数，不执行检索。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// L0 滑动窗口大小
    pub l0_window_size: u32,
    /// L0 检索返回条数
    pub l0_retrieve_top_k: u32,
    /// L1 检索返回条数
    pub l1_retrieve_top_k: u32,
    /// L2 检索返回条数
    pub l2_retrieve_top_k: u32,
    /// 语义相似度过滤阈值（余弦距离，超过此值视为不相关）
    pub similarity_threshold: f64,
    /// RRF 融合平滑系数
    pub rrf_k: u32,
    /// BM25 通道权重
    pub bm25_weight: f64,
    /// 图谱通道权重
    pub graph_weight: f64,
    /// L2 结果排序权重（<1.0 表示 L2 优先展示）
    pub retrieval_weight_l2: f64,
    /// L1 结果排序权重
    pub retrieval_weight_l1: f64,
}

impl Default for RetrievalConfig {
    /// 创建默认检索参数。
    ///
    /// 返回:
    /// - 适合轻度聊天场景的 L0/L1/L2 检索规模。
    /// - RRF k=60，BM25 权重 1.0，图谱权重 0.8。
    fn default() -> Self {
        Self {
            l0_window_size: 3,
            l0_retrieve_top_k: 3,
            l1_retrieve_top_k: 4,
            l2_retrieve_top_k: 2,
            similarity_threshold: 0.6,
            rrf_k: 60,
            bm25_weight: 1.0,
            graph_weight: 0.8,
            retrieval_weight_l2: 0.8,
            retrieval_weight_l1: 1.0,
        }
    }
}

// =========================================================
// 记忆衰减配置（Ebbinghaus）
// =========================================================

/// Ebbinghaus 遗忘曲线衰减参数。
///
/// 职责:
/// - 描述不同记忆层的基础稳定性。
/// - 描述 salience 和近期访问对保留率的修正。
///
/// 衰减公式：R = e^(-t / S)
/// - R：保留率 0..1
/// - t：距生成的天数
/// - S：稳定性系数，越大衰减越慢
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    /// L0 稳定性系数（细节信息衰减最快）
    pub s_l0: u32,
    /// L1 稳定性系数
    pub s_l1: u32,
    /// L2 稳定性系数（聚合摘要衰减最慢）
    pub s_l2: u32,
    /// 是否启用访问加成
    pub enable_access_boost: bool,
    /// 近期访问加成天数
    pub recent_boost_days: u32,
    /// 近期访问保留率下限
    pub recent_boost_floor: f64,
    /// salience 对稳定性的加成系数
    /// S_adjusted = S × (1 + salience × multiplier)
    pub salience_multiplier: f64,
}

impl Default for DecayConfig {
    /// 创建默认衰减参数。
    ///
    /// 返回:
    /// - L0/L1/L2 稳定性分别为 10/30/60。
    /// - 启用最近访问加成和 salience 修正。
    fn default() -> Self {
        Self {
            s_l0: 10,
            s_l1: 30,
            s_l2: 60,
            enable_access_boost: true,
            recent_boost_days: 7,
            recent_boost_floor: 0.5,
            salience_multiplier: 0.5,
        }
    }
}

// =========================================================
// Session 管理配置
// =========================================================

/// Session 生命周期管理参数。
///
/// 职责:
/// - 描述空闲多久触发 L1 摘要。
/// - 描述后台检查间隔和对话历史保留规模。
///
/// 说明:
/// - 优先沿用现有 Python session 行为。
/// - 上层 app 编排层负责解释这些参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// 空闲超过此时长（分钟）自动触发 L1 摘要
    pub l1_idle_minutes: u32,
    /// 空闲检测轮询间隔（秒）
    pub idle_check_interval_seconds: u32,
    /// L2 定时检查间隔（秒）
    pub l2_check_interval_seconds: u32,
    /// 对话历史最大保留消息数
    pub max_history_messages: u32,
}

impl Default for SessionConfig {
    /// 创建默认 Session 管理参数。
    ///
    /// 返回:
    /// - 10 分钟空闲触发 L1。
    /// - 最多保留 40 条对话历史供上下文使用。
    fn default() -> Self {
        Self {
            l1_idle_minutes: 10,
            idle_check_interval_seconds: 60,
            l2_check_interval_seconds: 86400,
            max_history_messages: 40,
        }
    }
}

// =========================================================
// 记忆层触发阈值
// =========================================================

/// 记忆层触发阈值。
///
/// 职责:
/// - 控制何时将未吸收 L1 合并为 L2（路径 A 计数触发 + 路径 B 时间触发）。
/// - 控制何时触发 L3 性格推断（路径 A 计数触发 + 路径 B 时间触发）。
/// - 对齐 Python `MergerConfig` + `ProfileConfig` 的触发策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    /// 未吸收 L1 触发 L2 合并的条数阈值（路径 A）
    pub l2_trigger_count: u32,
    /// 最早未吸收 L1 触发 L2 的天数阈值（路径 B）
    pub l2_trigger_days: u32,
    /// 未吸收事件触发 L3 推断的条数阈值（路径 A）
    pub l3_trigger_count: u32,
    /// 最早未吸收事件触发 L3 推断的天数阈值（路径 B）
    pub l3_trigger_days: u32,
    /// L2 事件提取时簇间 LLM 请求间隔（毫秒），用于避免触发远程 API 速率限制。
    /// `Default` 实现为 800（等待 800ms）；建议对 DeepSeek 等有速率限制的 API 调大。
    #[serde(default)]
    pub cluster_delay_ms: u64,
}

impl Default for ThresholdConfig {
    /// 创建默认记忆层触发阈值。
    ///
    /// 返回:
    /// - 5 条未吸收 L1 或最早未吸收 L1 超过 7 天时触发 L2 检查。
    /// - 10 条未吸收事件或最早事件超过 30 天时触发 L3 推断。
    fn default() -> Self {
        Self {
            l2_trigger_count: 5,
            l2_trigger_days: 7,
            l3_trigger_count: 10,
            l3_trigger_days: 30,
            cluster_delay_ms: 800,
        }
    }
}

// =========================================================
// 事件提取配置（L1→L2）
// =========================================================

/// 事件提取器 LLM 参数。
///
/// 职责:
/// - 控制 EventExtractor 调用 LLM 时的 `max_tokens`、`temperature` 和单簇最大事件数。
/// - 独立于全局 `[backend]` 配置，因为事件提取的 JSON 输出需要比对话大得多的 token 预算。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventExtractionConfig {
    /// LLM 生成温度（0.0-2.0）
    #[serde(default = "default_event_extraction_temperature")]
    pub temperature: f64,
    /// 最大输出 token 数（事件 JSON 较长，需比对话大）
    #[serde(default = "default_event_extraction_max_tokens")]
    pub max_tokens: u32,
    /// 单簇最多提取的事件数
    #[serde(default = "default_event_extraction_max_events")]
    pub max_events: usize,
    /// 降级事件动态置信度公式开关。
    /// `true` → `min(0.59, 0.35 + 0.02 × n_l1)` 封顶 0.59 恒 tentative；
    /// `false` → 回退固定 `default_confidence`（0.5）。
    #[serde(default = "default_degraded_confidence_enabled")]
    pub degraded_confidence_enabled: bool,
}

fn default_event_extraction_temperature() -> f64 {
    0.3
}
fn default_event_extraction_max_tokens() -> u32 {
    8192
}
fn default_event_extraction_max_events() -> usize {
    5
}
fn default_degraded_confidence_enabled() -> bool {
    true
}

impl Default for EventExtractionConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 8192,
            max_events: 5,
            degraded_confidence_enabled: true,
        }
    }
}

// =========================================================
// 索引配置
// =========================================================

/// 索引相关参数。
///
/// 职责:
/// - 控制 BM25 增量更新和周期性重建节奏。
/// - 为后续向量索引和图谱索引配置预留扩展位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    /// BM25 增量合并阈值（缓冲区积累超过此条数触发合并）
    pub bm25_incremental_threshold: u32,
    /// BM25 定时重建间隔（秒）
    pub bm25_rebuild_interval: u32,
}

impl Default for IndexConfig {
    /// 创建默认索引参数。
    ///
    /// 返回:
    /// - BM25 缓冲区积累 10 条后合并。
    /// - 每 300 秒进行一次重建检查。
    fn default() -> Self {
        Self {
            bm25_incremental_threshold: 10,
            bm25_rebuild_interval: 300,
        }
    }
}

// =========================================================
// 日志配置
// =========================================================

/// 日志配置。
///
/// 职责:
/// - 控制是否记录完整 prompt。
/// - 为后续日志级别、日志目录和轮转策略预留配置入口。
///
/// 安全约束:
/// - `log_full_prompt` 默认关闭，开启前应由 UI/CLI 给出隐私警告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// 是否记录完整 prompt（默认关闭，需显式开启并警告）
    pub log_full_prompt: bool,
}

impl Default for LoggingConfig {
    /// 创建默认日志配置。
    ///
    /// 返回:
    /// - 默认不记录完整 prompt。
    fn default() -> Self {
        Self {
            log_full_prompt: false,
        }
    }
}

// =========================================================
// L3 推断配置
// =========================================================

/// L3 性格推断配置（Phase B + Phase C）。
///
/// 职责:
/// - 集中管理推断器、置信度更新、漂移检测和全量校准的参数。
/// - 所有字段均含合理默认值，无需手动配置即可运行。
///
/// 字段约定:
/// - `inferrer`: Phase B LLM 三步推断参数。
/// - `confidence`: Phase C 证据累积置信度参数。
/// - `drift`: Phase C Wasserstein 漂移检测参数。
/// - `calibration`: 定期全量校准触发参数。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InferenceConfig {
    /// Phase B 推断器配置
    #[serde(default)]
    pub inferrer: InferrerConf,
    /// Phase C 置信度配置
    #[serde(default)]
    pub confidence: ConfidenceConf,
    /// Phase C 漂移检测配置
    #[serde(default)]
    pub drift: DriftConf,
    /// 全量校准配置
    #[serde(default)]
    pub calibration: CalibrationConf,
    /// 画像升级开关。
    /// 独立配置开关：全部关闭时输出回退旧版行为。
    #[serde(default)]
    pub upgrade: InferenceUpgradeConfig,
}

/// Phase B 推断器配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferrerConf {
    /// LLM 生成温度（默认 0.3）
    #[serde(default = "default_inferrer_temperature")]
    pub temperature: f64,
    /// LLM 最大输出 tokens（默认 2048）
    #[serde(default = "default_inferrer_max_tokens")]
    pub max_tokens: u32,
    /// 小样本分类的证据阈值（默认 5.0）
    #[serde(default = "default_inferrer_low_evidence")]
    pub low_evidence_threshold: f64,
    /// 每步最大 tokens（默认 2048）
    #[serde(default = "default_inferrer_step_tokens")]
    pub step_max_tokens: u32,
}

fn default_inferrer_temperature() -> f64 {
    0.3
}
fn default_inferrer_max_tokens() -> u32 {
    2048
}
fn default_inferrer_low_evidence() -> f64 {
    5.0
}
fn default_inferrer_step_tokens() -> u32 {
    2048
}

impl Default for InferrerConf {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 2048,
            low_evidence_threshold: 5.0,
            step_max_tokens: 2048,
        }
    }
}

/// Phase C 置信度更新配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceConf {
    /// L2 层稳定性系数 S（默认 60，Ebbinghaus 遗忘曲线）
    #[serde(default = "default_confidence_stability")]
    pub stability_s: f64,
    /// 时间衰减保底值（默认 0.01）
    #[serde(default = "default_confidence_min_decay")]
    pub min_decay: f64,
}

fn default_confidence_stability() -> f64 {
    60.0
}
fn default_confidence_min_decay() -> f64 {
    0.01
}

impl Default for ConfidenceConf {
    fn default() -> Self {
        Self {
            stability_s: 60.0,
            min_decay: 0.01,
        }
    }
}

/// Phase C 漂移检测配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftConf {
    /// 显著性水平（锁定 0.05）
    #[serde(default = "default_drift_alpha")]
    pub alpha: f64,
    /// 置换检验次数（锁定 1000）
    #[serde(default = "default_drift_n_permutations")]
    pub n_permutations: usize,
}

fn default_drift_alpha() -> f64 {
    0.05
}
fn default_drift_n_permutations() -> usize {
    1000
}

impl Default for DriftConf {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            n_permutations: 1000,
        }
    }
}

/// 全量校准配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationConf {
    /// 增量更新轮次阈值（默认 10）
    #[serde(default = "default_calibration_round")]
    pub round_threshold: u32,
    /// 事件量翻倍比例阈值（默认 2.0）
    #[serde(default = "default_calibration_doubling")]
    pub event_doubling_ratio: f64,
    /// 差异告警比例（默认 0.3）
    #[serde(default = "default_calibration_diff_alert")]
    pub diff_alert_ratio: f64,
}

fn default_calibration_round() -> u32 {
    10
}
fn default_calibration_doubling() -> f64 {
    2.0
}
fn default_calibration_diff_alert() -> f64 {
    0.3
}

impl Default for CalibrationConf {
    fn default() -> Self {
        Self {
            round_threshold: 10,
            event_doubling_ratio: 2.0,
            diff_alert_ratio: 0.3,
        }
    }
}

// =========================================================
// 画像升级配置
// =========================================================

/// 画像升级开关。
///
/// 职责:
/// - 独立控制画像升级的四个增量（阈值 0.85 / 冷启动先验 / 降级置信度 / 漂移检测真实实现）。
/// - 全部关闭时画像输出回退旧版行为。
///
/// 兼容性说明:
/// - 每个开关默认开启。
/// - struct 级 `#[serde(default)]`：`[inference.upgrade]` 表只写部分键时回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceUpgradeConfig {
    /// 跨版本簇匹配阈值是否使用 0.85。
    /// `false` → 回退旧值 0.75。
    pub cross_version_threshold_085: bool,
    /// 冷启动先验是否使用跨用户经验分布。
    /// `false` → 回退当前 persona 内先验。
    pub cold_start_cross_user_prior: bool,
    /// 漂移检测是否从 `persona_cluster_snapshots` 恢复真实旧分布。
    /// `false` → 回退硬编码占位（全 0 / 0.5，all-zeros 守卫下不触发）。
    pub drift_restore_real_distribution: bool,
}

impl Default for InferenceUpgradeConfig {
    /// 创建默认画像升级配置。
    ///
    /// 返回:
    /// - 三个增量开关默认开启。
    fn default() -> Self {
        Self {
            cross_version_threshold_085: true,
            cold_start_cross_user_prior: true,
            drift_restore_real_distribution: true,
        }
    }
}

/// 杂项配置（预留扩展位）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MiscConfig {
    // 预留：未来可扩展天气查询城市、通知偏好等轻量选项
}

// =========================================================
// utt 话语块配置（v1.4 新增）
// =========================================================

/// utt 话语块（原文注入通道）配置。
///
/// 职责:
/// - 控制原文切分、检索与注入的开关和参数。
/// - 控制原文注入的 persona 类型白名单（隐私最小暴露）。
///
/// 安全约束:
/// - `persona_kind_whitelist` 默认仅角色类 persona（char/anim/oc/hist），
///   助手/系统类 persona 不注入原文，行为与 v1.3 完全一致。
/// - 原文是最高敏感层，关闭开关后注入行为整体回退 v1.3。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[utt]` 表只写部分键时
///   （部分覆盖场景），缺失字段回退 `Default` 实现，避免解析失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UttConfig {
    /// 是否启用 utt 话语块全链路（切分/构建/检索/注入）。
    /// `false` 时行为回退 v1.3（不注入原文片段）。
    pub enabled: bool,
    /// 时间间隙阈值（分钟）：相邻消息间隔超过此值切分为新块。
    /// 档位经 v1.5 探针对比定稿。
    pub theta_gap_minutes: u32,
    /// 单块最大消息条数：超过此条数强制切分。
    /// 档位经 v1.5 探针对比定稿。
    pub max_msgs_per_block: u32,
    /// 对话时检索返回的 utt 块数量（top_k）。
    /// 档位经 v1.5 探针对比定稿。
    pub retrieve_top_k: u32,
    /// 原文片段注入的字符预算上限（所有块合计）。
    /// 超预算时按相似度从低到高丢弃整块，不做块内截断。
    pub max_block_chars: u32,
    /// 原文注入的 persona 类型白名单。
    /// 白名单外的 persona（助手/系统类）不注入原文。
    pub persona_kind_whitelist: Vec<PersonaKind>,
}

impl Default for UttConfig {
    /// 创建默认 utt 配置。
    ///
    /// 返回:
    /// - 启用全链路，30 分钟间隙 / 40 条上限切分。
    /// - 检索 top_k=3，注入预算 1500 字符。
    /// - 白名单 = 角色类 persona（char/anim/oc/hist）。
    fn default() -> Self {
        Self {
            enabled: true,
            theta_gap_minutes: 30,
            max_msgs_per_block: 40,
            retrieve_top_k: 3,
            max_block_chars: 1500,
            persona_kind_whitelist: vec![
                PersonaKind::Char,
                PersonaKind::Anim,
                PersonaKind::Oc,
                PersonaKind::Hist,
            ],
        }
    }
}

// =========================================================
// examples 配置（v1.4 新增）
// =========================================================

/// examples（Few-shot 示例激活）配置。
///
/// 职责:
/// - 控制会话关闭时的回复对抽取、评分轮换与兜底注入。
/// - `max_examples` 与既有 `list_selected` 的 LIMIT 保持一致。
///
/// 说明:
/// - `enabled=false` 时行为回退 v1.3（读侧通道保留，写侧不激活）。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：`[examples]` 表只写部分键时回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExamplesConfig {
    /// 是否启用 examples 写侧激活（抽取/入库/轮换/兜底注入）。
    pub enabled: bool,
    /// 注入时的最大示例条数。
    pub max_examples: u32,
}

impl Default for ExamplesConfig {
    /// 创建默认 examples 配置。
    ///
    /// 返回:
    /// - 启用，最多注入 5 条示例（与既有查询 LIMIT 一致）。
    fn default() -> Self {
        Self {
            enabled: true,
            max_examples: 5,
        }
    }
}

// =========================================================
// 桥接配置（v1.4 新增）
// =========================================================

/// 跨会话桥接配置。
///
/// 职责:
/// - 控制新会话创建时是否加载最近一个已关闭会话的尾部原文。
/// - 桥接内容受原文白名单约束，不写日志。
///
/// 说明:
/// - `enabled=false` 时不加载桥接，行为等同 v1.3。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：`[bridge]` 表只写部分键时回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BridgeConfig {
    /// 是否启用桥接（新会话加载上一会话尾部原文）。
    pub enabled: bool,
    /// 桥接内容字符预算上限。
    /// 超限时从头部截断、保最近内容。
    pub max_chars: u32,
}

impl Default for BridgeConfig {
    /// 创建默认桥接配置。
    ///
    /// 返回:
    /// - 启用，预算 800 字符。
    fn default() -> Self {
        Self {
            enabled: true,
            max_chars: 800,
        }
    }
}

// =========================================================
// 缓存配置
// =========================================================

/// 缓存淘汰策略。
///
/// 说明:
/// - `Lru`: 最近最少使用（按 `last_accessed_at` 淘汰，命中会刷新访问时间）。
/// - `Fifo`: 先入先出（按 `created_at` 淘汰，与命中无关）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CacheEviction {
    /// 最近最少使用（默认）
    #[default]
    Lru,
    /// 先入先出
    Fifo,
}

impl CacheEviction {
    /// 返回策略的 snake_case 名称（供日志与展示）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lru => "lru",
            Self::Fifo => "fifo",
        }
    }
}

/// 三层生成缓存配置。
///
/// 职责:
/// - 控制 LLM 响应精确缓存（`llm_response_cache` 表）与 L2 聚类去重指纹。
/// - `enabled=false` 时精确缓存关闭，每次生成直接调用 LLM。
/// - L2 指纹可独立开关；关闭后事件提取不做集合跳过/相似度去重。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：`[cache]` 表只写部分键时回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// 精确缓存总开关。
    /// `false` 时 LLM 调用不查询/不写入缓存（行为同 v1.4）。
    pub enabled: bool,
    /// `llm_response_cache` 表容量上限（条目数）。
    /// 写入后超出上限按 `eviction` 策略淘汰最旧条目。
    pub max_entries: u64,
    /// 淘汰策略（lru | fifo）。
    pub eviction: CacheEviction,
    /// L2 聚类去重指纹开关。
    /// `false` 时不做「同集合跳过」与「新事件相似度去重」（行为回退 v1.4）。
    pub l2_fingerprint_enabled: bool,
    /// 新提取事件与已有事件相似度去重的判定阈值（0.0..=1.0）。
    /// 相似度 ≥ 此值时判为重复、跳过保存。
    pub l2_similarity_threshold: f64,
    /// 相似度去重比对的最远事件条数（取 persona 最近 N 条）。
    pub l2_recent_events_limit: u32,
}

impl Default for CacheConfig {
    /// 创建默认缓存配置。
    ///
    /// 返回:
    /// - 精确缓存默认开启，容量 10000 条，LRU 淘汰。
    /// - L2 指纹默认开启，相似度阈值 0.95，比对最近 200 条事件。
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 10_000,
            eviction: CacheEviction::Lru,
            l2_fingerprint_enabled: true,
            l2_similarity_threshold: 0.95,
            l2_recent_events_limit: 200,
        }
    }
}

// =========================================================
// 行为层配置
// =========================================================

/// 行为模型学习与驱动配置（`[behavior]` 配置组）。
///
/// 职责:
/// - 集中管理行为层全链路参数：聚类与规则生成、情境路由、增量更新。
/// - `enabled=false` 时行为层全链路关闭（不学习/不路由）。
///
/// 降级链:
/// - 聚类参数（θ_nb / min_cluster_size / θ_join / β1 / β2）按初值推进，
///   全部标注「待实证」——探针工具链定稿后回填，参数不可用时回退本默认值。
/// - embedding 不可用 → 双通道向量通道关闭，退化为纯关键词 Jaccard 通道（β=0）。
///
/// 字段约定:
/// - `theta_nb`: 密度聚类邻域相似度阈值（待实证：初值 0.5，v3.1 建议真实数据 P50~P75）。
/// - `beta1` + `beta2`: 双通道融合权重，约束 β1 + β2 ≤ 1（关键词通道 = 1 − β1 − β2）。
/// - `theta_route`: 路由阈值，全部候选低于此值 → 不注入（静默降级）。
/// - `top_n`: 路由 Top-N 合并上限（主规则完整注入 + 次规则仅合并 avoid/params）。
/// - `min_evidence`: 证据量门槛，簇内有效样本量 < 此值 → 不生成规则文本（仅参数）。
/// - `min_n_eff`: 有效样本量门槛（salience 加权），< 此值 → 降级候选规则。
/// - `valence_std_limit`: 簇内 valence 标准差上限，超限视为反应倾向不一致 → 降级候选。
/// - `max_outlier_ratio`: 孤立点比例上限，聚类超过此比例触发失败模式检查（下调 θ_nb）。
/// - `pending_expire_days`: 待定池样本超过此天数未成簇 → 低置信标记（不参与规则生成）。
/// - `evidence_decay_threshold`: 规则证据衰减后的保留率下限，低于此值 → 规则降级/失效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// 行为层总开关（false = 不学习/不路由，行为回退 v1.4）
    pub enabled: bool,
    /// 密度聚类邻域相似度阈值 θ_nb（待实证：v3.1 初值 0.5，v1.6 探针定稿）
    pub theta_nb: f64,
    /// 核心样本最小邻居数 min_cluster_size（v3.1 默认 3，探针备选 2）
    pub min_cluster_size: usize,
    /// 增量归簇阈值 θ_join（v3.1 默认 0.7）
    pub theta_join: f64,
    /// 反应通道权重 β1（待实证，v3.1 初值 0.4）
    pub beta1: f64,
    /// 情境通道权重 β2（待实证，v3.1 初值 0.3）
    pub beta2: f64,
    /// 情境路由阈值 θ_route（默认 0.6，全部低于 → 不注入）
    pub theta_route: f64,
    /// 路由评分 cos 项权重 γ（默认 0.7）
    pub gamma: f64,
    /// 路由 Top-N 合并上限（默认 3）
    pub top_n: usize,
    /// 规则文本生成的证据量门槛（默认 5）
    pub min_evidence: usize,
    /// 有效样本量门槛 n_eff（默认 5）
    pub min_n_eff: usize,
    /// 簇内 valence 标准差上限（默认 0.5，超限降级候选规则）
    pub valence_std_limit: f64,
    /// 聚类孤立点比例上限（默认 0.6，超限触发失败模式检查）
    pub max_outlier_ratio: f64,
    /// 待定池样本过期天数（默认 30，超期未成簇 → 低置信标记）
    pub pending_expire_days: u32,
    /// 规则证据衰减保留率下限（默认 0.3，低于 → 降级/失效）
    pub evidence_decay_threshold: f64,
    /// 行为层近期事件加权窗口（天）：窗口内事件 recency_factor=1.0，
    /// 之后指数衰减（半衰期 = 窗口）。
    pub recent_days: i64,
}

impl Default for BehaviorConfig {
    /// 创建默认行为层配置（v3.1 初值 + 待实证标注）。
    fn default() -> Self {
        Self {
            enabled: true,
            theta_nb: 0.5,
            min_cluster_size: 3,
            theta_join: 0.7,
            beta1: 0.4,
            beta2: 0.3,
            theta_route: 0.6,
            gamma: 0.7,
            top_n: 3,
            min_evidence: 5,
            min_n_eff: 5,
            valence_std_limit: 0.5,
            max_outlier_ratio: 0.6,
            pending_expire_days: 30,
            evidence_decay_threshold: 0.3,
            recent_days: 30,
        }
    }
}

// =========================================================
// 知识层配置
// =========================================================

/// 知识层配置组（`[knowledge]`）。
///
/// 职责:
/// - 控制知识层（persona_facts 生命周期 + 事实卡片注入）的开关与阈值。
/// - 总开关关闭时知识层全链路禁用，prompt 不含知识块。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeConfig {
    /// 知识层总开关（默认 false —— 自动抽取默认关闭，需用户显式开启）。
    ///
    /// `false` → 不抽取、不检索注入，prompt 不含知识块。
    pub auto_fact_detect: bool,
    /// 规则判定器开关（零新增 LLM 调用；false = 不检索注入，仅保留知识库写入能力）。
    pub detector_enabled: bool,
    /// 判重语义余弦阈值（默认 0.85）。
    pub dedup_cosine_threshold: f64,
    /// 判重关键词交集阈值（≥1 个共同词判重复）。
    pub dedup_keyword_min: u32,
    /// 多事件互证语义余弦阈值（默认 0.7）。
    pub corroboration_cosine_threshold: f64,
    /// 事实卡片注入预算（字符上限；默认 800）。
    pub injection_budget_chars: usize,
    /// volatile 事实时效半衰期（天）。
    pub volatile_halflife_days: u32,
}

impl Default for KnowledgeConfig {
    /// 创建默认知识层配置。
    ///
    /// 返回:
    /// - 自动抽取默认关闭（`auto_fact_detect=false`，需用户显式开启）。
    /// - 判重 0.85 / 互证 0.7。
    /// - 注入预算 800 字符；volatile 半衰期 30 天。
    fn default() -> Self {
        Self {
            auto_fact_detect: false,
            detector_enabled: true,
            dedup_cosine_threshold: 0.85,
            dedup_keyword_min: 1,
            corroboration_cosine_threshold: 0.7,
            injection_budget_chars: 800,
            volatile_halflife_days: 30,
        }
    }
}

// =========================================================
// 嵌入模型运行时配置
// =========================================================

/// 嵌入模型计算设备选择。
///
/// 职责:
/// - 控制 candle 编码器运行在哪个设备上（CPU / CUDA GPU / 自动探测）。
/// - 序列化到 `[embedding] device` 配置项。
///
/// 降级约束:
/// - `cuda` / `auto` 在 CUDA 不可用（未编译 feature 或环境无 GPU）时
///   静默回退 CPU，不阻塞模型加载（回归红线：静默降级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingDevice {
    /// 强制使用 CPU 推理（最保守，默认）。
    Cpu,
    /// 强制使用 CUDA GPU；不可用时回退 CPU。
    Cuda,
    /// 自动探测：CUDA 可用则用 GPU，否则 CPU。
    Auto,
}

impl Default for EmbeddingDevice {
    /// 默认使用自动探测设备。
    fn default() -> Self {
        Self::Auto
    }
}

impl EmbeddingDevice {
    /// 返回人类可读的设备名（用于日志与诊断）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Auto => "auto",
        }
    }

    /// 从 config.toml 内容解析嵌入设备配置。
    ///
    /// 参数:
    /// - `toml_text`: 配置文件全文。
    ///
    /// 返回:
    /// - 解析成功返回 `[embedding].device`；文件缺失 / 解析失败 / 字段缺失
    ///   均回退默认 `Auto`（静默降级，不阻塞启动）。
    pub fn from_toml_str(toml_text: &str) -> Self {
        toml::from_str::<RamariaConfig>(toml_text)
            .map(|cfg| cfg.embedding.device)
            .unwrap_or_default()
    }
}

/// 嵌入模型运行时配置组（`[embedding]`）。
///
/// 职责:
/// - 控制原生 safetensors 嵌入编码器的计算设备（CPU / CUDA / 自动）。
/// - 设备选择不改变向量语义，仅影响推理性能。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// 编码器计算设备（cpu / cuda / auto）。
    pub device: EmbeddingDevice,
}

impl Default for EmbeddingConfig {
    /// 创建默认嵌入配置（自动探测设备）。
    fn default() -> Self {
        Self {
            device: EmbeddingDevice::Auto,
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = RamariaConfig::default();

        // 版本控制字段
        assert_eq!(cfg.version, CURRENT_APP_VERSION);
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);

        // 检索参数默认值校验
        assert_eq!(cfg.retrieval.l0_window_size, 3);
        assert_eq!(cfg.retrieval.rrf_k, 60);
        assert!((cfg.retrieval.similarity_threshold - 0.6).abs() < f64::EPSILON);

        // 衰减参数
        assert_eq!(cfg.decay.s_l0, 10);
        assert_eq!(cfg.decay.s_l1, 30);
        assert_eq!(cfg.decay.s_l2, 60);
        assert!(cfg.decay.enable_access_boost);

        // Session 参数
        assert_eq!(cfg.session.l1_idle_minutes, 10);

        // 阈值
        assert_eq!(cfg.thresholds.l2_trigger_count, 5);
        assert_eq!(cfg.thresholds.l2_trigger_days, 7);

        // 后端默认
        assert_eq!(cfg.backend.provider, LlmProvider::LmStudio);

        // 日志默认
        assert!(!cfg.logging.log_full_prompt);

        // 缓存默认（v1.5）：精确缓存与 L2 指纹默认开启
        assert!(cfg.cache.enabled);
        assert_eq!(cfg.cache.max_entries, 10_000);
        assert_eq!(cfg.cache.eviction, CacheEviction::Lru);
        assert!(cfg.cache.l2_fingerprint_enabled);
        assert!((cfg.cache.l2_similarity_threshold - 0.95).abs() < f64::EPSILON);
        assert_eq!(cfg.cache.l2_recent_events_limit, 200);

        // 行为层默认：初值 + 待实证标注
        assert!(cfg.behavior.enabled);
        assert!((cfg.behavior.theta_nb - 0.5).abs() < f64::EPSILON);
        assert_eq!(cfg.behavior.min_cluster_size, 3);
        assert!((cfg.behavior.theta_join - 0.7).abs() < f64::EPSILON);
        assert!((cfg.behavior.beta1 - 0.4).abs() < f64::EPSILON);
        assert!((cfg.behavior.beta2 - 0.3).abs() < f64::EPSILON);
        assert!((cfg.behavior.theta_route - 0.6).abs() < f64::EPSILON);
        assert!((cfg.behavior.gamma - 0.7).abs() < f64::EPSILON);
        assert_eq!(cfg.behavior.top_n, 3);
        assert_eq!(cfg.behavior.min_evidence, 5);
        assert_eq!(cfg.behavior.min_n_eff, 5);
        assert!((cfg.behavior.valence_std_limit - 0.5).abs() < f64::EPSILON);
        assert!((cfg.behavior.max_outlier_ratio - 0.6).abs() < f64::EPSILON);
        assert_eq!(cfg.behavior.pending_expire_days, 30);
        assert!((cfg.behavior.evidence_decay_threshold - 0.3).abs() < f64::EPSILON);
        // 权重约束：β1 + β2 ≤ 1（关键词通道 = 1 − β1 − β2 非负）
        assert!(cfg.behavior.beta1 + cfg.behavior.beta2 <= 1.0);

        // 知识层默认：自动抽取默认关闭
        assert!(!cfg.knowledge.auto_fact_detect, "auto_fact_detect 默认关闭");
        assert!(cfg.knowledge.detector_enabled);
        assert!((cfg.knowledge.dedup_cosine_threshold - 0.85).abs() < f64::EPSILON);
        assert_eq!(cfg.knowledge.dedup_keyword_min, 1);
        assert!((cfg.knowledge.corroboration_cosine_threshold - 0.7).abs() < f64::EPSILON);
        assert_eq!(cfg.knowledge.injection_budget_chars, 800);
        assert_eq!(cfg.knowledge.volatile_halflife_days, 30);

        // 画像升级：三开关默认开启
        assert!(cfg.inference.upgrade.cross_version_threshold_085);
        assert!(cfg.inference.upgrade.cold_start_cross_user_prior);
        assert!(cfg.inference.upgrade.drift_restore_real_distribution);

        // 事件提取降级动态置信度默认开启
        assert!(cfg.event_extraction.degraded_confidence_enabled);
    }

    #[test]
    fn config_serde_roundtrip() {
        let cfg = RamariaConfig::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let back: RamariaConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(cfg.version, back.version);
        assert_eq!(cfg.schema_version, back.schema_version);
        assert_eq!(cfg.retrieval.rrf_k, back.retrieval.rrf_k);
        assert_eq!(cfg.decay.s_l0, back.decay.s_l0);
        assert_eq!(cfg.backend.provider, back.backend.provider);
        assert!(
            (cfg.decay.salience_multiplier - back.decay.salience_multiplier).abs() < f64::EPSILON
        );
        // 缓存组 roundtrip
        assert_eq!(cfg.cache.enabled, back.cache.enabled);
        assert_eq!(cfg.cache.max_entries, back.cache.max_entries);
        assert_eq!(cfg.cache.eviction, back.cache.eviction);
        assert_eq!(
            cfg.cache.l2_fingerprint_enabled,
            back.cache.l2_fingerprint_enabled
        );
        // 行为层 roundtrip
        assert_eq!(cfg.behavior.enabled, back.behavior.enabled);
        assert!((cfg.behavior.theta_nb - back.behavior.theta_nb).abs() < f64::EPSILON);
        assert_eq!(
            cfg.behavior.min_cluster_size,
            back.behavior.min_cluster_size
        );
        assert!((cfg.behavior.theta_route - back.behavior.theta_route).abs() < f64::EPSILON);
        assert!((cfg.behavior.gamma - back.behavior.gamma).abs() < f64::EPSILON);
    }

    #[test]
    fn path_config_serde() {
        let paths = PathConfig {
            data_dir: "/tmp/ramaria/data".into(),
            config_dir: "/tmp/ramaria/config".into(),
            log_dir: "/tmp/ramaria/logs".into(),
            vector_index_dir: "/tmp/ramaria/vectors".into(),
        };
        let json = serde_json::to_string(&paths).unwrap();
        let back: PathConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.data_dir, paths.data_dir);
        assert_eq!(back.vector_index_dir, paths.vector_index_dir);
    }

    #[test]
    fn config_json_contains_expected_keys() {
        let cfg = RamariaConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();

        assert!(json.contains("version"));
        assert!(json.contains("schema_version"));
        assert!(json.contains("l0_window_size"));
        assert!(json.contains("rrf_k"));
        assert!(json.contains("s_l0"));
        assert!(json.contains("l1_idle_minutes"));
        assert!(json.contains("l2_trigger_count"));
        assert!(json.contains("bm25_incremental_threshold"));
        assert!(json.contains("log_full_prompt"));
        // v1.4 新增配置组
        assert!(json.contains("theta_gap_minutes"));
        assert!(json.contains("max_msgs_per_block"));
        assert!(json.contains("retrieve_top_k"));
        assert!(json.contains("max_block_chars"));
        assert!(json.contains("persona_kind_whitelist"));
        assert!(json.contains("max_examples"));
        assert!(json.contains("bridge"));
    }

    // =========================================================
    // v1.4 新增配置组测试（[utt] / [examples] / [bridge]）
    // =========================================================

    #[test]
    fn utt_config_defaults() {
        let cfg = RamariaConfig::default();

        // 开关默认开启
        assert!(cfg.utt.enabled);
        // 切分参数
        assert_eq!(cfg.utt.theta_gap_minutes, 30);
        assert_eq!(cfg.utt.max_msgs_per_block, 40);
        // 检索与预算
        assert_eq!(cfg.utt.retrieve_top_k, 3);
        assert_eq!(cfg.utt.max_block_chars, 1500);
        // 默认白名单 = 角色类 persona（char/anim/oc/hist）
        let expected = [
            PersonaKind::Char,
            PersonaKind::Anim,
            PersonaKind::Oc,
            PersonaKind::Hist,
        ];
        assert_eq!(cfg.utt.persona_kind_whitelist, expected);
        // 助手/系统类不在默认白名单中（助手类不注入原文）
        assert!(!cfg.utt.persona_kind_whitelist.contains(&PersonaKind::Rama));
        assert!(!cfg.utt.persona_kind_whitelist.contains(&PersonaKind::User));
    }

    #[test]
    fn examples_config_defaults() {
        let cfg = RamariaConfig::default();
        assert!(cfg.examples.enabled);
        assert_eq!(cfg.examples.max_examples, 5);
    }

    #[test]
    fn bridge_config_defaults() {
        let cfg = RamariaConfig::default();
        assert!(cfg.bridge.enabled);
        assert_eq!(cfg.bridge.max_chars, 800);
    }

    #[test]
    fn v14_config_groups_serde_roundtrip() {
        // JSON 往返：新配置组序列化/反序列化保持一致
        let cfg = RamariaConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: RamariaConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(back.utt.theta_gap_minutes, cfg.utt.theta_gap_minutes);
        assert_eq!(
            back.utt.persona_kind_whitelist,
            cfg.utt.persona_kind_whitelist
        );
        assert_eq!(back.examples.max_examples, cfg.examples.max_examples);
        assert_eq!(back.bridge.max_chars, cfg.bridge.max_chars);
        assert_eq!(back.bridge.enabled, cfg.bridge.enabled);
    }

    #[test]
    fn v14_config_groups_toml_roundtrip() {
        // TOML 往返（config.toml 通道）：新配置组可经 toml 文本无损恢复
        let cfg = RamariaConfig::default();
        let toml_text = toml::to_string(&cfg).expect("默认配置应可序列化为 TOML");
        let back: RamariaConfig = toml::from_str(&toml_text).expect("默认 TOML 应可反序列化");

        assert_eq!(back.utt.enabled, cfg.utt.enabled);
        assert_eq!(back.utt.theta_gap_minutes, cfg.utt.theta_gap_minutes);
        assert_eq!(back.utt.max_msgs_per_block, cfg.utt.max_msgs_per_block);
        assert_eq!(back.utt.retrieve_top_k, cfg.utt.retrieve_top_k);
        assert_eq!(back.utt.max_block_chars, cfg.utt.max_block_chars);
        assert_eq!(
            back.utt.persona_kind_whitelist,
            cfg.utt.persona_kind_whitelist
        );
        assert_eq!(back.examples.enabled, cfg.examples.enabled);
        assert_eq!(back.examples.max_examples, cfg.examples.max_examples);
        assert_eq!(back.bridge.enabled, cfg.bridge.enabled);
        assert_eq!(back.bridge.max_chars, cfg.bridge.max_chars);
    }

    #[test]
    fn v14_config_groups_missing_fields_fallback_to_defaults() {
        // 兼容性：旧配置文件（无 [utt]/[examples]/[bridge]）解析后回退默认值
        let legacy_toml = r#"
version = "1.4.0"
schema_version = 1
[backend]
provider = "lm-studio"
"#;
        let cfg: RamariaConfig = toml::from_str(legacy_toml).expect("旧配置应可解析");
        assert!(cfg.utt.enabled, "缺失 [utt] 组应回退默认值");
        assert_eq!(cfg.utt.theta_gap_minutes, 30);
        assert_eq!(cfg.examples.max_examples, 5);
        assert!(cfg.bridge.enabled);
    }

    #[test]
    fn v14_config_groups_partial_override() {
        // 部分覆盖：只配置 [utt] 的 enabled=false，其余字段回退默认
        let partial_toml = r#"
[utt]
enabled = false
"#;
        let cfg: RamariaConfig = toml::from_str(partial_toml).expect("部分配置应可解析");
        assert!(!cfg.utt.enabled);
        // 未配置字段使用默认值
        assert_eq!(cfg.utt.theta_gap_minutes, 30);
        assert_eq!(cfg.utt.persona_kind_whitelist.len(), 4);
        assert_eq!(cfg.examples.max_examples, 5);
    }

    #[test]
    fn v14_whitelist_serde_string_form() {
        // config.toml 中以字符串数组书写白名单（PersonaKind lowercase 序列化）
        let toml_text = r#"
[utt]
persona_kind_whitelist = ["char", "anim", "oc", "hist"]
"#;
        let cfg: RamariaConfig = toml::from_str(toml_text).expect("白名单应可解析");
        assert_eq!(
            cfg.utt.persona_kind_whitelist,
            vec![
                PersonaKind::Char,
                PersonaKind::Anim,
                PersonaKind::Oc,
                PersonaKind::Hist
            ]
        );
    }
}
