//! crates/ramaria-core/src/types/persona_struct.rs - Ramaria 人格体系结构体模块
//!
//! 设计特点:
//! - 定义 Persona 人格注册与画像字段
//! - PersonaFact 表示结构化画像事实
//! - TraitEvidence / PersonalityTrait 构成 L3 性格推断证据链
//! - 提供构造与状态辅助方法
//! - 所有类型支持 serde，时间统一使用 Unix 毫秒

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    EvidenceDirection, FactSource, FactStatus, FactTier, PersonaKind, ProfileField, TraitLayer,
    TraitSource, TraitStatus, now_ms,
};

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
