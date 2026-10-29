//! crates/ramaria-service/src/types/cascade.rs - Ramaria 记忆级联入口类型
//!
//! 设计特点:
//! - 目标层用枚举表达：L2 只做事件提取，L3 继续性格推断（跑到该层为止）
//! - 计数口径面向「本次调用实际推进的量」：不累计历史状态，重复调用可观测为零
//! - 纯数据类型：不含行为与传输概念，供无桌面的批处理入口直接读取与序列化

/// 级联目标层（跑到该层为止）。
///
/// 字段约定:
/// - `L2`: 只做 L1 → L2 事件提取，不触发 L3；
/// - `L3`: 事件提取后继续 L3 性格推断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeUntil {
    /// 跑到 L2 事件提取为止。
    L2,
    /// 跑到 L3 性格推断为止。
    L3,
}

/// 一次级联的运行计数。
///
/// 字段约定:
/// - `persona_uid`: 本次处理的人格（级联只处理该 persona 的记忆）；
/// - `l1_consumed`: 本次离开「未吸收 L1」集合的条数（提取失败时按 0 计，不虚报）；
/// - `l2_events`: 本次新建的 L2 事件数；
/// - `l3_traits`: 本次写入的活跃标签数（`traits_saved + traits_updated`；未跑 L3 恒 0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeOutcome {
    pub persona_uid: String,
    pub l1_consumed: usize,
    pub l2_events: usize,
    pub l3_traits: usize,
}
