//! crates/ramaria-core/src/types.rs - Ramaria 核心业务数据类型模块
//!
//! 设计特点:
//! - 覆盖核心领域对象: Session、Message、MemoryL1、MemoryEvent、Persona、PersonalityTrait 等
//! - 完整 Persona 体系: 9 个枚举 + 9 个结构体，覆盖人格注册、事件提取、性格推断全链路
//! - ID 双轨制: TEXT 主键表使用 UUID v4（sessions/messages/memory_l1），INTEGER AUTOINCREMENT 表使用 i64
//! - 统一时间规范: 所有时间使用 Unix 毫秒时间戳，存储层以 INTEGER 形式持久化
//! - 所有公共业务类型支持 serde，方便 CLI、Tauri IPC、存储层和测试夹具共享
//! - 提供轻量构造函数和状态辅助方法，避免上层重复初始化或破坏领域约束

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 创建一个新的 UUID v4。
///
/// 用法:
/// - sessions / messages / memory_l1 等 TEXT 主键表创建 ID 时优先使用。
///
/// 返回:
/// - 新的 UUID v4。
#[inline]
pub fn new_id() -> Uuid {
    Uuid::new_v4()
}

/// 将 UUID 格式化为 SQLite TEXT 兼容的字符串。
///
/// 返回:
/// - UUID 的小写 hex 字符串，如 `"550e8400-e29b-41d4-a716-446655440000"`。
#[inline]
pub fn uuid_to_db(u: Uuid) -> String {
    u.to_string()
}

/// 从 SQLite TEXT 解析 UUID。
///
/// 返回:
/// - 成功时返回 Ok(UUID)。
/// - 解析失败时返回 `Err(RamariaError::Validation)`，携带 trace_id 和原始值。
///
/// 说明:
/// - 存储层可能读到历史遗留的非法数据，此处返回明确的错误而非静默降级。
/// - 调用方应记录 WARNING 日志并传播错误，以便上层统一处理数据一致性问题。
#[inline]
pub fn uuid_from_db(s: &str) -> crate::error::RamariaResult<Uuid> {
    Uuid::parse_str(s).map_err(|_| {
        crate::error::RamariaError::validation(format!(
            "UUID 解析失败: 数据库中存储了非法 UUID 值 '{s}'，可能由历史数据损坏或 bug 引起"
        ))
    })
}

/// 检查 UUID 是否为 nil（表示解析失败或未初始化）。
///
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
/// - 若系统时钟在 UNIX_EPOCH 之前（极度异常），返回 0。
///   上层应在发现时间戳为 0 时记录 ERROR 日志。
#[inline]
pub fn now_ms() -> i64 {
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => {
            // 系统时钟异常——返回 0 作为哨兵值，上层需检测并告警
            0
        }
    }
}

// =========================================================
// 消息来源
// =========================================================

/// 消息来源：本地模型或线上 API。
///
/// 职责:
/// - 标记一条消息来自本地 provider 还是线上 provider。
/// - 供隐私提示、日志脱敏和 UI 状态展示使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
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
#[non_exhaustive]
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
// 会话与原始消息（TEXT 主键表 — 使用 UUID）
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
/// - `persona_uid = Some(...)`: 创建此 session 时使用的对话人格。
/// - `persona_uid = None`: 存量 session 或未指定人格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    /// Session 开始时间（Unix 毫秒）
    pub started_at: i64,
    /// Session 结束时间，None 表示未关闭
    pub ended_at: Option<i64>,
    /// 创建此 session 时绑定的对话人格 UID（可空兼容存量数据）
    pub persona_uid: Option<String>,
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
    /// - 带新 UUID、当前开始时间、未关闭状态、无 persona_uid 的 Session。
    pub fn new() -> Self {
        Self {
            id: new_id(),
            started_at: now_ms(),
            ended_at: None,
            persona_uid: None,
        }
    }

    /// 创建一个绑定人格的活跃 Session。
    ///
    /// 参数:
    /// - `persona_uid`: 对话人格标识（None 表示 rama 自身）。
    ///
    /// 返回:
    /// - 带新 UUID、当前开始时间、绑定 persona_uid 的 Session。
    pub fn with_persona(persona_uid: Option<String>) -> Self {
        Self {
            id: new_id(),
            started_at: now_ms(),
            ended_at: None,
            persona_uid,
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

    /// 判断 Session 是否仍在进行中。
    ///
    /// 返回:
    /// - `true`: Session 处于活跃状态（`ended_at` 为 None）。
    /// - `false`: Session 已关闭（`ended_at` 有值）。
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// L0 原始消息。
///
/// 职责:
/// - 保存用户、助手、系统或工具的原始消息。
/// - 作为 L1 摘要、检索索引和对话历史的事实源。
/// - `persona_uid` 标记发言人，用于 Persona-Aware RAG 的原话过滤。
///
/// 去重:
/// - `fingerprint` 为 SHA-256 前 16 位 hex，用于历史导入去重。
/// - 正常对话产生的消息此字段为 None。
///
/// 字段约定:
/// - `persona_uid`: 发言人标识。系统/助手消息填 None，导入消息填对应发言人的 uid。
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
    /// 发言人标识，系统/助手消息为 None
    pub persona_uid: Option<String>,
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
    /// - 带新 UUID、当前创建时间且无 fingerprint 和 persona_uid 的消息。
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
            persona_uid: None,
        }
    }

    /// 设置发言人 persona_uid（链式调用）。
    ///
    /// 说明:
    /// - 用户消息通常不设（发言人是用户自己）。
    /// - 助手消息设为当前对话的人格 uid，用于前端显示"谁在回复"。
    pub fn with_persona_uid(mut self, uid: Option<String>) -> Self {
        self.persona_uid = uid;
        self
    }
}

// =========================================================
// 分层记忆类型（TEXT 主键 — 使用 UUID）
// =========================================================

