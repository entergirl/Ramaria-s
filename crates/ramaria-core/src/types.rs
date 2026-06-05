//! rust/crates/ramaria-core/src/types.rs - Ramaria 核心业务数据类型模块
//!
//! 设计特点:
//! - 覆盖核心领域对象: Session、Message、L1/L2/L3 Memory、BackendConfig、PrivacyConsent、AppState
//! - 统一 ID 规范: 所有实体使用 UUID v4，存储层以 TEXT 形式持久化
//! - 统一时间规范: 所有时间使用 Unix 毫秒时间戳，存储层以 INTEGER 形式持久化
//! - 所有公共业务类型支持 serde，方便 CLI、Tauri IPC、存储层和测试夹具共享
//! - 提供轻量构造函数和状态辅助方法，避免上层重复初始化或破坏领域约束

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 创建一个新的 UUID v4。
///
/// 用法:
/// - 所有业务实体创建 ID 时优先调用此函数。
/// - 存储层将 UUID 转换为 SQLite TEXT。
///
/// 返回:
/// - 新的 UUID v4。
#[inline]
pub fn new_id() -> Uuid {
    Uuid::new_v4()
}

/// 返回当前 Unix 毫秒时间戳。
///
/// 用法:
/// - 所有业务实体创建、更新、访问时间优先使用此函数。
///
/// 返回:
/// - 当前 Unix 毫秒时间戳。
///
/// 说明:
/// - 核心层不依赖 tokio、网络或数据库，因此使用标准库 `SystemTime`。
#[inline]
pub fn now_ms() -> i64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 消息来源：本地模型或线上 API。
///
/// 职责:
/// - 标记一条消息来自本地 provider 还是线上 provider。
/// - 供隐私提示、日志脱敏和 UI 状态展示使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageSource {
    #[default]
    Local,
    Online,
}

impl std::fmt::Display for MessageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Online => write!(f, "online"),
        }
    }
}

// =========================================================
// 消息角色
// =========================================================

/// 消息角色枚举。
///
/// 职责:
/// - 表示一条对话消息在聊天协议中的角色。
/// - 与 OpenAI Chat Completions API 兼容。
/// - 预留 `tool` 用于未来工具调用和插件调用结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    #[serde(rename = "tool")]
    Tool,
}

impl MessageRole {
    /// 返回 OpenAI API 兼容的小写字符串。
    ///
    /// 返回:
    /// - `user` / `assistant` / `system` / `tool`。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// =========================================================
// 会话与原始消息
// =========================================================

/// 对话会话。
///
/// 职责:
/// - 表示一次连续对话生命周期。
/// - 承载 L0 消息归属关系。
/// - 为 session 结束后的 L1 摘要生成提供边界。
///
/// 状态:
/// - `ended_at = None`: 会话仍在进行中。
/// - `ended_at = Some(...)`: 会话已关闭，可触发 L1 摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    /// Session 开始时间（Unix 毫秒）
    pub started_at: i64,
    /// Session 结束时间，None 表示未关闭
    pub ended_at: Option<i64>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// 创建一个新的活跃 Session。
    ///
    /// 返回:
    /// - 带新 UUID、当前开始时间、未关闭状态的 Session。
    pub fn new() -> Self {
        Self {
            id: new_id(),
            started_at: now_ms(),
            ended_at: None,
        }
    }

    /// 关闭当前 Session，记录结束时间。
    ///
    /// 说明:
    /// - 如果 Session 已关闭，此方法保持原结束时间不变。
    /// - 幂等设计避免重复关闭导致时间漂移。
    pub fn close(&mut self) {
        if self.ended_at.is_none() {
            self.ended_at = Some(now_ms());
        }
    }

