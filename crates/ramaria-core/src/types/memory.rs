//! crates/ramaria-core/src/types/memory.rs - Ramaria 记忆与事件数据类型模块
//!
//! 设计特点:
//! - 覆盖 L1 摘要、结构化证据线索与原文话语块溯源
//! - 定义 L2 事件、事件关系与来源、批量写入载体
//! - PersonaEventAggregate 汇总 persona 事件画像
//! - ClusterSnapshot 记录聚类快照与去重指纹
//! - ID 双轨制与 Unix 毫秒时间规范保持一致

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Presentation, new_id, now_ms};

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
/// - `text`: 必填，证据文本。结构体不做长度校验，非空与长度约束由提取侧保证。
/// - `time`: 可选，事件发生的时间描述（如"上周三晚上"）。
/// - `who`: 可选，涉及的人物/角色。
/// - `cause`: 可选，可辨时的因果线索（缺失留空，供背景参考）。
///
/// 迁移约定（一次性迁移）:
/// - 存量旧格式（字符串数组）由 migration 一次性迁移，字符串落 `text` 槽位、其余置空。
/// - 运行时不做兼容解析，读写均为新格式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceNote {
    /// 证据文本（必填；长度由提取侧约束，此处不校验）
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

/// 单个 persona 的事件级经验分布聚合行（跨用户冷启动先验的数据源）。
///
/// 职责:
/// - 由存储层对 `memory_events` 表做原始 SQL 聚合产生，供 L3 分层收缩
///   构造系统内"已有人格画像的跨用户经验先验"。
/// - 只承载存储层的聚合结果；样本量阈值判定与跨 persona 加权合并
///   （业务语义）由 ramaria-memory 的 shrink 模块负责。
///
/// 字段约定:
/// - `n_events`: 该 persona 参与聚合的事件条数（原始计数，非 salience 加权）。
/// - `valence_mean` / `share_mean`: 事件级简单均值（AVG），取值范围与
///   `MemoryEvent` 对应字段一致（valence -1.0..1.0、share 0.0..1.0）。
/// - `obj_ratio` / `sub_ratio` / `mix_ratio`: presentation 三态的事件计数占比，和为 1。
///
/// 口径说明:
/// - 本聚合行是"事件级"经验分布，不携带分类上下文与 salience 加权语义；
///   与 Phase A 分类内 `CategoryStats`（salience 加权）口径不同，仅作为跨 persona
///   的经验方向锚点，用于冷启动校准小样本分类。
/// - `memory_events.presentation` 在 SQLite 中以小写字符串存储
///   （`objective` / `subjective` / `mixed`）。
#[derive(Debug, Clone)]
pub struct PersonaEventAggregate {
    /// 已有人格画像标识（存储层聚合时已排除目标 persona）
    pub persona_uid: String,
    /// 该 persona 参与聚合的事件条数
    pub n_events: u64,
    /// valence 事件级均值（-1.0..1.0）
    pub valence_mean: f64,
    /// share 事件级均值（0.0..1.0）
    pub share_mean: f64,
    /// presentation 三态中 objective 的占比
    pub obj_ratio: f64,
    /// presentation 三态中 subjective 的占比
    pub sub_ratio: f64,
    /// presentation 三态中 mixed 的占比
    pub mix_ratio: f64,
}

impl PersonaEventAggregate {
    /// 构造一条聚合行（存储层查询转换与测试构造使用）。
    ///
    /// 参数:
    /// - 各字段含义见 struct 注释；`n_events` 必须 > 0 才表示存在经验来源。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        persona_uid: impl Into<String>,
        n_events: u64,
        valence_mean: f64,
        share_mean: f64,
        obj_ratio: f64,
        sub_ratio: f64,
        mix_ratio: f64,
    ) -> Self {
        Self {
            persona_uid: persona_uid.into(),
            n_events,
            valence_mean,
            share_mean,
            obj_ratio,
            sub_ratio,
            mix_ratio,
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

/// 事件批次写入请求：把一批事件、其来源链接、事件关系与 L1 吸收标记放进单事务。
///
/// 职责:
/// - 承载 L2 事件提取管线单次运行的整批写入结果，供存储层原子落库；
/// - 任一环节失败时整体回滚，杜绝"事件半写入 / 证据链缺失"，
///   使上层失败重试不会产生半批数据。
///
/// 下标约定:
/// - `sources` / `relations` 中的下标均指向 `events` 数组位置
///   （事件数据库 id 由事务内自增分配，写入前未知）。
#[derive(Debug, Clone, Default)]
pub struct EventBatchWrite {
    /// 待写入事件（顺序与返回值 `event_ids` 一一对应）
    pub events: Vec<MemoryEvent>,
    /// 来源链接：(事件在 events 中的下标, L1 id, 权重)
    pub sources: Vec<(usize, Uuid, f64)>,
    /// 事件关系：(from 事件下标, to 事件下标, 关系类型, 权重)
    pub relations: Vec<(usize, usize, EventRelationKind, f64)>,
    /// 随本事务一并标记为已吸收的 L1 id（空则不执行 UPDATE）
    pub absorbed_l1_ids: Vec<Uuid>,
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
        // `is_multiple_of` 需 Rust 1.87、`as_chunks` 需 Rust 1.88，
        // 与 workspace `rust-version = 1.88` 一致。
        if blob.is_empty() || !blob.len().is_multiple_of(4) {
            return None;
        }
        let count = blob.len() / 4;
        let mut vec = Vec::with_capacity(count);
        // 长度已校验为 4 的倍数，as_chunks 余数恒为空，逐块还原 f32。
        for chunk in blob.as_chunks::<4>().0 {
            vec.push(f32::from_le_bytes(*chunk));
        }
        Some(vec)
    }
}
