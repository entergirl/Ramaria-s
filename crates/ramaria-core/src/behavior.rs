//! crates/ramaria-core/src/behavior.rs - 行为层（L3 行为模型）核心类型
//!
//! 设计特点:
//! - 定义行为规则 BehaviorRule 及其情境/反应/参数/证据结构，对齐算法说明书 v3.1 §4
//! - 规则 = 情境侧（situation）+ 行为侧（reaction/params/avoid）+ 元数据（evidence/confidence/stability/source/enabled）
//! - reaction 为 `Option<String>`：`Some` = 完整规则文本；`None` = 候选规则（仅参数注入，D4 质控降级轨道）
//! - situation/params/avoid/evidence 均以 JSON 形态持久化（DB 单列），serde 全量序列化/反序列化
//! - 反馈日志 FeedbackLog 对齐 v3.1 §9.4（S1 强信号 edit/disable 写入；S2/S3 弱信号 v1.7 复用同表只增不删）
//! - 纯类型定义，零 I/O，不依赖数据库或异步运行时
//!
//! 安全约束:
//! - 本模块不记录任何对话原文；evidence 只引用事件 id + 权重（原文溯源经事件表二次查询）
//! - avoid 列表只存"禁忌/注意"主题词，不存完整用户消息

use crate::types::{Presentation, now_ms};
use serde::{Deserialize, Serialize};

// =========================================================
// 规则来源与基础枚举
// =========================================================

/// 行为规则的来源。
///
/// 字段约定:
/// - `Auto`: 由学习管线自动生成（D2→D4），自动生效（无人工参与）。
/// - `Manual`: 用户手工导入或编辑产生，优先级高于 Auto（v3.1 §4.5 / §9.3 强锚点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleSource {
    /// 自动生成（学习管线）
    Auto,
    /// 人工导入/编辑
    Manual,
}

impl RuleSource {
    /// 返回数据库存储用的字符串表示。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

/// 反馈日志的标的类型（v3.1 §9.4）。
///
/// 字段约定:
/// - 本版本（v1.5 H1）只写 `BehaviorRule`；`PersonaFact` / `PersonalityTrait`
///   为 v1.7 H2 预留（表结构已建好，只增不删）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    /// 行为规则（v1.5 H1 S1 写入对象）
    BehaviorRule,
    /// 知识事实（预留）
    PersonaFact,
    /// 性格 trait（预留）
    PersonalityTrait,
}

/// 反馈信号类型（v3.1 §9.1 信号分级）。
///
/// 字段约定:
/// - S1 强信号：`Edit` / `Disable`，weight=1.0，直接校准（Manual 覆盖 Auto）。
/// - S2 中信号：`Correction`（纠正性消息），weight=0.6，候选复审（v1.7 实现）。
/// - S3 弱信号：`Continue`（回复后继续发言），weight=0.2，仅趋势统计（v1.7 实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalType {
    /// S1：显式编辑规则
    Edit,
    /// S1：显式禁用规则
    Disable,
    /// S2：纠正性消息（预留）
    Correction,
    /// S3：回复后继续发言（预留）
    Continue,
}

impl SignalType {
    /// S1 强信号判定（直接校准）。
    pub fn is_strong(&self) -> bool {
        matches!(self, Self::Edit | Self::Disable)
    }
}

// =========================================================
// 情境侧（situation）
// =========================================================

/// 单条事件的情境特征（用于聚类样本）。
///
/// 职责:
/// - 是"情境-反应对"的输入侧：情况 = 关键词 + situation_strength（不含 valence，
///   避免情绪信号污染情境判定，v3.1 §4.2 Step 1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SituationSample {
    /// 事件 id（证据引用）
    pub event_id: i64,
    /// 关键词集（去重后的小写词）
    pub keywords: Vec<String>,
    /// 情境通道向量 s_i = embedding(关键词拼接)（聚类前向量化，可空 = 降级纯关键词）
    pub situation_vector: Option<Vec<f32>>,
    /// 情境强度 1-5（None 等效 3）
    pub situation_strength: Option<i32>,
    /// 事件开始时间（Unix 毫秒，用于时间跨度计算）
    pub start_ms: i64,
}

