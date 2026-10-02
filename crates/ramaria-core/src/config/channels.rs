//! crates/ramaria-core/src/config/channels.rs - Ramaria 通道与桥接配置模块
//!
//! 设计特点:
//! - 定义 utt 话语块（原文注入通道）配置
//! - 定义桥接配置（外部入口集成）
//! - 定义 MCP 服务端通道配置
//! - 各配置组提供稳定默认值
//! - 支持 serde，供配置文件与 DB settings 共享

use serde::{Deserialize, Serialize};

use crate::types::PersonaKind;

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
/// 三路检索独立参数说明:
/// - 本组为 **utt 原文路** 的专属切分与检索参数（时间间隙/单块条数/检索 top_k/
///   注入预算）。各路记忆注入的检索参数按路归属独立组，避免跨路共享一组数值
///   （例如以原文块的切分参数同时驱动摘要/知识路的召回）。
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
// MCP 接入（外部 MCP 客户端挂载的记忆服务）
// =========================================================

/// MCP 接入配置（`[mcp]`）。
///
/// 职责:
/// - 承载 MCP 服务端的唯一开关面：总开关、写侧治理（回流写入 / 封存触发）、
///   人格可见白名单、原文块开关与召回默认预算。
/// - 桌面「MCP 接入」面板读写本组；`ramaria-mcp` 启动时按本组装配召回策略与工具门禁。
///
/// 字段约定:
/// - `enabled`: 总开关，默认 false（用户在面板主动开启）。
/// - `allow_ingest`: 写工具门禁（`chat_ingest` / `chat_send`），默认 true。
/// - `allow_seal`: 是否允许 MCP 侧触发封存与摘要生成，默认 true；
///   关闭时写工具只写不封存（`finalize=true` 与空闲封存都会消耗 LLM 并改变记忆状态）。
/// - `allowed_personas`: 可见人格白名单，`["*"]` = 全部可见。
/// - `allow_raw_text`: 是否允许返回 utt 原文块，默认 false（原文是最高敏感层）。
/// - `max_items` / `max_chars`: 召回默认预算（缺省与上限由服务层归一化）。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[mcp]` 表只写部分键时缺失字段回退默认值。
/// - `enabled=false` 时 MCP 服务端不提供任何工具能力，对既有对话管线零影响。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// MCP 接入总开关（默认 false）。
    pub enabled: bool,
    /// 是否允许外部对话回流写入（`chat_ingest` / `chat_send` 门禁，默认 true）。
    pub allow_ingest: bool,
    /// 是否允许 MCP 侧触发封存与摘要生成（默认 true）。
    ///
    /// 说明:
    /// - 封存会调用 LLM 生成 L1 并改变记忆状态，故与写入开关分离；
    /// - 关闭时写工具只写不封存，回执中说明本次未封存。
    pub allow_seal: bool,
    /// 可见人格白名单（`["*"]` = 全部可见；空列表按 `["*"]` 处理）。
    pub allowed_personas: Vec<String>,
    /// 是否允许返回 utt 原文块（默认 false —— 内容可能随对话发送给客户端所用模型）。
    pub allow_raw_text: bool,
    /// 召回条目默认上限（默认 5；上限由服务层钳制）。
    pub max_items: u32,
    /// 召回上下文文本默认预算（字符，默认 1200）。
    pub max_chars: u32,
}

impl Default for McpConfig {
    /// 创建默认 MCP 接入配置。
    ///
    /// 返回:
    /// - 未开启（`enabled=false`，不启动即零影响）。
    /// - 写侧默认放开（`allow_ingest` / `allow_seal` 均为 true，回流闭环开箱可用）。
    /// - 读侧默认保守（全部人格可见、原文块关闭）。
    fn default() -> Self {
        Self {
            enabled: false,
            allow_ingest: true,
            allow_seal: true,
            allowed_personas: vec!["*".to_string()],
            allow_raw_text: false,
            max_items: 5,
            max_chars: 1200,
        }
    }
}
