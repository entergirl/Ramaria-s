//! crates/ramaria-core/src/types/backend.rs - Ramaria 后端配置数据类型模块
//!
//! 设计特点:
//! - 定义 LLM provider 枚举与模型能力描述
//! - BackendConfig 汇总非敏感后端配置
//! - PrivacyConsent 记录按 provider + base_url 的隐私确认
//! - LlmProvider 提供 as_str / is_online / Display 等辅助
//! - 所有类型支持 serde，不承载 API key 明文

use serde::{Deserialize, Serialize};

use super::now_ms;

// =========================================================
// 后端配置与隐私确认
// =========================================================

/// LLM Provider 标识。
///
/// 职责:
/// - 枚举 支持的 LLM provider。
/// - 区分本地和线上 provider，决定是否需要隐私确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LlmProvider {
    // 序列化统一为 `lm_studio`（snake_case，与 CLI/前端/DB 一致）；
    // 反序列化同时接受历史写法 `lm-studio`（v1.2/v1.3 模板与文档使用连字符，
    // 真实用户 config.toml 中可能仍是该写法，必须兼容以免升级后配置被当损坏）。
    #[serde(rename = "lm_studio", alias = "lm-studio")]
    LmStudio,
    DeepSeek,
    OpenAI,
}

impl LlmProvider {
    /// 返回 provider 的稳定字符串标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LmStudio => "lm_studio",
            Self::DeepSeek => "deepseek",
            Self::OpenAI => "openai",
        }
    }
    /// 是否为线上 provider（需要隐私确认）。
    pub fn is_online(&self) -> bool {
        matches!(self, Self::DeepSeek | Self::OpenAI)
    }
}

impl Default for LlmProvider {
    /// 默认 provider 为本地 LM Studio（与 `BackendSelection::default()` 一致，
    /// 供 serde `#[serde(default)]` 在字段缺失时回退）。
    fn default() -> Self {
        Self::LmStudio
    }
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 模型能力描述。
///
/// 职责:
/// - 描述某个 provider/model 的上下文长度、输出限制和协议能力。
/// - 供配置向导、运行时校验和 UI 展示使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapability {
    pub provider: LlmProvider,
    pub model_id: String,
    pub base_url: String,
    pub supports_streaming: bool,
    pub supports_json_mode: bool,
    pub context_window: u32,
    pub max_output_tokens: u32,
}

/// 非敏感后端配置（API key 不在此结构中）。
///
/// 职责:
/// - 保存 provider、model、base_url、embedding 模型和生成参数。
/// - 携带当前模型能力描述。
///
/// 安全约束:
/// - API key 不允许进入此结构。
/// - 线上 provider 的密钥必须从 OS keychain 读取。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub provider: LlmProvider,
    pub base_url: String,
    /// embedding 模型标识（远程模型 ID）
    pub embedding_model_id: Option<String>,
    /// embedding 模型本地路径（与 `base_url` 对应，同为 locator）
    pub embedding_model_path: Option<String>,
    pub temperature: f64,
    pub max_tokens: u32,
    /// 模型能力描述——`capability.model_id` 为 model_id 单一来源
    pub capability: ModelCapability,
}

impl BackendConfig {
    /// 根据 provider + base_url + model_id 创建配置，自动填充合理的默认值。
    ///
    /// 职责:
    /// - 消除 setup.rs 和 config.rs 中重复的 BackendConfig 构造逻辑。
    /// - 为各 provider 提供一致的默认 temperature / max_tokens / context_window。
    ///
    /// 参数:
    /// - `provider`: LLM 提供商。
    /// - `base_url`: API 基础地址。
    /// - `model_id`: 模型标识（LM Studio 可为空字符串）。
    ///
    /// 返回:
    /// - 带合理默认值的 BackendConfig 实例。
    pub fn new_with_defaults(provider: LlmProvider, base_url: String, model_id: String) -> Self {
        let is_lm_studio = provider == LlmProvider::LmStudio;

        Self {
            provider,
            base_url: base_url.clone(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.3,
            max_tokens: 2048,
            capability: ModelCapability {
                provider,
                model_id,
                base_url,
                supports_streaming: true,
                supports_json_mode: !is_lm_studio,
                context_window: if is_lm_studio { 4096 } else { 65536 },
                max_output_tokens: 8192,
            },
        }
    }

    /// LM Studio 默认配置。
    ///
    /// 返回:
    /// - 指向 `http://localhost:1234/v1` 的本地 OpenAI-compatible 配置。
    pub fn lm_studio_default() -> Self {
        Self {
            provider: LlmProvider::LmStudio,
            base_url: "http://localhost:1234/v1".to_string(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.3,
            max_tokens: 1024,
            capability: ModelCapability {
                provider: LlmProvider::LmStudio,
                model_id: String::new(),
                base_url: "http://localhost:1234/v1".to_string(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
        }
    }

    /// DeepSeek 默认配置。
    ///
    /// 返回:
    /// - 使用 DeepSeek 官方 OpenAI-compatible base URL 的线上配置。
    pub fn deepseek_default() -> Self {
        Self {
            provider: LlmProvider::DeepSeek,
            base_url: "https://api.deepseek.com/v1".to_string(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.3,
            max_tokens: 2048,
            capability: ModelCapability {
                provider: LlmProvider::DeepSeek,
                model_id: "deepseek-chat".to_string(),
                base_url: "https://api.deepseek.com/v1".to_string(),
                supports_streaming: true,
                supports_json_mode: true,
                context_window: 65536,
                max_output_tokens: 8192,
            },
        }
    }

    /// OpenAI 默认配置。
    ///
    /// 返回:
    /// - 使用 OpenAI 官方 base URL 的线上配置。
    pub fn openai_default() -> Self {
        Self {
            provider: LlmProvider::OpenAI,
            base_url: "https://api.openai.com/v1".to_string(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.3,
            max_tokens: 2048,
            capability: ModelCapability {
                provider: LlmProvider::OpenAI,
                model_id: "gpt-4o".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                supports_streaming: true,
                supports_json_mode: true,
                context_window: 128000,
                max_output_tokens: 16384,
            },
        }
    }
}

/// 隐私确认记录。
///
/// 职责:
/// - 记录用户是否允许某个线上 provider/base_url 接收对话和记忆上下文。
/// - 区分临时确认和跨重启持久确认。
///
/// 粒度:
/// - 每条记录对应一个 provider + base_url 组合。
/// - provider 或 base_url 改变时应重新确认。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyConsent {
    pub provider: LlmProvider,
    pub base_url: String,
    /// 确认时间（Unix 毫秒）
    pub timestamp: i64,
    /// 是否持久化（跨重启无需重新确认）
    pub persistent: bool,
}

impl PrivacyConsent {
    /// 创建一条 provider + base_url 粒度的隐私确认记录。
    pub fn new(provider: LlmProvider, base_url: String, persistent: bool) -> Self {
        Self {
            provider,
            base_url,
            timestamp: now_ms(),
            persistent,
        }
    }
}