/// 单条事件的反应侧（用于聚类样本与参数化）。
///
/// 职责:
/// - 是"情境-反应对"的输出侧：反应 = attitude + presentation + valence。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionSample {
    /// 事件 id（证据引用）
    pub event_id: i64,
    /// 反应通道向量 r_i = embedding(paraphrase ⊕ attitude)（可空 = 降级）
    pub reaction_vector: Option<Vec<f32>>,
    /// 去情境化态度文本（LLM 生成，用于翻译示例与向量化）
    pub paraphrase: Option<String>,
    /// 原始态度文本（示例展示用，不落日志）
    pub attitude: Option<String>,
    /// 情绪效价 -1.0..1.0
    pub valence: f64,
    /// 陈述方式
    pub presentation: Presentation,
    /// 显著性权重（证据量 salience 加权）
    pub salience: f64,
}

/// presentation 分布的一项。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PresentationFreq {
    /// 陈述方式
    pub presentation: Presentation,
    /// 该方式在簇内的频率 0.0..1.0（簇内占比）
    pub freq: f64,
}

/// 行为规则的情境侧特征（持久化 JSON）。
///
/// 职责:
/// - 描述规则适用的"情境"：关键词集 + 簇中心向量 + valence 分布 + presentation 分布等。
/// - 供 D5 情境路由做候选评分（cos(q, 簇中心) + 查询侧 Jaccard）。
///
/// 字段约定:
/// - `keywords`: 簇关键词并集（频次 Top-N）。
/// - `centroid`: 情境通道簇中心向量（路由 cos 项基准；embedding 不可用时为 None）。
/// - `response_centroid`: 反应通道簇中心向量（审计/增量更新参考；可空）。
/// - `valence_mean` / `valence_std`: 簇内加权 valence 均值与标准差（参数化与质控用）。
/// - `presentation_dist`: 簇内陈述方式分布（表达倾向参数化用）。
/// - `situation_strength_mean`: 簇内情境强度均值。
/// - `time_span_days`: 簇内事件时间跨度（天）。
/// - `trait_refs`: 关联画像 trait（可溯源，v3.1 §4.1；v1.5 不强制填充）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorSituation {
    /// 关键词集（簇关键词并集，频次 Top-N）
    pub keywords: Vec<String>,
    /// 情境通道簇中心向量（路由 cos 项基准）
    pub centroid: Option<Vec<f32>>,
    /// 反应通道簇中心向量
    pub response_centroid: Option<Vec<f32>>,
    /// 簇内加权 valence 均值 -1.0..1.0
    pub valence_mean: f64,
    /// 簇内 valence 标准差（质控：超限 0.5 降级候选规则）
    pub valence_std: f64,
    /// 簇内事件数（证据量基础）
    pub sample_count: usize,
    /// 簇内陈述方式分布
    pub presentation_dist: Vec<PresentationFreq>,
    /// 情境强度均值 1-5
    pub situation_strength_mean: f64,
    /// 事件时间跨度（天）
    pub time_span_days: f64,
    /// 关联画像 trait（可溯源，预留）
    pub trait_refs: Vec<String>,
}

impl BehaviorSituation {
    /// 空情境（无关键词、无向量）——用于手工规则导入时的占位。
    pub fn empty() -> Self {
        Self {
            keywords: Vec::new(),
            centroid: None,
            response_centroid: None,
            valence_mean: 0.0,
            valence_std: 0.0,
            sample_count: 0,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 0.0,
            trait_refs: Vec::new(),
        }
    }
}

// =========================================================
// 行为侧（reaction / params / avoid）
// =========================================================

/// 规则的结构化参数（v3.1 §4.1 params，供生成器微调）。
///
/// 字段约定:
/// - `emotional_intensity`: 情感强度 -1.0..1.0（= 簇内加权 valence，D4 Step 5）。
/// - `proactiveness`: 主动程度 0.0..1.0（倾向主动引导还是被动回应）。
/// - `detail_level`: 详细度 0.0..1.0（倾向简短还是展开说明）。
/// - `formality`: 正式度 0.0..1.0（倾向正式还是随意）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BehaviorParams {
    /// 情感强度 -1.0..1.0（= 加权 valence）
    pub emotional_intensity: f64,
    /// 主动程度 0.0..1.0
    pub proactiveness: f64,
    /// 详细度 0.0..1.0
    pub detail_level: f64,
    /// 正式度 0.0..1.0
    pub formality: f64,
}

impl Default for BehaviorParams {
    fn default() -> Self {
        Self {
            emotional_intensity: 0.0,
            proactiveness: 0.5,
            detail_level: 0.5,
            formality: 0.5,
        }
    }
}

// =========================================================
// 证据与规则主体
// =========================================================

