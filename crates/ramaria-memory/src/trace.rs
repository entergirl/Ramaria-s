//! crates/ramaria-memory/src/trace.rs - Ramaria 召回链路诊断轨迹模块
//!
//! 设计特点:
//! - 纯数据类型：承载四通道候选、融合分、衰减后分、过滤原因与预算裁剪结果，零 I/O
//! - 只记标识与标量：id、分数、字符数与条数，不含查询文本与记忆原文，可直接序列化落盘
//! - 默认不采集：装配入口不传选项时整条链路不构造任何 Vec，产品路径行为与开销不变
//! - 枚举统一 snake_case 序列化，便于评估产物跨运行读取

use serde::{Deserialize, Serialize};

// =========================================================
// 检索通道
// =========================================================

/// 检索通道标识。
///
/// 职责:
/// - 标出轨迹条目来自哪条检索通道；与 `rrf::OptionalChannels` 的四个通道身份一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceChannel {
    /// 向量通道
    Vector,
    /// BM25 全文通道
    Bm25,
    /// 图谱通道
    Graph,
    /// 关键词镜像通道
    Keyword,
}

/// 单通道候选。
///
/// 字段约定:
/// - `id`: 文档 label 原字符串（`L1:{uuid}` / `L2:{id}` / `graph:{实体}`）；
/// - `raw_score`: 该通道的原始分，不做跨通道归一；
/// - `rank`: 通道内排序名次，从 1 起。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelCandidate {
    pub id: String,
    pub raw_score: f64,
    pub rank: usize,
}

/// 单通道轨迹。
///
/// 字段约定:
/// - `active`: 该通道本次是否参与融合（启用且有结果）；
/// - `candidates`: 通道原始顺序的候选；通道关闭或无结果时为空 Vec。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelTrace {
    pub channel: TraceChannel,
    pub active: bool,
    pub candidates: Vec<ChannelCandidate>,
}

// =========================================================
// 融合与过滤
// =========================================================

/// 融合条目轨迹。
///
/// 字段约定:
/// - `id`: 文档 label；
/// - `layer`: 解析出的分层（`l1` / `l2` / `graph`）；解析失败留空字符串；
/// - `rrf_score`: 衰减前融合分；
/// - `decay_factor`: 衰减因子；未进入衰减阶段时为 1.0；
/// - `decayed_score`: 衰减后分数；未进入衰减阶段时等于 `rrf_score`；
/// - 四个 `*_raw`: 该文档在各通道的原始分，未出现于该通道为 `None`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusedEntryTrace {
    pub id: String,
    pub layer: String,
    pub rrf_score: f64,
    pub decay_factor: f64,
    pub decayed_score: f64,
    pub vector_raw: Option<f64>,
    pub bm25_raw: Option<f64>,
    pub graph_raw: Option<f64>,
    pub keyword_raw: Option<f64>,
}

/// 条目被丢弃的原因。
///
/// 职责:
/// - 区分丢弃发生在哪一阶段：`Future` / `FusionCut` / `PersonaMismatch` / `DocMissing` 由检索器记录，
///   `LayerFiltered` / `PersonaShare` / `MaxMemories` 由召回装配记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// 融合截断未进入融合结果
    FusionCut,
    /// 记忆子层开关剔除
    LayerFiltered,
    /// Persona-Aware share 阈值剔除
    PersonaShare,
    /// 条数预算剔除
    MaxMemories,
    /// 文档已不在检索器视图内
    DocMissing,
    /// 文档归属人格与请求目标不一致
    PersonaMismatch,
    /// 记忆时间晚于查询时钟（未来单元，融合前剔除）
    Future,
}

/// 被丢弃条目。
///
/// 字段约定:
/// - `id`: 文档 label 或（预算清单中的）单元标识；
/// - `score`: 丢弃时刻该条目的分数；无分数语义时为 0。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DroppedTrace {
    pub id: String,
    pub reason: DropReason,
    pub score: f64,
}

// =========================================================
// 阶段轨迹
// =========================================================

/// 检索阶段轨迹。
///
/// 字段约定:
/// - `channels`: 四个通道各一条，缺席通道 `active=false`；
/// - `fused`: 融合结果（衰减回填后按衰减后分数重排）；
/// - `dropped`: 检索器内部丢弃（融合截断 / 文档缺失 / 人格不一致）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RetrievalTrace {
    pub channels: Vec<ChannelTrace>,
    pub fused: Vec<FusedEntryTrace>,
    pub dropped: Vec<DroppedTrace>,
}

