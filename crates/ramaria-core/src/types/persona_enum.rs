//! crates/ramaria-core/src/types/persona_enum.rs - Ramaria 人格体系枚举模块
//!
//! 设计特点:
//! - 定义人格类型、画像字段与 trait 分层/来源枚举
//! - 覆盖事实来源、状态、层级等 fact 元数据枚举
//! - 定义 Presentation、EvidenceDirection 与 TraitStatus
//! - 各枚举提供 as_str/Display 等稳定字符串表示
//! - 所有枚举支持 serde，供跨层共享

use serde::{Deserialize, Serialize};

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
