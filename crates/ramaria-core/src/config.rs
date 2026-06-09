//! rust/crates/ramaria-core/src/config.rs - Ramaria 应用配置类型模块
//!
//! 设计特点:
//! - 按职责拆分配置域: 路径、后端、检索、衰减、Session、索引、日志、隐私
//! - 每组配置提供稳定默认值，保证首次启动和测试环境有一致行为
//! - 支持 serde 序列化与反序列化，便于 CLI、GUI 和配置文件共享
//! - 非敏感配置才允许进入 config.toml，API key 始终由 OS keychain 管理
//! - 配置结构只描述数据，不负责读取文件、访问环境变量或写入磁盘
//! - 内建版本控制（version + schema_version），支持未来配置文件迁移

use serde::{Deserialize, Serialize};

use crate::types::LlmProvider;

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
/// - 1: v1.0 初始 schema
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// 当前 Ramaria 应用版本号（与 workspace Cargo.toml 保持同步）。
pub const CURRENT_APP_VERSION: &str = "0.1.0";

// =========================================================
// 应用配置根结构
// =========================================================

/// Ramaria v1.0 完整应用配置。
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

    /// 隐私
    #[serde(default)]
    pub privacy: PrivacyConfig,

    /// 杂项
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
            privacy: PrivacyConfig::default(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// - v1.0 优先沿用现有 Python session 行为。
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
/// - 控制何时将未吸收 L1 合并为 L2。
/// - 通过数量和时间两个条件避免记忆长期停留在 L1。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    /// 未吸收 L1 触发 L2 合并的条数阈值
    pub l2_trigger_count: u32,
    /// 最早未吸收 L1 触发 L2 的天数阈值
    pub l2_trigger_days: u32,
}

impl Default for ThresholdConfig {
    /// 创建默认记忆层触发阈值。
    ///
    /// 返回:
    /// - 5 条未吸收 L1 或最早未吸收 L1 超过 7 天时触发 L2 检查。
    fn default() -> Self {
        Self {
            l2_trigger_count: 5,
            l2_trigger_days: 7,
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
// 隐私配置
// =========================================================

/// 隐私相关配置。
///
/// 职责:
/// - 作为隐私相关设置的结构化占位，供未来扩展（如日志脱敏级别、数据留存策略）。
/// - 线上记忆注入开关由 `BackendSelection.online_memory_injection` 统一管理。
///
/// 说明:
/// - v1.0 中此结构当前无字段。`online_memory_injection` 已归入 `BackendSelection`，
///   避免两处配置不一致导致行为未定义。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrivacyConfig {
    // v1.0 预留：未来可扩展日志脱敏级别、数据留存策略等
}

// =========================================================
// 杂项配置
// =========================================================

/// 杂项配置。
///
/// 职责:
/// - 放置尚未形成独立配置域的轻量选项。
/// - 避免临时字段散落到多个不相关结构中。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiscConfig {
    /// 天气查询城市（可选）
    pub weather_city: Option<String>,
}

impl Default for MiscConfig {
    /// 创建默认杂项配置。
    ///
    /// 返回:
    /// - 天气城市为空，表示未配置天气偏好。
    fn default() -> Self {
        Self { weather_city: None }
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
    }
}
