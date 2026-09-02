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
const CURRENT_APP_VERSION: &str = "1.6.0";

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

    /// 杂项（预留扩展位，当前无字段）
    #[serde(default)]
    pub misc: MiscConfig,
}

// =========================================================
// 注入层运行时间门（探针消融专用，仅内存）
// =========================================================

/// 记忆注入层运行时间门——逐层控制对话 prompt 的记忆注入。
///
/// 职责:
/// - 承载消融评估所需的每层 on/off 开关：行为 / 知识 / 表达（说话风格+示例）/
///   utt 原文 / 脉络（近期对话脉络+桥接）/ RAG 相关记忆。
/// - 全部默认开启：不修改本结构时，对话管线行为与既有版本完全一致。
///
/// 使用约定:
/// - 本结构是"运行时内存开关"，不在 config.toml / DB settings 中持久化；
///   探针等评估场景在克隆出的配置上修改后传入 `send_message_with_config`。
/// - 字段与 prompt 段落一一对应（详见各字段注释），关闭后该段落不注入；
///   对应"层"的定义与技术报告 §16.3 消融口径一致：
///   - 行为层（`behavior`）: 情境-反应规则块。
///   - 知识层（`knowledge`）: 事实卡片（动态检索的知识块）。
///   - 表达层（`speaking_style` + `examples` + `utt`）: 说话风格 / 风格规则 /
///     对话示例 / 原文样例（原文片段）。
///   - 脉络层（`narrative` + `bridge`）: 近期对话脉络 / 桥接（上一会话尾部）。
///   - RAG 相关记忆（`memory_rag`）: ChatRequest.memory_context（L1/L2/L3 摘要检索）。
#[derive(Debug, Clone)]
pub struct InjectionGate {
    /// 行为规则注入（`## 行为规则`）。
    pub behavior: bool,
    /// 知识层事实卡片注入（`# 知识（知识层，按需）`）。
    pub knowledge: bool,
    /// 说话风格注入（`## 说话风格` / `## 自动风格规则`，表达层子段）。
    pub speaking_style: bool,
    /// 对话示例（Few-shot `## 对话示例`，表达层子段）。
    pub examples: bool,
    /// utt 原文片段注入（`## 原文片段`，表达层"原文样例"）。
    pub utt: bool,
    /// 近期对话脉络注入（`## 近期对话脉络`，脉络层）。
    pub narrative: bool,
    /// 桥接注入（`## 桥接（上一会话尾部）`，脉络层）。
    pub bridge: bool,
    /// RAG 相关历史记忆注入（`ChatRequest.memory_context`，摘要/转述通道）。
    pub memory_rag: bool,
}

impl InjectionGate {
    /// 全部开启（默认状态；ablation=None 时行为与既有版本一致）。
    pub fn all_on() -> Self {
        Self {
            behavior: true,
            knowledge: true,
            speaking_style: true,
            examples: true,
            utt: true,
            narrative: true,
            bridge: true,
            memory_rag: true,
        }
    }

    /// 全部关闭（B0 无记忆注入：仅保留 persona 角色与当前对话）。
    pub fn all_off() -> Self {
        Self {
            behavior: false,
            knowledge: false,
            speaking_style: false,
            examples: false,
            utt: false,
            narrative: false,
            bridge: false,
            memory_rag: false,
        }
    }
}

impl Default for InjectionGate {
    /// 默认全部开启（无覆盖时与既有版本行为一致）。
    fn default() -> Self {
        Self::all_on()
    }
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
    /// 脉络加权注入开关（v1.7 B4）：跨会话近期摘要按"时间（衰减 × 访问加成）× 话题相关性"
    /// 融合排序注入；`false` 回退 v1.6 的"无条件取最近 N 条"。
    #[serde(default = "default_narrative_weighted")]
    pub narrative_weighted: bool,
    /// 脉络注入的最大条数（v1.7 B4），默认 3。
    #[serde(default = "default_narrative_top_k")]
    pub narrative_top_k: u32,
}

/// serde 默认值：脉络加权注入默认启用（自动为主可配置）。
fn default_narrative_weighted() -> bool {
    true
}