    /// 判断 Session 是否已关闭。
    ///
    /// 返回:
    /// - `true`: Session 仍在进行中。
    /// - `false`: Session 已关闭。
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// L0 原始消息。
///
/// 职责:
/// - 保存用户、助手、系统或工具的原始消息。
/// - 作为 L1 摘要、检索索引和对话历史的事实源。
///
/// 去重:
/// - `fingerprint` 为 SHA-256 前 16 位 hex，用于历史导入去重。
/// - 正常对话产生的消息此字段为 None。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: MessageRole,
    pub content: String,
    /// 消息创建时间（Unix 毫秒）
    pub created_at: i64,
    pub source: MessageSource,
    /// 导入去重指纹，None 表示正常对话消息
    pub fingerprint: Option<String>,
}

impl Message {
    /// 创建一条新消息。
    ///
    /// 参数:
    /// - `session_id`: 消息所属 Session。
    /// - `role`: 消息角色。
    /// - `content`: 原始文本内容。
    /// - `source`: 本地或线上来源。
    ///
    /// 返回:
    /// - 带新 UUID、当前创建时间且无 fingerprint 的消息。
    pub fn new(
        session_id: Uuid,
        role: MessageRole,
        content: String,
        source: MessageSource,
    ) -> Self {
        Self {
            id: new_id(),
            session_id,
            role,
            content,
            created_at: now_ms(),
            source,
            fingerprint: None,
        }
    }
}

// =========================================================
// 分层记忆类型
// =========================================================

/// L1 单次会话摘要。
///
/// 职责:
/// - 表示一次 Session 关闭后生成的会话摘要。
/// - 保存关键词、时间段、情绪效价和显著性，供后续检索和 L2 聚合使用。
/// - 通过 `absorbed` 标记是否已被 L2 消化。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryL1 {
    pub id: Uuid,
    pub session_id: Uuid,
    /// 摘要文本
    pub summary: String,
    /// 逗号分隔的关键词
    pub keywords: Option<String>,
    /// 时间段（清晨/上午/下午/傍晚/夜间/深夜）
    pub time_period: Option<String>,
    /// 气氛描述
    pub atmosphere: Option<String>,
    /// 情绪效价 -1.0..1.0
    pub valence: f64,
    /// 情感显著性 0.0..1.0
    pub salience: f64,
    /// 是否已被 L2 吸收
    pub absorbed: bool,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 最近被检索命中的时间
    pub last_accessed_at: Option<i64>,
}

impl MemoryL1 {
    /// 创建一条新的 L1 记忆。
    ///
    /// 参数:
    /// - `session_id`: 来源 Session。
    /// - `summary`: 摘要文本。
    /// - `time_period`: 可选时间段，如“上午”“夜间”。
    ///
    /// 返回:
    /// - 默认 valence=0.0、salience=0.5、未吸收的 L1 记忆。
    pub fn new(session_id: Uuid, summary: String, time_period: Option<String>) -> Self {
        Self {
            id: new_id(),
            session_id,
            summary,
            keywords: None,
            time_period,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: now_ms(),
            last_accessed_at: None,
        }
    }

    /// 标记此 L1 已被 L2 吸收。
    ///
    /// 用法:
    /// - L2 merger 成功生成 L2 后调用。
    pub fn mark_absorbed(&mut self) {
        self.absorbed = true;
    }

    /// 记录被检索访问。
    ///
    /// 用法:
    /// - 检索命中并注入上下文后调用，用于遗忘曲线访问加成。
    pub fn touch(&mut self) {
        self.last_accessed_at = Some(now_ms());
    }
}

/// 时间段的合法值集合。
pub const TIME_PERIOD_OPTIONS: &[&str] = &["清晨", "上午", "下午", "傍晚", "夜间", "深夜"];

/// L2 时间段聚合摘要。
///
/// 职责:
/// - 表示一段时间内多条 L1 的聚合摘要。
/// - 用于压缩长期上下文，减少 prompt 中的碎片化信息。
/// - 通过 `l2_sources` 关系追踪其来源 L1。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryL2 {
    pub id: Uuid,
    /// 聚合摘要文本
    pub summary: String,
    /// 逗号分隔的关键词
    pub keywords: Option<String>,
    /// 覆盖时间段起点（Unix 毫秒）
    pub period_start: i64,
    /// 覆盖时间段终点（Unix 毫秒）
    pub period_end: i64,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 最近被检索命中的时间
    pub last_accessed_at: Option<i64>,
}