/// 条数预算轨迹。
///
/// 字段约定:
/// - `max_memories`: 本次生效的条数上限；
/// - `candidates`: Persona-Aware 过滤后的候选条数；
/// - `kept`: 实际进入上下文的条数；
/// - `tokens_before` / `tokens_after`: 裁剪前候选摘要的估算 token 之和 / 最终上下文文本的估算 token。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetTrace {
    pub max_memories: usize,
    pub candidates: usize,
    pub kept: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

/// utt 原文通道轨迹。
///
/// 字段约定:
/// - `active`: 本次是否对该 persona 生效（闸门与白名单同时满足）；
/// - `candidates`: 检索命中，按返回顺序编号；
/// - `rendered_chars`: 渲染结果字符数（未渲染为 0）；
/// - `budget_chars`: 原文注入字符预算。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UttTrace {
    pub active: bool,
    pub candidates: Vec<ChannelCandidate>,
    pub rendered_chars: usize,
    pub budget_chars: usize,
}

/// 一次召回的完整轨迹。
///
/// 字段约定:
/// - `query_chars`: 查询文本字符数（只记长度，不记文本）；
/// - `query_vector_dim`: 查询向量维度，未生成时为 `None`；
/// - `retrieval`: 检索阶段轨迹；
/// - `dropped`: 装配阶段丢弃（子层过滤 / 人格过滤 / 条数预算）；
/// - `budget`: 条数预算轨迹；
/// - `utt`: 原文通道轨迹。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallTrace {
    pub query_chars: usize,
    pub query_vector_dim: Option<usize>,
    pub retrieval: RetrievalTrace,
    pub dropped: Vec<DroppedTrace>,
    pub budget: BudgetTrace,
    pub utt: UttTrace,
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 带值轨迹经 JSON 序列化往返后逐字段相等（字段名与结构可用于外部产物）。
    #[test]
    fn recall_trace_serde_round_trip() {
        let trace = RecallTrace {
            query_chars: 12,
            query_vector_dim: Some(768),
            retrieval: RetrievalTrace {
                channels: vec![
                    ChannelTrace {
                        channel: TraceChannel::Bm25,
                        active: true,
                        candidates: vec![
                            ChannelCandidate {
                                id: "L1:0b0f0f8e-0000-4000-8000-000000000001".to_string(),
                                raw_score: 3.5,
                                rank: 1,
                            },
                            ChannelCandidate {
                                id: "L2:42".to_string(),
                                raw_score: 1.25,
                                rank: 2,
                            },
                        ],
                    },
                    ChannelTrace {
                        channel: TraceChannel::Vector,
                        active: false,
                        candidates: Vec::new(),
                    },
                ],
                fused: vec![FusedEntryTrace {
                    id: "L2:42".to_string(),
                    layer: "l2".to_string(),
                    rrf_score: 0.0328,
                    decay_factor: 0.87,
                    decayed_score: 0.0285,
                    vector_raw: None,
                    bm25_raw: Some(1.25),
                    graph_raw: Some(0.5),
                    keyword_raw: Some(0.4),
                }],
                dropped: vec![DroppedTrace {
                    id: "graph:加班".to_string(),
                    reason: DropReason::FusionCut,
                    score: 0.9,
                }],
            },
            dropped: vec![DroppedTrace {
                id: "L2:43".to_string(),
                reason: DropReason::MaxMemories,
                score: 0.01,
            }],
            budget: BudgetTrace {
                max_memories: 5,
                candidates: 6,
                kept: 5,
                tokens_before: 120,
                tokens_after: 96,
            },
            utt: UttTrace {
                active: true,
                candidates: vec![ChannelCandidate {
                    id: "L0:7".to_string(),
                    raw_score: 0.81,
                    rank: 1,
                }],
                rendered_chars: 240,
                budget_chars: 1500,
            },
        };

        let json = serde_json::to_string(&trace).expect("轨迹应可序列化");
        let back: RecallTrace = serde_json::from_str(&json).expect("轨迹应可反序列化");
        assert_eq!(back, trace);
    }
}
