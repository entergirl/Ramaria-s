//! crates/ramaria-service/src/types/eval.rs - Ramaria 评测生成入口数据结构
//!
//! 设计特点:
//! - 层掩码覆盖九层注入面：`all()` 与产品默认等价，`memory_only()` 只留记忆四路
//! - 查询时钟与访问时间刷新由选项显式声明：默认不指定时钟、不刷新访问时间
//! - 预置上下文用于 full-context 对照：`Some` 时跳过召回直接注入，`None` 时走常规召回
//! - 观测只记分层 / 标识 / 分数 / 字数 / token，不含记忆原文与提示词文本
//! - 产物侧结构可序列化，矩阵文件可反序列化选项（枚举为纯 bool 字段，缺项即解析失败）
//! - 生成结果携带 provider 上报的 token 用量（含缓存命中），只存数值不存文本

use ramaria_core::traits::LlmUsage;
use ramaria_memory::trace::RecallTrace;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =========================================================
// 注入层掩码
// =========================================================

/// 注入层掩码（评测消融的九层开关）。
///
/// 字段约定:
/// - `l1` / `l2`: 摘要路记忆子层（未全开时按层收窄段落与条目）；
/// - `l3`: 性格标签（素材层的 traits 块）；
/// - `utt`: 原文块通道（仍受配置与宿主策略约束）；
/// - `knowledge`: 知识层（判定器检索 + 角色层事实素材）；
/// - `narrative`: 脉络素材；
/// - `behavior`: 行为层情境路由；
/// - `style`: 表达层自动风格规则；
/// - `examples`: 示例预选与注入。
///
/// 说明:
/// - 掩码只做减法：开启不等于必然注入（配置闸门、检索命中等条件照常生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalInjectionMask {
    pub l1: bool,
    pub l2: bool,
    pub l3: bool,
    pub utt: bool,
    pub knowledge: bool,
    pub narrative: bool,
    pub behavior: bool,
    pub style: bool,
    pub examples: bool,
}

impl EvalInjectionMask {
    /// 九层全开（等价产品默认注入面）。
    pub fn all() -> Self {
        Self {
            l1: true,
            l2: true,
            l3: true,
            utt: true,
            knowledge: true,
            narrative: true,
            behavior: true,
            style: true,
            examples: true,
        }
    }

    /// 只开记忆四路（L1 / L2 / L3 / utt），其余注入层关闭。
    pub fn memory_only() -> Self {
        Self {
            l1: true,
            l2: true,
            l3: true,
            utt: true,
            knowledge: false,
            narrative: false,
            behavior: false,
            style: false,
            examples: false,
        }
    }
}

impl Default for EvalInjectionMask {
    /// 默认九层全开（与产品默认一致）。
    fn default() -> Self {
        Self::all()
    }
}

// =========================================================
// 选项与观测
// =========================================================

/// 评测生成选项。
///
/// 字段约定:
/// - `as_of_ms`: 查询时钟（Unix 毫秒）；`None` = 以当前时间为衰减基准、不按未来过滤；
/// - `mask`: 注入层掩码（默认九层全开）；
/// - `touch`: 召回命中后是否刷新 L1 访问时间（默认 `false`：条件间与续跑可复现）；
/// - `max_chars`: 记忆类注入的总字符预算（记忆上下文优先，原文片段用剩余预算）；`None` = 不截断；
/// - `preset_context`: 预置上下文；`Some` 时跳过召回，直接作为记忆上下文注入（评测对照用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalChatOptions {
    pub as_of_ms: Option<i64>,
    pub mask: EvalInjectionMask,
    pub touch: bool,
    #[serde(default)]
    pub max_chars: Option<u32>,
    #[serde(default)]
    pub preset_context: Option<String>,
}

impl Default for EvalChatOptions {
    /// 默认：不指定查询时钟、九层全开、不刷新访问时间、不截断、无预置上下文。
    fn default() -> Self {
        Self {
            as_of_ms: None,
            mask: EvalInjectionMask::all(),
            touch: false,
            max_chars: None,
            preset_context: None,
        }
    }
}

/// 召回条目观测（分层 / 标识 / 分数，不含记忆原文）。
///
/// 字段约定:
/// - `layer`: 召回分层（`l1` / `l2` / `graph`）；
/// - `id`: 文档标识（`L1:{uuid}` / `L2:{id}` / `graph:{实体}`）；
/// - `score`: 衰减重排后的融合分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalRecallItem {
    pub layer: String,
    pub id: String,
    pub score: f64,
}

/// 召回观测（条目 + 轨迹 + 注入体量）。
///
/// 字段约定:
/// - `items`: Persona-Aware 过滤与条数预算后的命中；
/// - `trace`: 召回各阶段轨迹（通道候选、融合、丢弃、预算、utt）；
/// - `context_chars`: 记忆上下文文本字符数（未注入为 0）；
/// - `injected_tokens`: 记忆上下文与原文片段的估算 token 之和（产品口径的 `estimate_tokens`）；
/// - `utt_chars`: 原文片段字符数（未注入为 0）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalRecallObservation {
    pub items: Vec<EvalRecallItem>,
    pub trace: RecallTrace,
    pub context_chars: usize,
    pub injected_tokens: usize,
    pub utt_chars: usize,
}

/// 评测生成结果。
///
/// 字段约定:
/// - `reply`: 人格回复全文；
/// - `session_id`: 本轮对话所属会话（复用或新建）；
/// - `chars`: 回复字符数；
/// - `recall`: 本轮召回观测（无召回时为空观测，不省略字段）；
/// - `usage`: 本次生成的 token 用量；provider 未上报或缓存命中时为 `None`（旧产物缺该键）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalChatOutcome {
    pub reply: String,
    pub session_id: Uuid,
    pub chars: usize,
    pub recall: EvalRecallObservation,
    #[serde(default)]
    pub usage: Option<LlmUsage>,
}
