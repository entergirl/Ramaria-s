//! crates/ramaria-service/src/proactive/topic.rs - Ramaria 主动生成指令与结果形态
//!
//! 设计特点:
//! - 跨模块契约：选题器 / 调度与生成用例之间的输入输出形态，独立于 Protocol 与 serde
//! - 指令形态承载生成所需全部素材与调度记账信号：人格 / 目标会话 / 来源标识 /
//!   锚点 / 角度 / 语气 / 候选效价
//! - 结果形态只含投递所需元数据：落库消息 id / 内容 / 落点会话 / 人格 / 来源标识
//! - 纯数据：无 I/O、无业务逻辑，字段取值口径由调用方（选题器 / 生成用例）约定
//! - 日志隐私：两端形态可整体记录字段名与长度，不承载日志输出职责

use uuid::Uuid;

// =========================================================
// 主动生成指令与结果
// =========================================================

/// 主动生成指令（选题与判据裁决的产物，供生成用例消费）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProactiveDirective {
    /// 目标人格 uid。
    pub persona: String,
    /// 目标会话（None = 问候类新建；Some = 事件跟进类落所属会话）。
    pub session_id: Option<Uuid>,
    /// 选题来源标识（如 `event` / `rule` / `time_node` / `light_touch`；仅日志与透传，不参与生成逻辑）。
    pub source: String,
    /// 选题键（来源内稳定标识，如事件 id 的文本形态；None = 无稳定键，不参与去重冷却）。
    pub topic_key: Option<String>,
    /// 候选锚点摘要（判据与生成共见；None = 轻触达无具体话题）。
    pub anchor: Option<String>,
    /// 开口角度（None = 未提供）。
    pub angle: Option<String>,
    /// 说话语气（None = 未提供）。
    pub tone: Option<String>,
    /// 候选效价（-1.0~1.0）：供调度记账「不连选」符号；
    /// 轻触达等无情绪信号候选填 0.0。
    pub valence: f64,
}

/// 主动生成结果（成功生成并落库后返回，供调度投递消费）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProactiveOutcome {
    /// 落库消息 id（投递端幂等键与通知定位依据）。
    pub message_id: Uuid,
    /// 生成内容（assistant 消息全文）。
    pub content: String,
    /// 消息落点会话。
    pub session_id: Uuid,
    /// 人格 uid。
    pub persona: String,
    /// 选题来源标识（从指令透传）。
    pub source: String,
    /// 选题键（从指令透传；None = 无稳定键）。
    pub topic_key: Option<String>,
    /// 候选效价（从指令透传；-1.0~1.0；无情绪信号候选为 0.0）。
    pub valence: f64,
}
