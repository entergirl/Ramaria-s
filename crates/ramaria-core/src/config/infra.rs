//! crates/ramaria-core/src/config/infra.rs - Ramaria 基础设施配置模块
//!
//! 设计特点:
//! - 定义后端选择、日志与杂项配置
//! - 定义 LLM 响应缓存与淘汰策略
//! - 定义嵌入设备与嵌入运行时配置
//! - 各配置组提供稳定默认值
//! - 支持 serde，供配置文件与 DB settings 共享

use serde::{Deserialize, Serialize};

use super::RamariaConfig;
use crate::types::LlmProvider;

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
#[serde(default)]
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

/// 杂项配置（预留扩展位）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MiscConfig {
    // 预留：未来可扩展天气查询城市、通知偏好等轻量选项
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
    /// 强制使用 CPU 推理（最保守；默认设备为 `Auto`，见 `EmbeddingDevice::default()`）。
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