/// L1 摘要的结构化证据线索（v1.4 起替代旧字符串数组）。
///
/// 职责:
/// - 记录支撑 summary 结论的具体事实引用，供 L2 事件提取与前端证据链展示。
/// - `time` / `who` / `cause` 为可选槽位，为 L2 事件提取提供因果线索（v1.4 B1）。
///
/// 格式:
/// - `text`: 必填，证据文本（校验要求 ≥ 5 字符）。
/// - `time`: 可选，事件发生的时间描述（如"上周三晚上"）。
/// - `who`: 可选，涉及的人物/角色。
/// - `cause`: 可选，可辨时的因果线索（缺失留空，供背景参考）。
///
/// 迁移约定（一次性迁移）:
/// - 存量旧格式（字符串数组）由 migration 一次性迁移，字符串落 `text` 槽位、其余置空。
/// - 运行时不做兼容解析，读写均为新格式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceNote {
    /// 证据文本（必填，≥ 5 字符）
    pub text: String,
    /// 事件发生的时间描述（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// 涉及的人物/角色（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
    /// 因果线索（可选，仅供背景参考，不视为事实断言）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl EvidenceNote {
    /// 创建仅含文本的证据线索（其余槽位置空）。
    ///
    /// 参数:
    /// - `text`: 证据文本。
    ///
    /// 返回:
    /// - `time`/`who`/`cause` 均为 None 的 EvidenceNote。
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            time: None,
            who: None,
            cause: None,
        }
    }

    /// 创建带完整槽位的证据线索。
    ///
    /// 参数:
    /// - `text`: 证据文本（必填）。
    /// - `time`/`who`/`cause`: 可选槽位，None 表示未填写。
    pub fn with_slots(
        text: impl Into<String>,
        time: Option<String>,
        who: Option<String>,
        cause: Option<String>,
    ) -> Self {
        Self {
            text: text.into(),
            time,
            who,
            cause,
        }
    }
}

/// L1 单次会话摘要。
///
/// 职责:
/// - 表示一次 Session 关闭后生成的会话摘要。
/// - 保存关键词、时间段、情绪效价和显著性，供后续检索和事件提取使用。
/// - 通过 `absorbed` 标记是否已被事件提取器消化。
/// - `salience` 升级为全链路连续权重：所有加权统计（均值、方差、n_eff）以 salience 为权重。
///
/// 字段约定:
/// - `persona_uid`: 本条摘要主要描述哪个人。描述用户自己时为 None。
/// - `context_json`: JSON 格式，存 `chat_partners` 列表，事件提取时按此字段分组。
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
    /// 情感显著性 0.0..1.0，全链路连续权重
    pub salience: f64,
    /// 是否已被事件提取器吸收
    pub absorbed: bool,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 最近被检索命中的时间
    pub last_accessed_at: Option<i64>,
    /// 人格关联——本条摘要描述的对象
    pub persona_uid: Option<String>,
    /// 分组上下文——JSON 格式 `{"chat_partners": ["user-0001", "char-0003"]}`
    pub context_json: Option<String>,
    /// 情境强度 1-5：
    /// - 1-2: 弱情境（闲聊、日常寒暄）→ 加权 ×1.5
    /// - 3: 中性情境（默认值）→ 加权 ×1.0
    /// - 4-5: 强情境（冲突、关键决策）→ 加权 ×0.5
    /// - None: 存量数据，等同于 3
    pub situation_strength: Option<i32>,
    /// 证据线索列表（结构化对象，v1.4 起替代旧字符串数组）。
    ///
    /// 存储支持摘要结论的具体事实引用，每条 evidence 记录
    /// "谁在什么条件下表达了什么态度/经历了什么事件"。
    ///
    /// 格式:
    /// - `Some(vec![EvidenceNote { text, time?, who?, cause? }, ...])` — 正常产出
    /// - `Some(vec![])` — LLM 未产出有效 evidence（降级路径，不阻塞 L1 生成）
    /// - `None` — 存量数据或尚未生成
    ///
    /// 用途:
    /// - L2 事件提取时作为证据互证判断的输入
    /// - 前端 L3 性格画像的证据链溯源展示
    pub evidence_notes: Option<Vec<EvidenceNote>>,
    /// 与上一对话块的话题延续关系。
    ///
    /// 枚举（相对上一块）:
    /// - `"延续"` — 当前对话承接上一块话题继续讨论
    /// - `"转折"` — 话题发生转换或明显偏移
    /// - `"无关"` — 与上一块完全无关（独立话题，生成时忽略上文）
    ///
    /// 语义:
    /// - `None` — 无上一块（首块/独立摘要路径，等同 v1.4 行为），
    ///   或 LLM 输出非法值被校验丢弃。
    /// - 供 L2 因果与脉络注入使用，不参与摘要文本本身。
    pub continuation: Option<String>,
}

impl MemoryL1 {
    /// 创建一条新的 L1 记忆。
    ///
    /// 参数:
    /// - `session_id`: 来源 Session。
    /// - `summary`: 摘要文本。
    /// - `time_period`: 可选时间段，如"上午""夜间"。
    ///
    /// 返回:
    /// - 默认 valence=0.0、salience=0.5、未吸收、situation_strength=None（等效 3）的 L1 记忆。
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
            persona_uid: None,
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        }
    }

    /// 标记此 L1 已被事件提取器吸收。
    ///
    /// 用法:
    /// - 事件提取成功后调用。
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

// =========================================================
// utt 话语块（v1.4 新增 — 原文注入通道的最小单元）
// =========================================================

/// utt 话语块——原文按连续性切分的最小注入单元（v1.4 A1）。
///
/// 职责:
/// - 承载一次会话中按时间间隙/条数上限切分出的连续原文片段。
/// - 作为对话时【原文片段】注入、跨会话桥接与未来风格统计的原料底座。
///
/// 字段约定:
/// - `persona_uid`: 块归属人格，原文按 persona 严格隔离（隐私约束）。
/// - `block_text`: 块内原文全文，按原文格式拼接（含发言人标记），不写日志。
/// - `embedding`: 块文本向量（f32 小端 BLOB），`None` 表示未生成或 embedding 不可用。
///
/// 安全约束:
/// - 原文是最高敏感层：注入受 `persona_kind_whitelist` 约束，内容不记录日志。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UttBlock {
    /// 内部索引（INTEGER AUTOINCREMENT，0 表示尚未入库）
    pub id: i64,
    /// 块归属人格 UID
    pub persona_uid: String,
    /// 来源会话 ID
    pub session_id: Uuid,
    /// 块内首条消息 ID
    pub start_msg_id: Uuid,
    /// 块内末条消息 ID
    pub end_msg_id: Uuid,
    /// 块内原文全文
    pub block_text: String,
    /// 块内消息条数
    pub msg_count: u32,
    /// 首末消息时间跨度（毫秒）
    pub time_span_ms: i64,
    /// 块文本向量（f32 小端 BLOB），None 表示未生成
    pub embedding: Option<Vec<u8>>,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
}