/// 单条证据：规则 ← 事件 的可溯源引用（v3.1 §9.5 证据链）。
///
/// 职责:
/// - 支持 `rule evidence` 命令展示 规则 → 事件 → 原文 溯源链。
/// - 只存事件 id + 权重，原文经事件表二次查询（原文不落规则表，隐私红线）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BehaviorEvidence {
    /// 事件 id（memory_events.id）
    pub event_id: i64,
    /// 证据权重（salience 加权，近期事件加权后归一）
    pub weight: f64,
}

/// 行为规则（v3.1 §4.1 BehaviorRule）。
///
/// 职责:
/// - 表示一条"情境 → 反应"规则，供 D5 情境路由与 prompt 行为块注入使用。
/// - 规则文本为主（可解释、可编辑、注入 prompt 由 LLM 执行），结构化参数为辅。
///
/// 状态:
/// - `reaction = Some(..)`: 完整规则（含规则文本），可注入。
/// - `reaction = None`: 候选规则（仅参数注入，D4 质控降级轨道），路由时只合并 params。
/// - `enabled = false`: 规则被禁用（用户禁用后不参与路由，可再启用）。
///
/// 字段约定:
/// - `source = Manual` 时优先级高于 Auto（v3.1 §4.5），且作为聚类强锚点（§9.3）。
/// - `confidence`: 证据量 × 一致性（0.0..1.0）。
/// - `stability`: 跨时间一致性（0.0..1.0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorRule {
    /// 规则 id（INTEGER AUTOINCREMENT）
    pub id: i64,
    /// 所属 persona
    pub persona_uid: String,
    /// 情境侧特征
    pub situation: BehaviorSituation,
    /// 规则文本（None = 候选规则仅参数注入）
    pub reaction: Option<String>,
    /// 结构化参数
    pub params: BehaviorParams,
    /// 禁忌/注意列表
    pub avoid: Vec<String>,
    /// 证据链（事件 id + 权重）
    pub evidence: Vec<BehaviorEvidence>,
    /// 置信度 0.0..1.0
    pub confidence: f64,
    /// 稳定性 0.0..1.0
    pub stability: f64,
    /// 来源（Auto | Manual）
    pub source: RuleSource,
    /// 是否启用（参与路由）
    pub enabled: bool,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 最近更新时间（Unix 毫秒）
    pub updated_at: i64,
}