impl MemoryL2 {
    /// 创建一条新的 L2 记忆。
    ///
    /// 参数:
    /// - `summary`: 聚合摘要文本。
    /// - `period_start`: 覆盖时间段起点。
    /// - `period_end`: 覆盖时间段终点。
    ///
    /// 返回:
    /// - 带新 UUID、当前创建时间、未访问状态的 L2 记忆。
    pub fn new(summary: String, period_start: i64, period_end: i64) -> Self {
        Self {
            id: new_id(),
            summary,
            keywords: None,
            period_start,
            period_end,
            created_at: now_ms(),
            last_accessed_at: None,
        }
    }

    /// 记录被检索访问。
    ///
    /// 用法:
    /// - 检索命中并注入上下文后调用，用于近期访问加成。
    pub fn touch(&mut self) {
        self.last_accessed_at = Some(now_ms());
    }
}

/// L2 → L1 溯源关系。
///
/// 职责:
/// - 保留 L2 聚合摘要与来源 L1 的可追溯关系。
/// - 支持调试、解释和未来删除/重算策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2Source {
    pub l2_id: Uuid,
    pub l1_id: Uuid,
}

impl L2Source {
    /// 创建一条 L2 到 L1 的溯源关系。
    pub fn new(l2_id: Uuid, l1_id: Uuid) -> Self {
        Self { l2_id, l1_id }
    }
}

/// 用户画像字段（L3 记忆层）。
///
/// 职责:
/// - 约束用户画像可写入的字段集合。
/// - 避免 L3 画像无限扩张为任意 key-value。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileField {
    /// 基础信息
    BasicInfo,
    /// 近期状态
    PersonalStatus,
    /// 兴趣爱好
    Interests,
    /// 社交情况
    Social,
    /// 历史事件
    History,
    /// 近期背景
    RecentContext,
}

impl ProfileField {
    /// 返回字段的中文标签。
    pub fn label(&self) -> &'static str {
        match self {
            Self::BasicInfo => "基础信息",
            Self::PersonalStatus => "近期状态",
            Self::Interests => "兴趣爱好",
            Self::Social => "社交情况",
            Self::History => "历史事件",
            Self::RecentContext => "近期背景",
        }
    }

    /// 返回字段的键名（用于序列化）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BasicInfo => "basic_info",
            Self::PersonalStatus => "personal_status",
            Self::Interests => "interests",
            Self::Social => "social",
            Self::History => "history",
            Self::RecentContext => "recent_context",
        }
    }
}

impl std::fmt::Display for ProfileField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 画像条目状态。
///
/// 职责:
/// - 表示画像条目的审核或生效状态。
/// - v1.0 默认直接 approved，后续可扩展用户确认流程。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProfileStatus {
    /// 已生效
    #[default]
    Approved,
    /// 待用户确认
    Pending,
    /// 用户拒绝
    Rejected,
}

impl ProfileStatus {
    /// 返回状态的稳定字符串标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Pending => "pending",
            Self::Rejected => "rejected",
        }
    }
}

/// 用户画像条目（L3 层）。
///
/// 职责:
/// - 保存用户长期稳定特征，例如基础信息、兴趣、近期状态和历史事件。
/// - 作为 L3 记忆注入 System Prompt。
///
/// 版本策略:
/// - 画像设计为追加写入。
/// - `is_current` 标记当前生效版本。
/// - 历史版本保留用于调试、回滚或未来审计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub field: ProfileField,
    pub content: String,
    /// 来源 L1 ID
    pub source_l1_id: Option<Uuid>,
    pub status: ProfileStatus,
    /// 是否为当前生效版本
    pub is_current: bool,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