impl UttBlock {
    /// 创建新话语块（id=0，由存储层回填）。
    ///
    /// 参数:
    /// - `persona_uid`: 块归属人格。
    /// - `session_id`: 来源会话。
    /// - `start_msg_id` / `end_msg_id`: 消息区间。
    /// - `block_text`: 原文全文。
    /// - `msg_count`: 消息条数。
    /// - `time_span_ms`: 时间跨度。
    ///
    /// 返回:
    /// - `embedding=None`、`created_at=当前时间` 的 UttBlock。
    pub fn new(
        persona_uid: String,
        session_id: Uuid,
        start_msg_id: Uuid,
        end_msg_id: Uuid,
        block_text: String,
        msg_count: u32,
        time_span_ms: i64,
    ) -> Self {
        Self {
            id: 0,
            persona_uid,
            session_id,
            start_msg_id,
            end_msg_id,
            block_text,
            msg_count,
            time_span_ms,
            embedding: None,
            created_at: now_ms(),
        }
    }
}

// =========================================================
// Persona 枚举体系（9 个枚举）
// =========================================================

/// 人格类型。
///
/// 职责:
/// - 区分人格画像的主体类型，决定检索权限和行为模式。
/// - `rama` 类型拥有全量检索权（了解对话双方），其他类型仅检索自己的记忆。
///
/// 格式:
/// - `uid` 值按 `{kind}-{seq}` 格式自动生成，如 `user-0001`、`char-0003`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PersonaKind {
    /// 用户本人
    User,
    /// 助手（Ramaria 自身）
    Rama,
    /// 熟人复刻
    Char,
    /// 虚拟角色
    Anim,
    /// 原创角色
    Oc,
    /// 历史人物
    Hist,
}

impl PersonaKind {
    /// 返回人格类型的稳定字符串标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Rama => "rama",
            Self::Char => "char",
            Self::Anim => "anim",
            Self::Oc => "oc",
            Self::Hist => "hist",
        }
    }

    /// 从 persona_uid 推断人格类型。
    ///
    /// 规则: uid 前缀匹配，未知前缀保守回退为 `Char`。
    /// - `"rama-"` → `Rama`
    /// - `"user-"` → `User`
    /// - `"char-"` → `Char`
    /// - `"anim-"` → `Anim`
    /// - `"oc-"` → `Oc`
    /// - `"hist-"` → `Hist`
    /// - 其他前缀 / 无前缀 → `Char`
    pub fn from_uid(uid: &str) -> Self {
        if uid.starts_with("rama-") {
            Self::Rama
        } else if uid.starts_with("user-") {
            Self::User
        } else if uid.starts_with("char-") {
            Self::Char
        } else if uid.starts_with("anim-") {
            Self::Anim
        } else if uid.starts_with("oc-") {
            Self::Oc
        } else if uid.starts_with("hist-") {
            Self::Hist
        } else {
            Self::Char
        }
    }

    /// 获取当前 persona 类型在 Persona-Aware RAG 中使用的 share 阈值。
    ///
    /// 规则:
    /// - `Rama`: 助手自身，不设阈值（由调用方决定，默认 0.0 全量）
    /// - `User`: 用户本人，较宽松过滤
    /// - `Char`/`Anim`/`Oc`/`Hist`: 角色类型，需严格过滤
    pub fn min_share(&self, rama_threshold: f64, user_threshold: f64, char_threshold: f64) -> f64 {
        match self {
            Self::Rama => rama_threshold,
            Self::User => user_threshold,
            Self::Char | Self::Anim | Self::Oc | Self::Hist => char_threshold,
        }
    }
}

impl std::fmt::Display for PersonaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 事实/画像字段分类。
///
/// 职责:
/// - 约束 `persona_facts` 表可写入的字段集合。
/// - 避免画像无限扩张为任意 key-value。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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
    /// 说话风格
    SpeakingStyle,
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
            Self::SpeakingStyle => "说话风格",
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
            Self::SpeakingStyle => "speaking_style",
        }
    }
}

impl std::fmt::Display for ProfileField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 性格层次——三层性格模型。
///
/// 职责:
/// - `Base`: 底色——跨情境稳定的深层性格基调（2-3 条）。
/// - `Primary`: 主色调——日常最突出的性格，形成第一印象（1-2 条）。
/// - `Accent`: 点缀——仅在特定条件下浮现的隐藏性格（2-4 条）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum TraitLayer {
    Base,
    Primary,
    Accent,
}

impl TraitLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Primary => "primary",
            Self::Accent => "accent",
        }
    }
}

impl std::fmt::Display for TraitLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 性格来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum TraitSource {
    L1,
    Event,
    Manual,
    Inferred,
}

impl TraitSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::Event => "event",
            Self::Manual => "manual",
            Self::Inferred => "inferred",
        }
    }
}

/// 事实来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FactSource {
    L1,
    Event,
    Manual,
}

impl FactSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::Event => "event",
            Self::Manual => "manual",
        }
    }
}

/// 事实生命周期状态（persona_facts.status 版本链）。
///
/// 职责:
/// - 约束 `persona_facts.status` 可写入的状态集合。
/// - 检索与注入只取 `Active`；`Superseded` 沿 `version_of` 链可追溯；`Candidate` 待互证提升。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FactStatus {
    /// 当前生效，参与检索与注入
    Active,
    /// 已被覆盖（沿 version_of 链指向被替换事实）
    Superseded,
    /// 待互证提升（主观隐含事实初始落于此轨道）
    Candidate,
}

impl FactStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Candidate => "candidate",
        }
    }
}

impl std::fmt::Display for FactStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 事实分层策略（persona_facts.tier）。
///
/// 职责:
/// - 约束 `persona_facts.tier` 可写入的分层，决定更新与衰减策略。
///
/// 策略约定:
/// - `Stable`: 基础信息/兴趣爱好/社交长期——不轻易覆盖（需互证或 manual），不衰减。
/// - `Volatile`: 近期状态/近期背景——新覆盖旧，保留版本链，随事件时间衰减。
/// - `Historical`: 历史事件——只追加不覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FactTier {
    Stable,
    Volatile,
    Historical,
}

impl FactTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Volatile => "volatile",
            Self::Historical => "historical",
        }
    }
}

impl std::fmt::Display for FactTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 事件关系类型——事件间 6 种语义关联。
///
/// 职责:
/// - `Contradicts` 是点缀层（Accent）性格的重要信号源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub enum EventRelationKind {
    /// 前因 → 后果
    CausedBy,
    /// 部分 → 整体
    PartOf,
    /// 一般关联
    RelatedTo,
    /// 后续发展
    ContinuedBy,
    /// 矛盾（点缀层性格信号源）
    Contradicts,
    /// 纯时序，无因果
    Timeline,
}

impl EventRelationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CausedBy => "CausedBy",
            Self::PartOf => "PartOf",
            Self::RelatedTo => "RelatedTo",
            Self::ContinuedBy => "ContinuedBy",
            Self::Contradicts => "Contradicts",
            Self::Timeline => "Timeline",
        }
    }
}