impl BehaviorRule {
    /// 创建一条新规则（id=0，由存储层回填）。
    ///
    /// 参数:
    /// - `persona_uid`: 规则所属人格。
    /// - `situation`: 情境侧特征。
    /// - `reaction`: 规则文本（None = 候选规则）。
    /// - `params`: 结构化参数。
    /// - `source`: 规则来源。
    ///
    /// 返回:
    /// - 带当前时间戳、默认 enabled=true 的规则。
    pub fn new(
        persona_uid: impl Into<String>,
        situation: BehaviorSituation,
        reaction: Option<String>,
        params: BehaviorParams,
        source: RuleSource,
    ) -> Self {
        let now = now_ms();
        Self {
            id: 0,
            persona_uid: persona_uid.into(),
            situation,
            reaction,
            params,
            avoid: Vec::new(),
            evidence: Vec::new(),
            confidence: 0.0,
            stability: 0.0,
            source,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// 是否为完整规则（含规则文本）。
    pub fn has_reaction(&self) -> bool {
        self.reaction.is_some()
    }

    /// 是否为候选规则（仅参数注入）。
    pub fn is_candidate(&self) -> bool {
        self.reaction.is_none()
    }
}

// =========================================================
// 反馈日志（v3.1 §9.4，H1 S1 写入）
// =========================================================

/// 反馈日志条目。
///
/// 职责:
/// - 记录用户对规则/事实/trait 的显式干预（S1 强信号），供校准与审计。
/// - v1.5 H1 只写入 edit/disable（weight=1.0）；S2/S3 弱信号 v1.7 复用同表。
///
/// 安全约束:
/// - `detail` 只存"编辑前后快照"（规则字段 JSON），不存完整对话原文。
/// - `session_id` 可选（标记干预发生在哪个会话，用于审计关联）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackLog {
    /// 日志 id（INTEGER AUTOINCREMENT）
    pub id: i64,
    /// 所属 persona
    pub persona_uid: String,
    /// 标的类型
    pub target_type: TargetType,
    /// 标的 id（如 behavior_rules.id 的字符串形式）
    pub target_id: String,
    /// 信号类型
    pub signal_type: SignalType,
    /// 信号权重（S1 = 1.0）
    pub weight: f64,
    /// 干预发生的会话（可选）
    pub session_id: Option<String>,
    /// 编辑前后快照 JSON（可选）
    pub detail: Option<String>,
    /// 记录时间（Unix 毫秒）
    pub created_at: i64,
}

impl FeedbackLog {
    /// 创建一条 S1 反馈日志（weight 由信号类型决定）。
    ///
    /// 参数:
    /// - `persona_uid`: 所属人格。
    /// - `target_type`: 标的类型。
    /// - `target_id`: 标的 id。
    /// - `signal_type`: 信号类型（Edit/Disable 为 S1 强信号）。
    /// - `session_id`: 干预发生的会话（可选）。
    /// - `detail`: 编辑前后快照 JSON（可选）。
    ///
    /// 返回:
    /// - id=0（由存储层回填）、created_at=当前时间、weight 按 S1=1.0 的日志。
    pub fn new(
        persona_uid: impl Into<String>,
        target_type: TargetType,
        target_id: impl Into<String>,
        signal_type: SignalType,
        session_id: Option<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            id: 0,
            persona_uid: persona_uid.into(),
            target_type,
            target_id: target_id.into(),
            signal_type,
            weight: if signal_type.is_strong() { 1.0 } else { 0.6 },
            session_id,
            detail,
            created_at: now_ms(),
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
    fn rule_source_serde() {
        assert_eq!(
            serde_json::to_string(&RuleSource::Auto).unwrap(),
            r#""auto""#
        );
        assert_eq!(
            serde_json::to_string(&RuleSource::Manual).unwrap(),
            r#""manual""#
        );
        assert_eq!(RuleSource::Auto.as_str(), "auto");
        assert_eq!(RuleSource::Manual.as_str(), "manual");
    }

    #[test]
    fn target_type_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&TargetType::BehaviorRule).unwrap(),
            r#""behavior_rule""#
        );
        assert_eq!(
            serde_json::to_string(&TargetType::PersonaFact).unwrap(),
            r#""persona_fact""#
        );
        assert_eq!(
            serde_json::to_string(&TargetType::PersonalityTrait).unwrap(),
            r#""personality_trait""#
        );
    }

    #[test]
    fn signal_type_strong_detection() {
        assert!(SignalType::Edit.is_strong());
        assert!(SignalType::Disable.is_strong());
        assert!(!SignalType::Correction.is_strong());
        assert!(!SignalType::Continue.is_strong());
    }

    #[test]
    fn feedback_log_s1_weight_is_1_0() {
        let log = FeedbackLog::new(
            "char-0001",
            TargetType::BehaviorRule,
            "3",
            SignalType::Disable,
            Some("sess-1".into()),
            None,
        );
        assert_eq!(log.weight, 1.0);
        assert!(log.id == 0);
        assert!(log.created_at > 0);
    }

    #[test]
    fn behavior_params_default_clamped_range() {
        let p = BehaviorParams::default();
        assert_eq!(p.emotional_intensity, 0.0);
        assert!((0.0..=1.0).contains(&p.proactiveness));
        assert!((0.0..=1.0).contains(&p.detail_level));
        assert!((0.0..=1.0).contains(&p.formality));
    }

    #[test]
    fn behavior_rule_roundtrip_json() {
        let rule = BehaviorRule::new(
            "char-0001",
            BehaviorSituation {
                keywords: vec!["加班".into(), "累".into()],
                centroid: Some(vec![0.1, 0.2, 0.3]),
                response_centroid: None,
                valence_mean: -0.4,
                valence_std: 0.2,
                sample_count: 6,
                presentation_dist: vec![PresentationFreq {
                    presentation: Presentation::Subjective,
                    freq: 0.8,
                }],
                situation_strength_mean: 3.5,
                time_span_days: 20.0,
                trait_refs: vec![],
            },
            Some("当聊到加班时，倾向表达疲惫并安慰对方。".into()),
            BehaviorParams::default(),
            RuleSource::Auto,
        );
        let json = serde_json::to_string(&rule).unwrap();
        let back: BehaviorRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, back);
        assert!(back.has_reaction());
        assert!(!back.is_candidate());
    }

    #[test]
    fn candidate_rule_has_no_reaction() {
        let rule = BehaviorRule::new(
            "char-0001",
            BehaviorSituation::empty(),
            None,
            BehaviorParams::default(),
            RuleSource::Auto,
        );
        assert!(rule.is_candidate());
        assert!(!rule.has_reaction());
    }

    #[test]
    fn evidence_chain_is_id_and_weight_only() {
        let ev = BehaviorEvidence {
            event_id: 42,
            weight: 0.8,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BehaviorEvidence = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
        assert!(!json.contains("原文"), "证据 JSON 不应含任何原文内容");
    }
}