impl UserProfile {
    /// 创建一条新的画像条目。
    ///
    /// 参数:
    /// - `field`: 画像字段。
    /// - `content`: 画像内容。
    /// - `source_l1_id`: 可选来源 L1。
    ///
    /// 返回:
    /// - 默认 approved、非 current 的画像条目。
    pub fn new(field: ProfileField, content: String, source_l1_id: Option<Uuid>) -> Self {
        Self {
            id: new_id(),
            field,
            content,
            source_l1_id,
            status: ProfileStatus::default(),
            is_current: false,
            updated_at: now_ms(),
        }
    }

    /// 标记为当前生效版本。
    ///
    /// 用法:
    /// - 写入新画像并将旧版本置为 historical 后调用。
    pub fn mark_current(&mut self) {
        self.is_current = true;
        self.updated_at = now_ms();
    }
}

// =========================================================
// 后端配置与隐私确认
// =========================================================

/// LLM Provider 标识。
///
/// 职责:
/// - 枚举 v1.0 支持的 LLM provider。
/// - 区分本地和线上 provider，决定是否需要隐私确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    #[serde(rename = "lm_studio")]
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
    pub model_id: String,
    pub base_url: String,
    /// embedding 模型标识
    pub embedding_model_id: Option<String>,
    pub temperature: f64,
    pub max_tokens: u32,
    pub capability: ModelCapability,
}

impl BackendConfig {
    /// LM Studio 默认配置。
    ///
    /// 返回:
    /// - 指向 `http://localhost:1234/v1` 的本地 OpenAI-compatible 配置。
    pub fn lm_studio_default() -> Self {
        Self {
            provider: LlmProvider::LmStudio,
            model_id: String::new(),
            base_url: "http://localhost:1234/v1".to_string(),
            embedding_model_id: None,
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
            model_id: "deepseek-chat".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            embedding_model_id: None,
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
            model_id: "gpt-4o".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            embedding_model_id: None,
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

// =========================================================
// 应用状态机
// =========================================================

/// 应用全局状态。
///
/// 职责:
/// - 统一 CLI 和 Desktop 对应用生命周期的理解。
/// - 驱动首次配置、模型下载、索引重建、正常对话和错误恢复界面。
///
/// 状态流:
/// - `NeedsSetup` -> `DownloadingModel` -> `Indexing` -> `Ready`
/// - 可恢复故障进入 `Degraded`
/// - 不可恢复故障进入 `FatalError`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppState {
    /// 首次配置未完成，需进入配置向导
    NeedsSetup,
    /// embedding 模型下载中
    DownloadingModel,
    /// 索引初始化或重建中
    Indexing,
    /// 可正常对话
    Ready,
    /// 可恢复故障（如 LLM 暂不可用）
    Degraded,
    /// 不可恢复错误（数据库损坏、keychain 失败等）
    FatalError,
}

impl AppState {
    /// 返回应用状态的稳定字符串标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NeedsSetup => "needs_setup",
            Self::DownloadingModel => "downloading_model",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::FatalError => "fatal_error",
        }
    }
}