/// serde 默认值：脉络注入条数默认 3。
fn default_narrative_top_k() -> u32 {
    3
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
            narrative_weighted: true,
            narrative_top_k: 3,
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
// L1 渐进式摘要配置（B3）
// =========================================================

/// L1 摘要相关配置（`[l1]`）。
///
/// 职责:
/// - 承载渐进式摘要（B3）触发参数，长会话按段生成 L1、封存只摘要尾部。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1Config {
    /// 渐进式摘要配置（`[l1.progressive]`）
    #[serde(default)]
    pub progressive: L1ProgressiveConfig,
}

impl Default for L1Config {
    /// 创建默认 L1 配置。
    ///
    /// 返回:
    /// - 渐进式摘要默认关闭（回退 v1.6 整会话/按 utt 切分行为）。
    fn default() -> Self {
        Self {
            progressive: L1ProgressiveConfig::default(),
        }
    }
}

/// 渐进式摘要（B3）触发参数。
///
/// 职责:
/// - 长会话（消息数 > `msg_threshold` 或跨度 > `span_hours`）在封存时按段生成 L1，
///   每段独立成 L1（absorbed=0 入候选池），最后一段覆盖最新对话（尾部）。
/// - 短会话未达触发条件时回退 v1.6 行为（整会话摘要，不额外切段）。
///
/// 设计依据:
/// - 决策 D-V17-005：消息数>100 或跨度>24h（可配置）；段 L1 实时入缓冲；
///   L2 提取仍封存触发；封存只摘要尾部。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1ProgressiveConfig {
    /// 渐进式摘要总开关（默认 false——关闭时回退 v1.6 行为）。
    #[serde(default = "default_progressive_enabled")]
    pub enabled: bool,
    /// 消息数触发阈值（默认 100 条）：会话消息数超过此值触发分段。
    #[serde(default = "default_progressive_msg_threshold")]
    pub msg_threshold: u32,
    /// 时间跨度触发阈值（默认 24 小时）：首末消息跨度超过此值触发分段。
    #[serde(default = "default_progressive_span_hours")]
    pub span_hours: u32,
    /// 单段最大消息条数（默认 60）：分段时每段不超过此值，尾段覆盖最新消息。
    #[serde(default = "default_progressive_tail_msg_count")]
    pub tail_msg_count: u32,
}

/// serde 默认值：渐进式摘要默认关闭（保守，回退 v1.6 行为）。
fn default_progressive_enabled() -> bool {
    false
}

/// serde 默认值：消息数触发阈值 100 条。
fn default_progressive_msg_threshold() -> u32 {
    100
}

/// serde 默认值：时间跨度触发阈值 24 小时。
fn default_progressive_span_hours() -> u32 {
    24
}

/// serde 默认值：单段最大消息条数 60。
fn default_progressive_tail_msg_count() -> u32 {
    60
}

