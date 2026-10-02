//! crates/ramaria-core/src/config/core.rs - Ramaria 根配置结构模块
//!
//! 设计特点:
//! - 定义 RamariaConfig 根配置，聚合各配置域子结构
//! - 提供 default_version / default_schema_version 供 serde 缺省回退
//! - 实现 Default 保证首次启动与测试环境行为一致
//! - 只描述数据，不负责读取文件、环境变量或磁盘写入
//! - 支持 serde 序列化，供 CLI、GUI 与 config.toml 共享

use serde::{Deserialize, Serialize};

use super::{
    BackendSelection, BehaviorConfig, BridgeConfig, CURRENT_APP_VERSION, CURRENT_SCHEMA_VERSION,
    CacheConfig, DecayConfig, EmbeddingConfig, EventExtractionConfig, ExamplesConfig,
    FeedbackConfig, IndexConfig, InferenceConfig, InjectionBudgetConfig, InjectionGate,
    KnowledgeConfig, L1Config, LayerDedupConfig, LoggingConfig, McpConfig, MiscConfig, PathConfig,
    RetrievalConfig, SessionConfig, StyleConfig, ThresholdConfig, UttConfig,
};

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

    /// L1 摘要配置（渐进式摘要 B3）
    #[serde(default)]
    pub l1: L1Config,

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

    /// 风格统计配置（`[style]`，表达层风格自动学习 A3）。
    #[serde(default)]
    pub style: StyleConfig,

    /// 弱反馈环配置（`[feedback]`，自我修正闭环 H2）。
    #[serde(default)]
    pub feedback: FeedbackConfig,

    /// 记忆注入层运行时间门（探针消融专用，仅内存不落盘）。
    ///
    /// 职责:
    /// - 按"注入层"细粒度开关控制对话管线向 prompt 注入的各记忆段落，
    ///   供消融评估（B0/B1/F0/F1~F4/S_*）在单次调用内真实关闭对应层。
    /// - `#[serde(skip)]`：不写入 config.toml、不同步 DB settings——
    ///   本闸门只存在于内存中，默认全开，任何配置持久化/加载后均回退全开，
    ///   保证常规对话（未显式覆盖）行为与既有版本完全一致（回归红线）。
    /// - 与既有语义开关（如 `[behavior].enabled`）是"与"关系：
    ///   本闸门关闭 = 该层不注入；闸门开启 = 仍遵循既有语义开关。
    #[serde(skip)]
    pub injection: InjectionGate,

    /// 注入协调预算（`[injection_budget]`，RAG 基座与四层注入的协调分配）。
    ///
    /// 默认关闭：不启用时对话管线走既有各层独立预算 + `apply_token_budget`
    /// 整条截断路径，行为与既有版本逐字段等价（回归红线）。
    #[serde(default)]
    pub injection_budget: InjectionBudgetConfig,

    /// 层间证据去重与冲突仲裁（`[layer_dedup]`，注入装配前的跨层去重）。
    ///
    /// 默认关闭：不启用时对话管线沿用既有知识层引用级去重（RAG 覆盖集合 +
    /// 角色层同 id 剔除），行为与既有版本逐字段等价（回归红线）。
    #[serde(default)]
    pub layer_dedup: LayerDedupConfig,

    /// MCP 接入配置（`[mcp]`，外部 MCP 客户端挂载的记忆服务）。
    #[serde(default)]
    pub mcp: McpConfig,

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
            l1: L1Config::default(),
            utt: UttConfig::default(),
            examples: ExamplesConfig::default(),
            bridge: BridgeConfig::default(),
            cache: CacheConfig::default(),
            behavior: BehaviorConfig::default(),
            knowledge: KnowledgeConfig::default(),
            embedding: EmbeddingConfig::default(),
            style: StyleConfig::default(),
            feedback: FeedbackConfig::default(),
            injection: InjectionGate::default(),
            injection_budget: InjectionBudgetConfig::default(),
            layer_dedup: LayerDedupConfig::default(),
            mcp: McpConfig::default(),
            misc: MiscConfig::default(),
        }
    }
}
