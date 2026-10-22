//! crates/ramaria-service/src/proactive/schedule.rs - Ramaria 主动对话调度与打扰控制
//!
//! 设计特点:
//! - 单轮判定链：硬闸门（可见性 / 人格主动开关 / 状态 / 宽限 / 免打扰 / 每日上限 /
//!   全局日上限 / 冷却 / 退避 / 最小空闲 / 隐私）→ 活跃时段门与时机合成 → 判据节流 →
//!   选题 → 生成 → 投放
//! - 时钟注入：单轮入口接收时间戳，测试与宿主时钟对齐均不需改实现
//! - 状态按人格隔离：处理尾部统一回写（跳过路径的跨日重置 / 宽限基准同样落盘）
//! - 全局日上限：启用时单轮加载全局状态（跨日重置 + 生成计数）并整轮回写；
//!   不限时不读不写全局状态
//! - 降级纪律：单人格失败不阻塞其余；投递接收端未注册时静默丢弃
//! - 投放语义：按负载通道路由（本地 / 社交通道分流）；本地路径生成成功即记投递
//!   时间、当日计数与最近效价符号（消息已落库应用内可见），仅投递成功才记选题
//!   冷却；通道消息先过四层门禁（任一不过丢弃），平台投递失败不记账
//!   （消息未送达不消耗额度，允许下一窗口重试）
//! - 隐私：日志只记人格 / 来源 / 计数等元数据，不记消息内容

mod cycle;
mod deliver;
mod gates;
mod quiet;
mod round;
mod types;

pub(crate) use cycle::spawn;
pub(crate) use deliver::{deliver, record_delivery};
pub(crate) use gates::{GateOutcome, evaluate_recall_gates};
#[cfg(test)]
pub(crate) use round::run_tick;
#[cfg(test)]
pub(crate) use types::ProactiveTickSummary;
pub(crate) use types::{DeliveryOutcome, TopicPicker};

// =========================================================
// 常量
// =========================================================

/// 近期选题记录上限（超出丢弃最旧；状态键防膨胀）。
const RECENT_TOPICS_CAP: usize = 64;
/// 效价符号判定阈值：`|valence|` 低于该值视为中性（不参与"不连选"符号判定）。
const VALENCE_SIGN_EPSILON: f64 = 0.1;
/// 可中断等待的分片长度（秒）。
const SHUTDOWN_POLL_CHUNK_SECONDS: u64 = 60;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