impl Default for L1ProgressiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            msg_threshold: 100,
            span_hours: 24,
            tail_msg_count: 60,
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
    /// 默认 10（窄切分、更细粒度分块）。
    pub theta_gap_minutes: u32,
    /// 单块最大消息条数：超过此条数强制切分。
    /// 默认 80（更大块、更少切分）。
    pub max_msgs_per_block: u32,
    /// 对话时检索返回的 utt 块数量（top_k）。
    /// 默认 3（top_k=1 会显著劣化事实召回）。
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
    /// - 启用全链路，10 分钟间隙 / 80 条上限切分。
    /// - 检索 top_k=3（top_k=1 会显著劣化，故保留 3），注入预算 1500 字符。
    /// - 白名单 = 角色类 persona（char/anim/oc/hist）。
    fn default() -> Self {
        Self {
            enabled: true,
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
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
    /// 密度聚类邻域相似度阈值 θ_nb：样本对相似度 ≥ 该值视为邻居。
    /// 默认 0.65（实证可使 59 事件细分出 ≥2 个行为簇）。
    pub theta_nb: f64,
    /// 核心样本最小邻居数 min_cluster_size（默认 3）。
    pub min_cluster_size: usize,
    /// 增量归簇阈值 θ_join（默认 0.7）。
    pub theta_join: f64,
    /// 反应通道权重 β1（默认 0.85，主导 paraphrase⊕attitude 语义）。
    pub beta1: f64,
    /// 情境通道权重 β2（默认 0.10，情境关键词参与比对）。
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
    /// 创建默认行为层配置。
    fn default() -> Self {
        Self {
            enabled: true,
            theta_nb: 0.65,
            min_cluster_size: 3,
            theta_join: 0.7,
            beta1: 0.85,
            beta2: 0.10,
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
// 风格统计配置（表达层 A3）
// =========================================================

/// 风格统计配置（`[style]`，表达层层次 2 自动学习）。
///
/// 职责:
/// - 控制表达层风格统计（五维指标 + 显著性检验 + 自动规则生成）的开关与阈值。
/// - `enabled=false` 时整链路关闭，prompt 注入回退 v1.6（无自动风格规则，
///   回归红线 1 锁定）。
/// - `auto_translate` 仅控制"LLM 离线翻译增强"是否启用；关闭或 LLM 不可用时
///   仅使用确定性模板拼接（D-V17-002 模板优先）。
///
/// 阈值说明（v3.1 §7.2 / D-V17-003）:
/// - `min_sample_count=200`：样本量低于此值时标注"数据不足"，不生成规则文本。
/// - 显著性判定：`|z| ≥ z_critical` 且 `频次 ≥ min_frequency` 且 `n_p ≥ min_sample_count`；
///   口癖词另加"相对超频比 > relative_boost_ratio"。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[style]` 表只写部分键时
///   缺失字段回退 `Default` 实现，避免解析失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleConfig {
    /// 风格统计总开关（默认 true —— 自动为主可配置）。
    /// `false` → 不统计、不生成规则、不注入，prompt 与 v1.6 语义等价。
    pub enabled: bool,
    /// LLM 离线翻译增强开关（默认 true —— 增强为可选，LLM 不可用静默降级模板）。
    /// `false` → 仅模板拼接（确定性可测、零 LLM 依赖）。
    pub auto_translate: bool,
    /// 样本量阈值 n_p（默认 200 条消息）。
    /// 低于此值时标注"数据不足"，不生成规则文本、不注入。
    pub min_sample_count: u32,
    /// 口癖词/话题词 Top-N（默认 10，文档范围 10~20）。
    pub top_n: u32,
    /// 口癖词相对超频比阈值（默认 2.0：persona 频率 / 全局频率 > 2）。
    pub relative_boost_ratio: f64,
    /// 显著项最小频次（默认 5 次：频次 ≥ 5 才参与显著性判定）。
    pub min_frequency: u32,
    /// z 临界值（默认 2.0：`|z| ≥ 2` 判定统计显著）。
    pub z_critical: f64,
}

impl Default for StyleConfig {
    /// 创建默认风格统计配置。
    ///
    /// 返回:
    /// - 默认开启全链路（自动为主可配置），`auto_translate=true`（LLM 增强可选）。
    /// - 显著性判定阈值：|z|≥2 且频次≥5 且 n_p≥200；口癖词相对超频比>2。
    fn default() -> Self {
        Self {
            enabled: true,
            auto_translate: true,
            min_sample_count: 200,
            top_n: 10,
            relative_boost_ratio: 2.0,
            min_frequency: 5,
            z_critical: 2.0,
        }
    }
}

// =========================================================
// 弱反馈环配置（H2，自我修正闭环）
// =========================================================

/// 弱反馈环配置（`[feedback]`，S2/S3 自我修正闭环 H2）。
///
/// 职责:
/// - 控制 S2 纠正 / S3 继续发言弱信号的采集与校准行为。
/// - `auto_apply_weak_feedback` 默认关闭：弱信号只写入 `feedback_log`
///   （审计），不自动修改任何规则/画像（回归红线 5）。
/// - 检测窗口（`correction_window_ms` / `continue_window_ms`）：用户消息与
///   上一条助手回复间隔在此窗口内才判为弱信号；超时（间隔更大）不累积。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[feedback]` 表只写部分键时
///   缺失字段回退 `Default` 实现，避免解析失败。
/// - 关闭开关不破坏主流程：检测/写入失败均静默降级，不影响对话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackConfig {
    /// S2/S3 弱反馈环总开关（默认 true —— 自动采集可配置）。
    /// `false` → 不检测、不写 feedback_log，行为回退 v1.6。
    pub enabled: bool,
    /// 弱反馈是否自动应用（默认 false）。
    /// `false` → 弱信号仅写 feedback_log（审计），不触发候选复审/趋势统计的落库，
    ///          规则与画像零自动修改（回归红线 5）。
    /// `true` → 检测到 S2 纠正/ S3 趋势异常时标记候选复审（不自动覆盖规则本身）。
    pub auto_apply_weak_feedback: bool,
    /// S2 纠正前缀检测窗口（毫秒，默认 60000 = 60s）。
    /// 用户消息与上一条助手回复间隔 ≤ 此值且命中纠正前缀 → S2 纠正信号。
    pub correction_window_ms: u64,
    /// S3 继续发言检测窗口（毫秒，默认 60000 = 60s）。
    /// 用户消息与上一条助手回复间隔 ≤ 此值且非纠正 → S3 继续信号。
    pub continue_window_ms: u64,
    /// 同一目标重复反馈的去重窗口（毫秒，默认 30000 = 30s）。
    /// 窗口内同一 persona+信号+目标不重复写入（避免短时连续消息累积重复反馈）。
    pub dedup_window_ms: u64,
    /// S3 趋势统计滑动窗口大小（默认 20 次）。
    /// 取最近 N 个回合的继续/不继续结果做趋势判定。
    pub s3_trend_window: u32,
    /// S3 标记复审所需连续"继续"命中数（默认 5）。
    pub s3_continue_trigger: u32,
    /// S3 标记复审所需随后的连续"不继续"数（默认 4）。
    pub s3_stop_trigger: u32,
}

impl Default for FeedbackConfig {
    /// 创建默认弱反馈环配置。
    ///
    /// 返回:
    /// - 默认开启采集（自动为主可配置），`auto_apply_weak_feedback=false`（保守）。
    /// - 检测窗口 60s（S2/S3），去重窗口 30s。
    /// - S3 趋势窗口 20 次，连续 ≥5 次继续后 4 次不继续 → 标记复审。
    fn default() -> Self {
        Self {
            enabled: true,
            auto_apply_weak_feedback: false,
            correction_window_ms: 60_000,
            continue_window_ms: 60_000,
            dedup_window_ms: 30_000,
            s3_trend_window: 20,
            s3_continue_trigger: 5,
            s3_stop_trigger: 4,
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

        // 行为层默认
        assert!(cfg.behavior.enabled);
        assert!((cfg.behavior.theta_nb - 0.65).abs() < f64::EPSILON);
        assert_eq!(cfg.behavior.min_cluster_size, 3);
        assert!((cfg.behavior.theta_join - 0.7).abs() < f64::EPSILON);
        assert!((cfg.behavior.beta1 - 0.85).abs() < f64::EPSILON);
        assert!((cfg.behavior.beta2 - 0.10).abs() < f64::EPSILON);
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
        // 风格统计配置组（表达层 A3）
        assert!(json.contains("style"));
        assert!(json.contains("auto_translate"));
        assert!(json.contains("min_sample_count"));
        assert!(json.contains("relative_boost_ratio"));
        assert!(json.contains("z_critical"));
        // 弱反馈环配置组（H2）
        assert!(json.contains("feedback"));
        assert!(json.contains("auto_apply_weak_feedback"));
        assert!(json.contains("s3_trend_window"));
    }

    #[test]
    fn style_config_defaults() {
        let cfg = RamariaConfig::default();
        // 默认开启全链路（自动为主可配置）；关闭时回退 v1.6 prompt
        assert!(cfg.style.enabled);
        assert!(cfg.style.auto_translate);
        // 显著性判定阈值（D-V17-003 / v3.1 §7.2）
        assert_eq!(cfg.style.min_sample_count, 200);
        assert_eq!(cfg.style.top_n, 10);
        assert!((cfg.style.relative_boost_ratio - 2.0).abs() < f64::EPSILON);
        assert_eq!(cfg.style.min_frequency, 5);
        assert!((cfg.style.z_critical - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn feedback_config_defaults() {
        let cfg = RamariaConfig::default();
        // 默认开启采集，auto_apply 默认 false（回归红线 5：关闭时零自动修改）
        assert!(cfg.feedback.enabled);
        assert!(
            !cfg.feedback.auto_apply_weak_feedback,
            "auto_apply 默认关闭"
        );
        // 检测窗口 60s / 去重窗口 30s
        assert_eq!(cfg.feedback.correction_window_ms, 60_000);
        assert_eq!(cfg.feedback.continue_window_ms, 60_000);
        assert_eq!(cfg.feedback.dedup_window_ms, 30_000);
        // S3 趋势窗口 20，连续 ≥5 继续后 4 次不继续
        assert_eq!(cfg.feedback.s3_trend_window, 20);
        assert_eq!(cfg.feedback.s3_continue_trigger, 5);
        assert_eq!(cfg.feedback.s3_stop_trigger, 4);
    }

    #[test]
    fn feedback_config_disabled_zero_auto_modify() {
        // 关闭 auto_apply：弱信号不自动修改规则/画像
        let mut cfg = RamariaConfig::default();
        cfg.feedback.auto_apply_weak_feedback = false;
        assert!(!cfg.feedback.auto_apply_weak_feedback);
        // 其余参数保持默认可独立配置
        assert_eq!(cfg.feedback.correction_window_ms, 60_000);
    }

    #[test]
    fn feedback_config_toml_partial_override() {
        // 只配置部分键 → 缺失字段回退默认值
        let toml_text = r#"
[feedback]
enabled = false
"#;
        let cfg: RamariaConfig = toml::from_str(toml_text).expect("部分配置应可解析");
        assert!(!cfg.feedback.enabled);
        // 未配置字段使用默认值
        assert!(!cfg.feedback.auto_apply_weak_feedback);
        assert_eq!(cfg.feedback.continue_window_ms, 60_000);
    }

    // =========================================================
    // 注入层运行时间门（InjectionGate，探针消融专用）
    // =========================================================

    /// 默认全开：无覆盖时对话管线注入行为与既有版本一致（回归红线）。
    #[test]
    fn injection_gate_defaults_all_on() {
        let g = InjectionGate::default();
        assert!(g.behavior);
        assert!(g.knowledge);
        assert!(g.speaking_style);
        assert!(g.examples);
        assert!(g.utt);
        assert!(g.narrative);
        assert!(g.bridge);
        assert!(g.memory_rag);
        let cfg = RamariaConfig::default();
        assert!(
            cfg.injection.behavior && cfg.injection.memory_rag,
            "默认配置闸门全开"
        );
    }

    /// 全关（B0 基座）与全开互为补集。
    #[test]
    fn injection_gate_off_is_complement_of_on() {
        let on = InjectionGate::all_on();
        let off = InjectionGate::all_off();
        assert!(!off.behavior && !off.memory_rag && !off.narrative);
        assert!(on.behavior && on.memory_rag && on.narrative);
    }

    /// 闸门不写入持久化：JSON/TOML 序列化不含 injection 键，
    /// 反序列化回退默认全开（保持配置文件与 DB 键集稳定）。
    #[test]
    fn injection_gate_is_memory_only_not_persisted() {
        let mut cfg = RamariaConfig::default();
        cfg.injection.memory_rag = false;
        cfg.injection.behavior = false;

        // JSON 通道（backend_config / 信封等）：顶层无 `injection` 键。
        // 注意不能用裸 "injection" 断言（`online_memory_injection` 亦含该子串）。
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed_json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            parsed_json.get("injection").is_none(),
            "injection 闸门不得序列化到 JSON"
        );
        let back_json: RamariaConfig = serde_json::from_str(&json).unwrap();
        assert!(back_json.injection.memory_rag, "反序列化后闸门回退全开");

        // TOML 通道（config.toml / ConfigSyncService）：无 `[injection]` 表。
        let toml_text = toml::to_string(&cfg).unwrap();
        assert!(
            !toml_text.contains("[injection]"),
            "injection 闸门不得序列化到 TOML"
        );
        let back_toml: RamariaConfig = toml::from_str(&toml_text).unwrap();
        assert!(back_toml.injection.behavior, "TOML 反序列化后闸门回退全开");
    }

    #[test]
    fn style_config_disabled_falls_back_to_v16() {
        // 关闭风格统计：整链路回退 v1.6（prompt 不含自动风格规则）
        let mut cfg = RamariaConfig::default();
        cfg.style.enabled = false;
        assert!(!cfg.style.enabled);
        // 其他阈值保持默认可独立配置
        assert_eq!(cfg.style.min_sample_count, 200);
    }

    // =========================================================
    // v1.4 新增配置组测试（[utt] / [examples] / [bridge]）
    // =========================================================

    #[test]
    fn utt_config_defaults() {
        let cfg = RamariaConfig::default();

        // 开关默认开启
        assert!(cfg.utt.enabled);
        // 切分参数（10/80）
        assert_eq!(cfg.utt.theta_gap_minutes, 10);
        assert_eq!(cfg.utt.max_msgs_per_block, 80);
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
        assert_eq!(cfg.utt.theta_gap_minutes, 10);
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
        assert_eq!(cfg.utt.theta_gap_minutes, 10);
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