/// 陈述方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Presentation {
    /// 客观
    Objective,
    /// 主观
    Subjective,
    /// 混合
    Mixed,
}

impl Presentation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Objective => "objective",
            Self::Subjective => "subjective",
            Self::Mixed => "mixed",
        }
    }
}

/// 证据方向——事件对性格标签的支撑/矛盾关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EvidenceDirection {
    /// 支撑
    Support,
    /// 矛盾
    Contradict,
    /// 中性
    Neutral,
}

impl EvidenceDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Support => "support",
            Self::Contradict => "contradict",
            Self::Neutral => "neutral",
        }
    }
}

/// 性格标签生命周期。
///
/// 状态:
/// - `Active`: 当前生效。
/// - `Deprecated`: 触发条件长期未满足（accent 30 天无事件自动标记）。
/// - `Historical`: 旧版本（全量校准时覆盖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum TraitStatus {
    Active,
    Deprecated,
    Historical,
}

impl TraitStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Historical => "historical",
        }
    }
}

// =========================================================
// Persona 结构体体系（9 个结构体）
// ID 类型约定:
// - INTEGER AUTOINCREMENT 表 → i64（内部索引）
// - TEXT/UUID 表 → Uuid（业务标识）
// - FK 列类型与目标表 PK 类型一致
// =========================================================