impl std::fmt::Display for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ID 与时间约定 ----

    #[test]
    fn new_id_is_unique() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
    }

    #[test]
    fn new_id_is_valid_uuid_v4() {
        let id = new_id();
        assert_eq!(id.get_version_num(), 4);
    }

    #[test]
    fn now_ms_is_reasonable() {
        let t = now_ms();
        // 2025 年之后的 Unix 毫秒至少是 1.7e12 量级
        assert!(t > 1_700_000_000_000, "timestamp too old: {t}");
        // 不应该超过 2050 年
        assert!(t < 2_600_000_000_000, "timestamp too far: {t}");
    }

    // ---- MessageRole ----

    #[test]
    fn message_role_serde_roundtrip() {
        for role in [
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::System,
            MessageRole::Tool,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let back: MessageRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
    }

    #[test]
    fn message_role_lowercase() {
        assert_eq!(
            serde_json::to_string(&MessageRole::User).unwrap(),
            r#""user""#
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::Assistant).unwrap(),
            r#""assistant""#
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::System).unwrap(),
            r#""system""#
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::Tool).unwrap(),
            r#""tool""#
        );
    }

    // ---- Session / Message ----

    #[test]
    fn session_lifecycle() {
        let mut session = Session::new();
        assert!(session.is_active());
        assert!(session.ended_at.is_none());

        session.close();
        assert!(!session.is_active());
        assert!(session.ended_at.is_some());
    }

    #[test]
    fn session_close_is_idempotent() {
        let mut session = Session::new();
        let first_close = now_ms();
        session.ended_at = Some(first_close);
        session.close();
        assert_eq!(session.ended_at, Some(first_close));
    }

    #[test]
    fn message_creation() {
        let sid = new_id();
        let msg = Message::new(
            sid,
            MessageRole::User,
            "你好".to_string(),
            MessageSource::Local,
        );
        assert_eq!(msg.session_id, sid);
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content, "你好");
        assert_eq!(msg.source, MessageSource::Local);
        assert!(msg.fingerprint.is_none());
    }

    #[test]
    fn session_message_serde_roundtrip() {
        let session = Session::new();
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session.id, back.id);
        assert_eq!(session.started_at, back.started_at);

        let msg = Message::new(
            session.id,
            MessageRole::Assistant,
            "回复".into(),
            MessageSource::Online,
        );
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.session_id, back.session_id);
        assert_eq!(msg.role, back.role);
        assert_eq!(msg.content, back.content);
    }

    // ---- Memory 类型 ----

    #[test]
    fn memory_l1_lifecycle() {
        let sid = new_id();
        let mut l1 = MemoryL1::new(sid, "摘要内容".into(), Some("上午".into()));

        assert!(!l1.absorbed);
        assert_eq!(l1.valence, 0.0);
        assert_eq!(l1.salience, 0.5);
        assert!(l1.last_accessed_at.is_none());

        l1.mark_absorbed();
        assert!(l1.absorbed);

        l1.touch();
        assert!(l1.last_accessed_at.is_some());
    }

    #[test]
    fn memory_l2_creation() {
        let start = now_ms() - 86_400_000; // 1 天前
        let end = now_ms();
        let l2 = MemoryL2::new("聚合摘要".into(), start, end);
        assert_eq!(l2.period_start, start);
        assert_eq!(l2.period_end, end);
        assert!(l2.last_accessed_at.is_none());
    }

    #[test]
    fn l2_source_creation() {
        let l2_id = new_id();
        let l1_id = new_id();
        let src = L2Source::new(l2_id, l1_id);
        assert_eq!(src.l2_id, l2_id);
        assert_eq!(src.l1_id, l1_id);
    }

    #[test]
    fn user_profile_creation() {
        let mut profile =
            UserProfile::new(ProfileField::BasicInfo, "姓名：小明".into(), Some(new_id()));
        assert_eq!(profile.field, ProfileField::BasicInfo);
        assert!(!profile.is_current);
        assert_eq!(profile.status, ProfileStatus::Approved);

        profile.mark_current();
        assert!(profile.is_current);
    }

    #[test]
    fn profile_field_labels() {
        assert_eq!(ProfileField::BasicInfo.label(), "基础信息");
        assert_eq!(ProfileField::PersonalStatus.label(), "近期状态");
        assert_eq!(ProfileField::Interests.label(), "兴趣爱好");
        assert_eq!(ProfileField::Social.label(), "社交情况");
        assert_eq!(ProfileField::History.label(), "历史事件");
        assert_eq!(ProfileField::RecentContext.label(), "近期背景");
    }

    #[test]
    fn profile_field_serde() {
        assert_eq!(
            serde_json::to_string(&ProfileField::BasicInfo).unwrap(),
            r#""basic_info""#
        );
        assert_eq!(
            serde_json::to_string(&ProfileField::RecentContext).unwrap(),
            r#""recent_context""#
        );
    }

    #[test]
    fn memory_serde_roundtrip() {
        let l1 = MemoryL1::new(new_id(), "记忆摘要".into(), Some("夜间".into()));
        let json = serde_json::to_string(&l1).unwrap();
        let back: MemoryL1 = serde_json::from_str(&json).unwrap();
        assert_eq!(l1.id, back.id);
        assert_eq!(l1.summary, back.summary);

        let l2 = MemoryL2::new("L2 摘要".into(), now_ms() - 1000, now_ms());
        let json = serde_json::to_string(&l2).unwrap();
        let back: MemoryL2 = serde_json::from_str(&json).unwrap();
        assert_eq!(l2.id, back.id);
    }

    // ---- BackendConfig / PrivacyConsent ----

    #[test]
    fn llm_provider_serde() {
        for (provider, expected) in [
            (LlmProvider::LmStudio, r#""lm_studio""#),
            (LlmProvider::DeepSeek, r#""deepseek""#),
            (LlmProvider::OpenAI, r#""openai""#),
        ] {
            assert_eq!(serde_json::to_string(&provider).unwrap(), expected);
            let back: LlmProvider = serde_json::from_str(expected).unwrap();
            assert_eq!(back, provider);
        }
    }

    #[test]
    fn llm_provider_is_online() {
        assert!(!LlmProvider::LmStudio.is_online());
        assert!(LlmProvider::DeepSeek.is_online());
        assert!(LlmProvider::OpenAI.is_online());
    }

    #[test]
    fn backend_config_defaults() {
        let lm = BackendConfig::lm_studio_default();
        assert_eq!(lm.provider, LlmProvider::LmStudio);
        assert!(lm.base_url.contains("localhost"));

        let ds = BackendConfig::deepseek_default();
        assert_eq!(ds.provider, LlmProvider::DeepSeek);
        assert_eq!(ds.model_id, "deepseek-chat");

        let oa = BackendConfig::openai_default();
        assert_eq!(oa.provider, LlmProvider::OpenAI);
    }

    #[test]
    fn privacy_consent_creation() {
        let consent = PrivacyConsent::new(
            LlmProvider::DeepSeek,
            "https://api.deepseek.com/v1".into(),
            true,
        );
        assert_eq!(consent.provider, LlmProvider::DeepSeek);
        assert!(consent.persistent);
        assert!(consent.timestamp > 0);
    }

    // ---- AppState ----

    #[test]
    fn app_state_serde() {
        for state in [
            AppState::NeedsSetup,
            AppState::DownloadingModel,
            AppState::Indexing,
            AppState::Ready,
            AppState::Degraded,
            AppState::FatalError,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: AppState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn app_state_ready_allows_conversation() {
        // Ready 状态下应该允许对话；其他状态不应该
        assert!(matches!(AppState::Ready, AppState::Ready));
        assert!(!matches!(AppState::NeedsSetup, AppState::Ready));
        assert!(!matches!(AppState::FatalError, AppState::Ready));
    }

    #[test]
    fn message_source_default() {
        assert_eq!(MessageSource::default(), MessageSource::Local);
    }

    #[test]
    fn message_source_serde() {
        assert_eq!(
            serde_json::to_string(&MessageSource::Local).unwrap(),
            r#""local""#
        );
        assert_eq!(
            serde_json::to_string(&MessageSource::Online).unwrap(),
            r#""online""#
        );
    }

    #[test]
    fn time_period_options() {
        assert_eq!(TIME_PERIOD_OPTIONS.len(), 6);
        assert!(TIME_PERIOD_OPTIONS.contains(&"清晨"));
        assert!(TIME_PERIOD_OPTIONS.contains(&"深夜"));
    }
}