/// 统一人格注册表条目。
///
/// 职责:
/// - `personas` 表对应的业务类型，是所有记忆主体的统一注册中心。
/// - `uid` 为全局业务标识（格式 `{kind}-{seq}`），`id` 仅内部索引(i64)。
/// - 仅自动创建 `user-0001` 和 `rama-0001`。
///
/// 字段约定:
/// - `source` + `ref_id`: 为 + 导入器预留的跨渠道身份去重键。
/// - `config`: JSON 格式的个性配置（温度、模型偏好等）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    /// 内部索引（INTEGER AUTOINCREMENT），不参与业务逻辑
    pub id: i64,
    /// 业务标识，如 `user-0001`、`char-0003`
    pub uid: String,
    pub name: String,
    pub kind: PersonaKind,
    pub seq: i64,
    /// 来源渠道：local / qq / wechat / telegram / manual / network
    pub source: String,
    /// 来源方原始 ID
    pub ref_id: Option<String>,
    pub avatar: Option<String>,
    /// JSON 个性配置
    pub config: Option<String>,
    /// 人格简要描述（面向用户的短文本， 新增）
    pub description: Option<String>,
    /// 1=启用，0=停用
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Persona {
    /// 创建一个新的人格注册条目。
    /// 创建时 id 为 0，由存储层在 INSERT 后回填。
    pub fn new(uid: String, name: String, kind: PersonaKind, seq: i64, source: String) -> Self {
        let now = now_ms();
        Self {
            id: 0,
            uid,
            name,
            kind,
            seq,
            source,
            ref_id: None,
            avatar: None,
            config: None,
            description: None,
            active: true,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 原子化人物事实（L2 层，替代旧的 `user_profile` 表）。
///
/// 职责:
/// - 每条事实独立可追溯，存"发生了什么"。
/// - 性格存"他是怎样的人"，归 L3 的 `PersonalityTrait`。
///
/// 字段约定:
/// - `ref_event_id` 为 i64 (FK→memory_events.id)。
/// - `ref_l1_id` 为 UUID (FK→memory_l1.id，TEXT 表)。
/// - 拆为两个独立可空列，避免一列指两张表的关系模型二义性。
///
/// 版本化字段:
/// - `status` = active / superseded / candidate（检索只取 active）。
/// - `tier` = stable / volatile / historical（分层更新与衰减策略）。
/// - `version_of` = 覆盖时新事实指向被替换事实 id；旧事实置 superseded。
/// - `confidence` = 0.0..1.0；主观隐含事实初始 0.5 入 candidate 轨道。
/// - `keyword_hint` = 事实关键词（判重交集 & 判定器话题检索使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaFact {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    pub persona_uid: String,
    pub field: ProfileField,
    pub content: String,
    pub source: FactSource,
    /// 生命周期状态
    pub status: FactStatus,
    /// 分层策略
    pub tier: FactTier,
    /// 覆盖链——指向被替换事实 id
    pub version_of: Option<i64>,
    /// 置信度 0.0..1.0
    pub confidence: f64,
    /// 事实关键词（逗号分隔）
    pub keyword_hint: Option<String>,
    /// FK→memory_events.id (INTEGER)
    pub ref_event_id: Option<i64>,
    /// FK→memory_l1.id (TEXT/UUID)
    pub ref_l1_id: Option<Uuid>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PersonaFact {
    /// 创建一条新事实。id 为 0，由存储层回填。
    pub fn new(
        persona_uid: String,
        field: ProfileField,
        content: String,
        source: FactSource,
    ) -> Self {
        let now = now_ms();
        Self {
            id: 0,
            persona_uid,
            field,
            content,
            source,
            status: FactStatus::Active,
            tier: FactTier::Stable,
            version_of: None,
            confidence: 1.0,
            keyword_hint: None,
            ref_event_id: None,
            ref_l1_id: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// L2 事件主表（替代旧的 `memory_l2` 表）。
///
/// 职责:
/// - 按人物维度管理离散生活事件，是从对话到性格推断的数据桥梁。
/// - 每条事件携带 8 个推断信号属性（valence/confidence/presentation/share/attitude/paraphrase/salience/keywords）。
///
/// 字段约定:
/// - `paraphrase`: 态度的去情境化重述，事件写入时 LLM 生成一次并持久化缓存。
/// - `confidence < 0.6` 的事件不参与性格推断（唯一硬截断）。
/// - `share` 不设推断阈值，仅在 RAG 暴露环节过滤（share >= 0.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    pub persona_uid: String,
    /// ≤20 字标题
    pub title: String,
    /// 2-3 句描述
    pub summary: String,
    /// 逗号分隔关键词（含分类标签和地点）
    pub keywords: Option<String>,
    /// JSON 数组，事件涉及的其他 persona_uid
    pub participants: Option<String>,
    /// 事件开始（Unix 毫秒）
    pub start: i64,
    /// 事件结束（Unix 毫秒）
    pub end: i64,
    /// 事实确凿度 0.0..1.0，<0.6 不参与性格推断
    pub confidence: f64,
    /// 全链路连续权重 0.0..1.0
    pub salience: f64,
    /// 情绪效价 -1.0..1.0
    pub valence: f64,
    /// 陈述方式
    pub presentation: Presentation,
    /// 分享意愿 0.0..1.0
    pub share: f64,
    /// 态度的自然语言原文
    pub attitude: Option<String>,
    /// 态度的去情境化重述（剥离具体实体）
    pub paraphrase: Option<String>,
    /// 合并了多少条 L1
    pub absorbed: i64,
    /// 情境强度 1-5（从源 L1 传播），None 等效 3
    pub situation_strength: Option<i32>,
    /// 底层动机标注（Schema 预埋），None 表示未标注
    pub motives: Option<String>,
    pub created_at: i64,
    pub last_accessed_at: Option<i64>,
    pub indexed_at: Option<i64>,
    pub index_version: Option<i64>,
}

impl MemoryEvent {
    /// 创建新事件。id 为 0，由存储层回填。
    pub fn new(persona_uid: String, title: String, summary: String, start: i64, end: i64) -> Self {
        let now = now_ms();
        Self {
            id: 0,
            persona_uid,
            title,
            summary,
            keywords: None,
            participants: None,
            start,
            end,
            confidence: 0.5,
            salience: 0.5,
            valence: 0.0,
            presentation: Presentation::Mixed,
            share: 0.5,
            attitude: None,
            paraphrase: None,
            absorbed: 0,
            situation_strength: None,
            motives: None,
            created_at: now,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        }
    }
}

/// 事件关系——事件间语义关联。
///
/// 字段约定:
/// - `from_id` / `to_id`: i64 (FK→memory_events.id)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRelation {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    /// FK→memory_events.id
    pub from_id: i64,
    /// FK→memory_events.id
    pub to_id: i64,
    pub kind: EventRelationKind,
    /// 关系强度，默认 0.5
    pub weight: f64,
    pub created_at: i64,
}

impl EventRelation {
    /// 创建新的事件关系。id 为 0，由存储层回填。
    pub fn new(from_id: i64, to_id: i64, kind: EventRelationKind) -> Self {
        Self {
            id: 0,
            from_id,
            to_id,
            kind,
            weight: 0.5,
            created_at: now_ms(),
        }
    }
}

/// 事件溯源（替代旧的 `l2_sources` 表）。
///
/// 字段约定:
/// - `event_id`: i64 (FK→memory_events.id)。
/// - `l1_id`: Uuid (FK→memory_l1.id)。
/// - `weight`: L1 对事件的贡献权重，默认 1.0。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSource {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    /// FK→memory_events.id
    pub event_id: i64,
    /// FK→memory_l1.id (TEXT/UUID)
    pub l1_id: Uuid,
    pub weight: f64,
}

impl EventSource {
    /// 创建新的事件溯源。id 为 0，由存储层回填。
    pub fn new(event_id: i64, l1_id: Uuid) -> Self {
        Self {
            id: 0,
            event_id,
            l1_id,
            weight: 1.0,
        }
    }
}

/// 性格证据链——性格标签与事件之间的支撑/矛盾关系。
///
/// 职责:
/// - 是置信度计算的持久化基础。
/// - `direction` + `score` 记录事件态度与性格的语义匹配度。
///
/// 字段约定:
/// - `trait_id`: i64 (FK→personality_traits.id)。
/// - `event_id`: i64 (FK→memory_events.id)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraitEvidence {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    /// FK→personality_traits.id
    pub trait_id: i64,
    /// FK→memory_events.id
    pub event_id: i64,
    pub direction: EvidenceDirection,
    /// -1.0..1.0，事件态度与性格的语义匹配度
    pub score: f64,
    /// 时间衰减权重
    pub decay: f64,
    pub created_at: i64,
}

impl TraitEvidence {
    /// 创建新的证据记录。id 为 0，由存储层回填。
    pub fn new(trait_id: i64, event_id: i64, direction: EvidenceDirection, score: f64) -> Self {
        Self {
            id: 0,
            trait_id,
            event_id,
            direction,
            score,
            decay: 1.0,
            created_at: now_ms(),
        }
    }
}

/// 三层结构化性格画像（L3 层核心产出）。
///
/// 职责:
/// - 从 L2 事件集中通过统计计算和 LLM 语义推断提炼的性格标签。
/// - 是 System Prompt 中角色核心定义的直接来源。
///
/// 字段约定:
/// - `confidence`/`evidence`/`consistency`: 冗余存储，从 `trait_evidence` 聚合计算，避免 System Prompt 构建时 JOIN 证据表。
/// - `ref_event_id`: i64 (FK→memory_events.id)。
/// - `ref_l1_id`: Uuid (FK→memory_l1.id)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalityTrait {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    pub persona_uid: String,
    pub layer: TraitLayer,
    /// 标签词，如"温和""幽默"
    pub trait_label: String,
    /// 在此人身上的具体含义
    pub meaning: String,
    /// 反向界定——它不是什么
    pub not_meaning: Option<String>,
    /// 浮现条件
    pub trigger: Option<String>,
    /// 抑制条件
    pub suppress: Option<String>,
    /// 与其他性格的关系
    pub related: Option<String>,
    /// 层内排序
    pub seq: i32,
    pub source: TraitSource,
    /// FK→memory_events.id
    pub ref_event_id: Option<i64>,
    /// FK→memory_l1.id
    pub ref_l1_id: Option<Uuid>,
    /// 聚合置信度 0..1
    pub confidence: f64,
    /// 有效证据量
    pub evidence: f64,
    /// 一致度
    pub consistency: f64,
    pub status: TraitStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PersonalityTrait {
    /// 创建新的性格标签。id 为 0，由存储层回填。
    pub fn new(
        persona_uid: String,
        layer: TraitLayer,
        trait_label: String,
        meaning: String,
        source: TraitSource,
        seq: i32,
    ) -> Self {
        let now = now_ms();
        Self {
            id: 0,
            persona_uid,
            layer,
            trait_label,
            meaning,
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq,
            source,
            ref_event_id: None,
            ref_l1_id: None,
            confidence: 0.0,
            evidence: 0.0,
            consistency: 0.0,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 对话 Few-shot 示例——注入 System Prompt 作为说话风格参考。
///
/// 职责:
/// - 从真实对话中选取 3-5 对示例，帮助 LLM 模仿该人格的说话风格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaExample {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    pub persona_uid: String,
    /// 对方说了什么
    pub partner: String,
    /// 此人回复了什么
    pub reply: String,
    /// FK→sessions.id (TEXT/UUID)
    pub session_id: Option<Uuid>,
    /// 前文（最多前 3 条）
    pub context: Option<String>,
    pub valence: f64,
    /// 话题标签，逗号分隔
    pub tags: Option<String>,
    /// 1=当前生效，0=候选库备选
    pub selected: bool,
    /// 回复字符数
    pub length: i32,
    pub created_at: i64,
}

impl PersonaExample {
    /// 创建新的对话示例。id 为 0，由存储层回填。
    pub fn new(persona_uid: String, partner: String, reply: String) -> Self {
        let len = reply.chars().count() as i32;
        Self {
            id: 0,
            persona_uid,
            partner,
            reply,
            session_id: None,
            context: None,
            valence: 0.0,
            tags: None,
            selected: false,
            length: len,
            created_at: now_ms(),
        }
    }
}

/// 态度聚类快照——支撑跨版本簇匹配。
///
/// 职责:
/// - 每次全量聚类后保存各分类下的簇结构和语义标签。
/// - 跨版本匹配时比对语义标签的 embedding 相似度，而非簇编号。
///
/// - `semantic_label`: 从核心样本 paraphrase 中提取的语义标签文本。
/// - `semantic_label_embedding`: 语义标签的 embedding 向量 BLOB（f32 小端序列化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSnapshot {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    pub persona_uid: String,
    /// 事件分类标签（工作/社交/家庭）
    pub category: String,
    /// 簇的语义标签（旧字段，保留兼容）
    pub cluster_label: String,
    /// JSON 数组，核心样本的去情境化态度文本
    pub samples: Option<String>,
    /// 该簇的事件数
    pub count: i32,
    /// 1=最新快照，0=历史版本
    pub is_current: bool,
    pub created_at: i64,
    /// 从核心样本提取的语义标签文本
    pub semantic_label: Option<String>,
    /// 语义标签的 embedding 向量（f32 小端 BLOB）
    pub semantic_label_embedding: Option<Vec<u8>>,
}

impl ClusterSnapshot {
    /// 创建新的聚类快照。id 为 0，由存储层回填。
    pub fn new(persona_uid: String, category: String, cluster_label: String) -> Self {
        Self {
            id: 0,
            persona_uid,
            category,
            cluster_label,
            samples: None,
            count: 0,
            is_current: true,
            created_at: now_ms(),
            semantic_label: None,
            semantic_label_embedding: None,
        }
    }

    /// 将 f32 向量序列化为 BLOB（小端字节序）。
    ///
    /// 格式: 每个 f32 占 4 字节，按小端排列。
    /// 用于存储到 `semantic_label_embedding` BLOB 列。
    pub fn serialize_embedding(vec: &[f32]) -> Vec<u8> {
        let mut blob = Vec::with_capacity(vec.len() * 4);
        for &val in vec {
            blob.extend_from_slice(&val.to_le_bytes());
        }
        blob
    }

    /// 从 BLOB 反序列化为 f32 向量。
    ///
    /// 参数:
    /// - `blob`: 小端字节序的 f32 BLOB 数据。
    ///
    /// 返回:
    /// - `Some(Vec<f32>)` 如果 BLOB 长度是 4 的倍数；`None` 如果数据损坏或为空。
    pub fn deserialize_embedding(blob: &[u8]) -> Option<Vec<f32>> {
        if blob.is_empty() || !blob.len().is_multiple_of(4) {
            return None;
        }
        let count = blob.len() / 4;
        let mut vec = Vec::with_capacity(count);
        for chunk in blob.chunks_exact(4) {
            let arr: [u8; 4] = chunk.try_into().unwrap();
            vec.push(f32::from_le_bytes(arr));
        }
        Some(vec)
    }
}

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
#[non_exhaustive]
pub enum AppState {
    /// 首次配置未完成，需进入配置向导
    NeedsSetup,
    /// embedding 模型下载中
    DownloadingModel,
    /// 索引初始化或重建中
    Indexing,
    /// 可正常对话
    Ready,
    /// 可恢复故障（LLM 暂不可用 或 嵌入模型未配置/不可用）
    ///
    /// 语义扩大：不再仅限 LLM 故障。当嵌入模型缺失时也进入此状态，
    /// 对话功能可用（BM25 + 图谱通道仍工作），但向量检索通道不可用。
    /// 前端应在对话页顶部显示具体原因的警告条。
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

    // ---- EvidenceNote（v1.4 结构化证据线索）----

    #[test]
    fn evidence_note_new_creates_text_only() {
        let note = EvidenceNote::new("用户表示最近一个月每天加班到10点以后");
        assert_eq!(note.text, "用户表示最近一个月每天加班到10点以后");
        assert!(note.time.is_none());
        assert!(note.who.is_none());
        assert!(note.cause.is_none());
    }

    #[test]
    fn evidence_note_with_slots() {
        let note = EvidenceNote::with_slots(
            "用户提到项目延期",
            Some("上周三晚上".to_string()),
            Some("用户".to_string()),
            Some("因为需求变更".to_string()),
        );
        assert_eq!(note.time.as_deref(), Some("上周三晚上"));
        assert_eq!(note.who.as_deref(), Some("用户"));
        assert_eq!(note.cause.as_deref(), Some("因为需求变更"));
    }

    #[test]
    fn evidence_note_serde_roundtrip_full() {
        let note = EvidenceNote::with_slots(
            "用户表示压力很大",
            Some("最近".to_string()),
            Some("用户".to_string()),
            Some("工作量大".to_string()),
        );
        let json = serde_json::to_string(&note).unwrap();
        let back: EvidenceNote = serde_json::from_str(&json).unwrap();
        assert_eq!(note, back);
    }

    #[test]
    fn evidence_note_serde_roundtrip_text_only() {
        let note = EvidenceNote::new("仅文本证据");
        let json = serde_json::to_string(&note).unwrap();
        let back: EvidenceNote = serde_json::from_str(&json).unwrap();
        assert_eq!(note, back);
        // 文本-only 序列化应包含 text 键且无多余槽位
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["text"], "仅文本证据");
        assert!(value.get("time").is_none(), "None 槽位不应序列化");
    }

    #[test]
    fn evidence_note_parses_missing_slots() {
        // 兼容：缺失槽位的 JSON 反序列化为 None（而非报错）
        let json = r#"{"text": "只有文本的证据"}"#;
        let note: EvidenceNote = serde_json::from_str(json).unwrap();
        assert_eq!(note.text, "只有文本的证据");
        assert!(note.time.is_none());
        assert!(note.who.is_none());
        assert!(note.cause.is_none());
    }

    #[test]
    fn evidence_note_parses_null_slots() {
        // LLM 可能输出 "time": null，应解析为 None
        let json = r#"{"text": "证据", "time": null, "who": null, "cause": null}"#;
        let note: EvidenceNote = serde_json::from_str(json).unwrap();
        assert_eq!(note.text, "证据");
        assert!(note.time.is_none());
    }

    #[test]
    fn memory_l1_evidence_notes_upgraded_to_structured() {
        // MemoryL1.evidence_notes 已是 Vec<EvidenceNote>（结构化新格式）
        let mut l1 = MemoryL1::new(Uuid::new_v4(), "摘要".to_string(), None);
        l1.evidence_notes = Some(vec![
            EvidenceNote::with_slots(
                "用户提到项目延期",
                Some("上周".to_string()),
                Some("用户".to_string()),
                Some("需求变更".to_string()),
            ),
            EvidenceNote::new("用户表示压力很大"),
        ]);
        let json = serde_json::to_string(&l1).unwrap();
        let back: MemoryL1 = serde_json::from_str(&json).unwrap();
        let notes = back.evidence_notes.expect("evidence_notes 不应为 None");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].who.as_deref(), Some("用户"));
        assert_eq!(notes[1].text, "用户表示压力很大");
        assert!(notes[1].cause.is_none());
    }

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
    fn uuid_to_db_and_back() {
        let id = new_id();
        let s = uuid_to_db(id);
        let back = uuid_from_db(&s).expect("合法 UUID 应解析成功");
        assert_eq!(id, back);
    }

    #[test]
    fn uuid_from_db_invalid_returns_error() {
        let result = uuid_from_db("not-a-valid-uuid");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.category(), "validation");
        assert!(err.context().contains("not-a-valid-uuid"));
    }

    #[test]
    fn now_ms_is_reasonable() {
        let t = now_ms();
        assert!(t > 1_700_000_000_000, "timestamp too old: {t}");
        assert!(t < 2_600_000_000_000, "timestamp too far: {t}");
    }

    // ---- MessageRole ----

    /// MessageRole serde 序列化（小写字符串）与往返验证。
    #[test]
    fn message_role_serde_cases() {
        let cases = [
            (MessageRole::User, r#""user""#),
            (MessageRole::Assistant, r#""assistant""#),
            (MessageRole::System, r#""system""#),
            (MessageRole::Tool, r#""tool""#),
        ];
        for (role, expected) in cases {
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, expected, "{role:?} 应序列化为小写");
            let back: MessageRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
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
        let msg = Message::new(sid, MessageRole::User, "你好".into(), MessageSource::Local);
        assert_eq!(msg.session_id, sid);
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content, "你好");
        assert_eq!(msg.source, MessageSource::Local);
        assert!(msg.fingerprint.is_none());
        assert!(msg.persona_uid.is_none());
    }

    #[test]
    fn message_with_persona_uid() {
        let sid = new_id();
        let mut msg = Message::new(sid, MessageRole::User, "你好".into(), MessageSource::Local);
        msg.persona_uid = Some("user-0001".into());
        assert_eq!(msg.persona_uid.as_deref(), Some("user-0001"));
    }

    #[test]
    fn session_message_serde_roundtrip() {
        let session = Session::new();
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session.id, back.id);
        assert_eq!(session.started_at, back.started_at);

        let mut msg = Message::new(
            session.id,
            MessageRole::Assistant,
            "回复".into(),
            MessageSource::Online,
        );
        msg.persona_uid = Some("rama-0001".into());
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.session_id, back.session_id);
        assert_eq!(msg.role, back.role);
        assert_eq!(msg.content, back.content);
        assert_eq!(back.persona_uid.as_deref(), Some("rama-0001"));
    }

    // ---- MemoryL1 ----

    #[test]
    fn memory_l1_lifecycle() {
        let sid = new_id();
        let mut l1 = MemoryL1::new(sid, "摘要内容".into(), Some("上午".into()));
        assert!(!l1.absorbed);
        assert_eq!(l1.valence, 0.0);
        assert_eq!(l1.salience, 0.5);
        assert!(l1.last_accessed_at.is_none());
        assert!(l1.persona_uid.is_none());
        assert!(l1.context_json.is_none());

        l1.mark_absorbed();
        assert!(l1.absorbed);
        l1.touch();
        assert!(l1.last_accessed_at.is_some());
    }

    #[test]
    fn memory_l1_with_persona_context() {
        let sid = new_id();
        let mut l1 = MemoryL1::new(sid, "摘要".into(), None);
        l1.persona_uid = Some("char-0003".into());
        l1.context_json = Some(r#"{"chat_partners":["user-0001","char-0003"]}"#.into());
        let json = serde_json::to_string(&l1).unwrap();
        let back: MemoryL1 = serde_json::from_str(&json).unwrap();
        assert_eq!(back.persona_uid.as_deref(), Some("char-0003"));
        assert!(
            back.context_json
                .as_deref()
                .unwrap()
                .contains("chat_partners")
        );
    }

    // ---- Persona 枚举 serde ----

    #[test]
    fn persona_kind_serde() {
        assert_eq!(
            serde_json::to_string(&PersonaKind::User).unwrap(),
            r#""user""#
        );
        assert_eq!(
            serde_json::to_string(&PersonaKind::Rama).unwrap(),
            r#""rama""#
        );
        assert_eq!(
            serde_json::to_string(&PersonaKind::Char).unwrap(),
            r#""char""#
        );
        let back: PersonaKind = serde_json::from_str(r#""anim""#).unwrap();
        assert_eq!(back, PersonaKind::Anim);
    }

    #[test]
    fn trait_layer_serde() {
        assert_eq!(
            serde_json::to_string(&TraitLayer::Base).unwrap(),
            r#""base""#
        );
        assert_eq!(
            serde_json::to_string(&TraitLayer::Primary).unwrap(),
            r#""primary""#
        );
        assert_eq!(
            serde_json::to_string(&TraitLayer::Accent).unwrap(),
            r#""accent""#
        );
    }

    #[test]
    fn event_relation_kind_serde() {
        // PascalCase 序列化
        assert_eq!(
            serde_json::to_string(&EventRelationKind::CausedBy).unwrap(),
            r#""CausedBy""#
        );
        assert_eq!(
            serde_json::to_string(&EventRelationKind::Contradicts).unwrap(),
            r#""Contradicts""#
        );
        let back: EventRelationKind = serde_json::from_str(r#""Timeline""#).unwrap();
        assert_eq!(back, EventRelationKind::Timeline);
    }

    #[test]
    fn presentation_serde() {
        assert_eq!(
            serde_json::to_string(&Presentation::Objective).unwrap(),
            r#""objective""#
        );
        assert_eq!(
            serde_json::to_string(&Presentation::Subjective).unwrap(),
            r#""subjective""#
        );
        assert_eq!(
            serde_json::to_string(&Presentation::Mixed).unwrap(),
            r#""mixed""#
        );
    }

    #[test]
    fn trait_status_serde() {
        assert_eq!(
            serde_json::to_string(&TraitStatus::Active).unwrap(),
            r#""active""#
        );
        assert_eq!(
            serde_json::to_string(&TraitStatus::Deprecated).unwrap(),
            r#""deprecated""#
        );
    }

    #[test]
    fn profile_field_includes_speaking_style() {
        assert_eq!(ProfileField::SpeakingStyle.label(), "说话风格");
        assert_eq!(ProfileField::SpeakingStyle.as_str(), "speaking_style");
        // 确保原有字段不变
        assert_eq!(ProfileField::BasicInfo.label(), "基础信息");
        assert_eq!(ProfileField::PersonalStatus.label(), "近期状态");
    }

    // ---- Persona 结构体创建（id 初始为 0） ----

    #[test]
    fn persona_creation() {
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        assert_eq!(p.uid, "user-0001");
        assert_eq!(p.kind, PersonaKind::User);
        assert!(p.active);
        assert_eq!(p.id, 0); // 存储层回填前为 0
    }

    #[test]
    fn persona_fact_creation() {
        let f = PersonaFact::new(
            "user-0001".into(),
            ProfileField::BasicInfo,
            "姓名：小明".into(),
            FactSource::L1,
        );
        assert_eq!(f.persona_uid, "user-0001");
        assert_eq!(f.field, ProfileField::BasicInfo);
        assert_eq!(f.id, 0);
    }

    #[test]
    fn memory_event_creation() {
        let now = now_ms();
        let ev = MemoryEvent::new(
            "user-0001".into(),
            "跳槽".into(),
            "换了新工作".into(),
            now - 86_400_000,
            now,
        );
        assert_eq!(ev.persona_uid, "user-0001");
        assert_eq!(ev.confidence, 0.5);
        assert_eq!(ev.salience, 0.5);
        assert_eq!(ev.presentation, Presentation::Mixed);
        assert_eq!(ev.id, 0);
    }

    #[test]
    fn event_relation_creation() {
        let rel = EventRelation::new(1, 2, EventRelationKind::CausedBy);
        assert_eq!(rel.from_id, 1);
        assert_eq!(rel.to_id, 2);
        assert_eq!(rel.kind, EventRelationKind::CausedBy);
        assert_eq!(rel.weight, 0.5);
        assert_eq!(rel.id, 0);
    }

    #[test]
    fn event_source_creation() {
        let l1 = new_id();
        let src = EventSource::new(5, l1);
        assert_eq!(src.event_id, 5);
        assert_eq!(src.l1_id, l1);
        assert_eq!(src.weight, 1.0);
        assert_eq!(src.id, 0);
    }

    #[test]
    fn trait_evidence_creation() {
        let ev = TraitEvidence::new(1, 10, EvidenceDirection::Support, 0.85);
        assert_eq!(ev.trait_id, 1);
        assert_eq!(ev.event_id, 10);
        assert_eq!(ev.direction, EvidenceDirection::Support);
        assert!((ev.score - 0.85).abs() < f64::EPSILON);
        assert_eq!(ev.id, 0);
    }

    #[test]
    fn personality_trait_creation() {
        let pt = PersonalityTrait::new(
            "user-0001".into(),
            TraitLayer::Primary,
            "幽默".into(),
            "喜欢用自嘲化解尴尬".into(),
            TraitSource::Inferred,
            1,
        );
        assert_eq!(pt.trait_label, "幽默");
        assert_eq!(pt.layer, TraitLayer::Primary);
        assert_eq!(pt.status, TraitStatus::Active);
        assert_eq!(pt.confidence, 0.0);
        assert_eq!(pt.id, 0);
    }

    #[test]
    fn persona_example_creation() {
        let ex = PersonaExample::new(
            "char-0003".into(),
            "今天怎么样？".into(),
            "还行，刚跑完步".into(),
        );
        assert_eq!(ex.persona_uid, "char-0003");
        assert_eq!(ex.length, 7); // "还行，刚跑完步" = 7 个字符
        assert!(!ex.selected);
        assert_eq!(ex.id, 0);
    }

    #[test]
    fn cluster_snapshot_creation() {
        let cs = ClusterSnapshot::new("user-0001".into(), "工作".into(), "对挑战的兴奋感".into());
        assert_eq!(cs.persona_uid, "user-0001");
        assert_eq!(cs.category, "工作");
        assert!(cs.is_current);
        assert_eq!(cs.id, 0);
    }

    // ---- Persona 类型 serde 往返 ----

    #[test]
    fn persona_serde_roundtrip() {
        let mut p = Persona::new(
            "rama-0001".into(),
            "Ramaria".into(),
            PersonaKind::Rama,
            1,
            "local".into(),
        );
        p.id = 42;
        let json = serde_json::to_string(&p).unwrap();
        let back: Persona = serde_json::from_str(&json).unwrap();
        assert_eq!(back.uid, "rama-0001");
        assert_eq!(back.kind, PersonaKind::Rama);
        assert_eq!(back.id, 42);
    }

    #[test]
    fn memory_event_serde_roundtrip() {
        let mut ev = MemoryEvent::new(
            "user-0001".into(),
            "事件".into(),
            "描述".into(),
            now_ms() - 1000,
            now_ms(),
        );
        ev.id = 7;
        let json = serde_json::to_string(&ev).unwrap();
        let back: MemoryEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.title, "事件");
        assert_eq!(back.persona_uid, "user-0001");
        assert_eq!(back.id, 7);
    }

    #[test]
    fn personality_trait_serde_roundtrip() {
        let mut pt = PersonalityTrait::new(
            "user-0001".into(),
            TraitLayer::Base,
            "温和".into(),
            "待人接物温和".into(),
            TraitSource::Inferred,
            0,
        );
        pt.id = 3;
        let json = serde_json::to_string(&pt).unwrap();
        let back: PersonalityTrait = serde_json::from_str(&json).unwrap();
        assert_eq!(back.trait_label, "温和");
        assert_eq!(back.layer, TraitLayer::Base);
        assert_eq!(back.id, 3);
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
    fn llm_provider_accepts_legacy_kebab_form() {
        // 兼容旧 config.toml 模板的连字符写法 `lm-studio`：
        // 反序列化必须兼容（否则真实用户配置文件会被误判为损坏而回退默认值）。
        // TOML 通道的完整场景（[backend] 表内 provider = "lm-studio"）由
        // config.rs::v14_config_groups_missing_fields_fallback_to_defaults 覆盖。
        let back: LlmProvider = serde_json::from_str(r#""lm-studio""#).unwrap();
        assert_eq!(back, LlmProvider::LmStudio);
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
        assert_eq!(ds.capability.model_id, "deepseek-chat");

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
