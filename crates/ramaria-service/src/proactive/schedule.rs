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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tracing::{debug, error, info, warn};

use ramaria_core::config::ProactiveConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{CHANNEL_QQ, CHANNEL_TELEGRAM, Persona, now_ms};

use crate::engine::Engine;

use super::activity::{self, ActivityGate};
use super::sink::ProactiveMessage;
use super::state::{self, ProactiveGlobalState, ProactiveState, RecentTopic};
use super::topic::{ProactiveDirective, ProactiveOutcome};

mod gates;
mod quiet;

use gates::{GateOutcome, GateSkip, evaluate_gates, hours_to_ms};

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
// 选题提供者
// =========================================================

/// 选题提供者：为指定人格产出候选指令（None = 本轮无题可提）。
///
/// 说明:
/// - `state` 为调度已加载的运行时状态（可变）：供选题器读取去重冷却记录、
///   写入判据 yes/no 计数；
/// - `activity_weight` 为时机权重（0.0~1.0：用户活跃与人格节律的合成软加权）：
///   供判据输入信号；样本不足未建模时取权重下限；
/// - 判据节流由调度层在调用前判定：本方法被调用即视为一次判据尝试（节流窗口消费）。
#[async_trait]
pub(crate) trait TopicPicker: Send + Sync {
    /// 产出本轮候选指令。
    ///
    /// 参数:
    /// - `engine`: 服务层引擎（供选题读取事件 / 规则等素材）。
    /// - `persona`: 目标人格 uid。
    /// - `now`: 本轮时间（Unix 毫秒）。
    /// - `state`: 调度已加载的运行时状态（可变；选题器写入判据计数）。
    /// - `activity_weight`: 时机权重（0.0~1.0；用户活跃与人格节律的合成）。
    ///
    /// 返回:
    /// - `Some(directive)`: 候选指令；
    /// - `None`: 本轮无题可提。
    async fn pick(
        &self,
        engine: &Engine,
        persona: &str,
        now: i64,
        state: &mut ProactiveState,
        activity_weight: f64,
    ) -> Option<ProactiveDirective>;
}

// =========================================================
// 单轮结果形态
// =========================================================

/// 单轮调度摘要（计数口径）。
///
/// 字段约定:
/// - `personas`: 本轮扫描的人格数；
/// - `attempts`: 进入选题（含判据尝试）的人格次数；
/// - `generated`: 生成成功条数；
/// - `delivered`: 实际投递条数。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProactiveTickSummary {
    pub personas: usize,
    pub attempts: usize,
    pub generated: usize,
    pub delivered: usize,
}

/// 投递结果（驱动差异化记账）。
enum DeliveryOutcome {
    /// 投递成功（本地或通道）。
    Delivered,
    /// 本地路径失败（未注册 / 本地 sink 报错）——维持既有记账口径。
    LocalFailed,
    /// 通道消息被门禁丢弃（总开关 / 绑定未启用 / 未允许主动 / 通道 sink 未装配 / 缺外部标识）。
    Dropped,
    /// 通道平台投递失败（sink 报错 / 绑定查询失败）。
    PlatformFailed,
}

/// 单人格处理结果。
enum PersonaTick {
    /// 硬闸门或活跃时段门未通过。
    Skipped(GateSkip),
    /// 判据尝试完成但无候选选题。
    NoTopic,
    /// 生成成功（`delivered` = 投递是否成功；`billable` = 是否计入配额）。
    Generated { delivered: bool, billable: bool },
    /// 生成层门禁静默跳过（不落库不投递）。
    GenerationSkipped,
    /// 生成失败（已进入选题，不记投递状态）。
    GenerationFailed,
}

// =========================================================
// 单轮调度
// =========================================================

/// 执行单轮主动调度检查。
///
/// 流程:
/// 1. 总开关判定（关闭直接返回空摘要，选题等一律不触达）；
/// 2. 列出全部人格（失败上抛，循环层下轮重试）；
/// 3. 全局日上限启用时加载全局状态并按本地日期跨日重置（失败上抛，下轮重试）；
/// 4. 逐人格：加载状态 → 判定链（闸门 / 活跃时段 / 判据节流 / 选题 / 生成 / 投放）
///    → 无条件回写状态；单人格失败记 warn 不阻塞其余；
/// 5. 全局状态无条件回写（跨日重置与生成计数均落盘）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `now`: 本轮时间（Unix 毫秒）。
/// - `picker`: 选题提供者。
///
/// 返回:
/// - 本轮各计数摘要；列出人格或读取全局状态失败时上抛。
pub(crate) async fn run_tick(
    engine: &Engine,
    now: i64,
    picker: &dyn TopicPicker,
) -> RamariaResult<ProactiveTickSummary> {
    let config = engine.config();
    if !config.proactive.enabled {
        debug!("主动调度：总开关关闭，本轮跳过");
        return Ok(ProactiveTickSummary::default());
    }

    let storage = engine.storage_ref().as_ref();
    let personas = storage.list_personas().await?;
    let mut summary = ProactiveTickSummary::default();

    // 全局日上限：不限时不读不写全局状态
    let mut global = if config.proactive.daily_total_limit > 0 {
        let mut g = state::load_global_state(storage).await?;
        let today = state::local_date_str(now);
        if g.daily_date != today {
            g.daily_count = 0;
            g.daily_date = today;
        }
        Some(g)
    } else {
        None
    };

    for persona in &personas {
        summary.personas += 1;
        let mut st = match state::load_state(storage, &persona.uid).await {
            Ok(st) => st,
            Err(e) => {
                warn!(persona = %persona.uid, error = %e, "主动调度：状态读取失败，跳过该人格");
                continue;
            }
        };

        let result = process_persona(
            engine,
            persona,
            now,
            picker,
            &config.proactive,
            global.as_ref(),
            &mut st,
        )
        .await;

        // 状态统一回写：跳过路径的跨日重置 / 宽限基准 / 退避归零等变更同样落盘
        if let Err(e) = state::save_state(storage, &persona.uid, &st).await {
            warn!(persona = %persona.uid, error = %e, "主动调度：状态保存失败");
        }

        match result {
            Ok(PersonaTick::Skipped(reason)) => {
                debug!(
                    persona = %persona.uid,
                    reason = reason.as_str(),
                    "主动调度：闸门未通过，跳过"
                );
            }
            Ok(PersonaTick::NoTopic) => {
                summary.attempts += 1;
                debug!(persona = %persona.uid, "主动调度：本轮无候选选题");
            }
            Ok(PersonaTick::Generated {
                delivered,
                billable,
            }) => {
                summary.attempts += 1;
                summary.generated += 1;
                if delivered {
                    summary.delivered += 1;
                }
                // 全局计数与人格级同口径：仅计费结果（本地失败仍计；丢弃 / 平台失败不计）
                if billable {
                    if let Some(g) = global.as_mut() {
                        g.daily_count = g.daily_count.saturating_add(1);
                    }
                }
            }
            Ok(PersonaTick::GenerationSkipped) => {
                summary.attempts += 1;
                debug!(persona = %persona.uid, "主动调度：生成层门禁跳过");
            }
            Ok(PersonaTick::GenerationFailed) => summary.attempts += 1,
            Err(e) => {
                warn!(
                    persona = %persona.uid,
                    error = %e,
                    "主动调度：单人格处理失败，跳过该人格"
                );
            }
        }
    }

    // 全局状态无条件回写：跨日重置与生成计数均落盘
    if let Some(g) = &global {
        if let Err(e) = state::save_global_state(storage, g).await {
            warn!(error = %e, "主动调度：全局状态保存失败");
        }
    }

    Ok(summary)
}

/// 处理单个人格的一轮调度。
///
/// 流程:
/// 1. 硬闸门（未通过返回跳过原因）；
/// 2. 活跃时段门（样本不足退化放行；低活跃度本轮不启动）；
/// 3. 时机权重合成（人格节律为软信号：样本不足中性，不构成硬否决）；
/// 4. 判据节流（仅判据开启时生效）；
/// 5. 选题（进入选题即视为一次判据尝试，先记账后调用）；
/// 6. 生成（`Ok(None)` = 生成层门禁静默跳过；`Err` 吸收为生成失败，不记投递状态）；
/// 7. 投放与记账（本地 / 通道分流；本地路径生成成功即记投递时间、当日计数与
///    候选效价符号，投递成功才记选题冷却；通道门禁丢弃 / 平台失败不记账）。
async fn process_persona(
    engine: &Engine,
    persona: &Persona,
    now: i64,
    picker: &dyn TopicPicker,
    config: &ProactiveConfig,
    global: Option<&ProactiveGlobalState>,
    st: &mut ProactiveState,
) -> RamariaResult<PersonaTick> {
    // ---- 1. 硬闸门 ----
    match evaluate_gates(engine, persona, now, config, global, st).await? {
        GateOutcome::Skip(reason) => return Ok(PersonaTick::Skipped(reason)),
        GateOutcome::Pass => {}
    }

    // ---- 2. 活跃时段门：软加权，样本不足退化放行 ----
    let hour = state::local_hour(now);
    let model = activity::load_model(
        engine.storage_ref().as_ref(),
        st,
        &persona.uid,
        now,
        config.active_hours_window_days,
        config.active_hours_min_samples,
    )
    .await?;
    let activity_weight =
        match activity::evaluate_gate(model.as_ref(), hour, config.active_hours_weight) {
            ActivityGate::LowWeight { norm } => {
                debug!(
                    persona = %persona.uid,
                    norm,
                    "主动调度跳过：当前时段活跃度过低"
                );
                return Ok(PersonaTick::Skipped(GateSkip::ActivityLow));
            }
            ActivityGate::Pass { weight } => {
                debug!(persona = %persona.uid, hour, weight, "主动调度：活跃时段门通过");
                weight
            }
            ActivityGate::NotModeled => {
                debug!(persona = %persona.uid, "主动调度：活跃时段样本不足，门放行");
                activity::weight_floor(config.active_hours_weight)
            }
        };

    // ---- 2.5 人格节律：与用户活跃合成时机权重（软信号，不构成硬否决）----
    let rhythm_model = activity::load_persona_model(
        engine.storage_ref().as_ref(),
        st,
        &persona.uid,
        now,
        config.rhythm_window_days,
        config.rhythm_min_samples,
    )
    .await?;
    let rhythm_norm = rhythm_model.map(|model| activity::hour_norm(&model.histogram, hour));
    let combined_weight =
        activity::combine_weight(activity_weight, rhythm_norm, config.rhythm_weight);
    debug!(
        persona = %persona.uid,
        rhythm_norm = ?rhythm_norm,
        combined_weight,
        "主动调度：时机权重合成"
    );

    // ---- 3. 判据节流（仅判据开启时生效）----
    if config.judge_enabled {
        if let Some(last) = st.last_judge_at {
            if now.saturating_sub(last) < hours_to_ms(config.judge_interval_hours) {
                return Ok(PersonaTick::Skipped(GateSkip::JudgeThrottled));
            }
        }
    }

    // ---- 4. 选题：进入选题即视为一次判据尝试，先记账后调用 ----
    st.last_judge_at = Some(now);
    let Some(directive) = picker
        .pick(engine, &persona.uid, now, st, combined_weight)
        .await
    else {
        return Ok(PersonaTick::NoTopic);
    };
    let source = directive.source.clone();
    let topic_key = directive.topic_key.clone();

    // ---- 5. 生成与投放 ----
    match engine.chat_proactive(directive).await {
        Ok(Some(outcome)) => {
            let delivery = deliver(engine, &outcome, now).await;
            record_delivery(
                st,
                now,
                &source,
                topic_key.as_deref(),
                &delivery,
                outcome.valence,
            );
            let delivered = matches!(delivery, DeliveryOutcome::Delivered);
            let billable = matches!(
                delivery,
                DeliveryOutcome::Delivered | DeliveryOutcome::LocalFailed
            );
            info!(
                persona = %persona.uid,
                source = %source,
                channel = %outcome.channel,
                delivered,
                billable,
                session_id = %outcome.session_id,
                "主动调度：已生成主动消息"
            );
            Ok(PersonaTick::Generated {
                delivered,
                billable,
            })
        }
        Ok(None) => {
            debug!(persona = %persona.uid, "主动调度：生成层门禁静默跳过");
            Ok(PersonaTick::GenerationSkipped)
        }
        // 生成失败（LLM / 存储）：不记投递状态，允许下一窗口重试（节流已记账）
        Err(e) => {
            warn!(persona = %persona.uid, error = %e, "主动调度：生成失败，本轮不记投递状态");
            Ok(PersonaTick::GenerationFailed)
        }
    }
}

/// 投放主动消息（路由 + 门禁 + 投递，异步调用接收端）。
///
/// 路由:
/// - `local` / `mcp` 等非社交通道 → 本地接收端：未注册 / 报错维持既有记账口径
///   （消息已落库应用内可见，生成即计配额）；
/// - `qq` / `telegram` → 通道接收端，投递前逐层门禁（通道总开关 / 外部标识 /
///   绑定存在且启用且允许主动 / 通道接收端已装配），任一不过即丢弃；
/// - 通道平台投递失败（sink 报错 / 绑定查询失败）不记账：消息未送达不消耗额度，
///   允许下一窗口重试。
///
/// 返回:
/// - 四态 [`DeliveryOutcome`]，驱动差异化记账。
async fn deliver(engine: &Engine, outcome: &ProactiveOutcome, now: i64) -> DeliveryOutcome {
    let is_channel = outcome.channel == CHANNEL_QQ || outcome.channel == CHANNEL_TELEGRAM;
    let message = ProactiveMessage {
        message_id: outcome.message_id,
        content: outcome.content.clone(),
        session_id: outcome.session_id,
        persona: outcome.persona.clone(),
        source: outcome.source.clone(),
        created_at: now,
        channel: outcome.channel.clone(),
        external_ref: outcome.external_ref.clone(),
    };

    if !is_channel {
        // 本地路径：维持既有口径（未注册静默丢弃；报错不记选题冷却）
        return match engine.proactive_sink() {
            None => {
                debug!(
                    persona = %outcome.persona,
                    "主动消息投递：接收端未注册，静默丢弃"
                );
                DeliveryOutcome::LocalFailed
            }
            Some(sink) => match sink.deliver(&message).await {
                Ok(()) => DeliveryOutcome::Delivered,
                Err(e) => {
                    warn!(
                        persona = %outcome.persona,
                        error = %e,
                        "主动消息投递失败"
                    );
                    DeliveryOutcome::LocalFailed
                }
            },
        };
    }

    // 通道路径：逐层门禁（任一不过 → 丢弃，零记账）
    if !engine.config().channel.proactive_enabled {
        debug!(channel = %outcome.channel, "主动消息通道投递：通道总开关关闭，丢弃");
        return DeliveryOutcome::Dropped;
    }
    let Some(external_ref) = message
        .external_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        debug!(channel = %outcome.channel, "主动消息通道投递：缺外部会话标识，丢弃");
        return DeliveryOutcome::Dropped;
    };
    match engine
        .channel_find_binding(&outcome.channel, &external_ref)
        .await
    {
        Ok(Some(binding)) => {
            if !binding.enabled || !binding.allow_proactive {
                debug!(
                    channel = %outcome.channel,
                    enabled = binding.enabled,
                    allow_proactive = binding.allow_proactive,
                    "主动消息通道投递：绑定未启用或未允许主动，丢弃"
                );
                return DeliveryOutcome::Dropped;
            }
        }
        Ok(None) => {
            debug!(channel = %outcome.channel, "主动消息通道投递：绑定不存在，丢弃");
            return DeliveryOutcome::Dropped;
        }
        Err(e) => {
            warn!(
                channel = %outcome.channel,
                error = %e,
                "主动消息通道投递：绑定查询失败（不记账）"
            );
            return DeliveryOutcome::PlatformFailed;
        }
    }
    let Some(sink) = engine.channel_proactive_sink() else {
        debug!(channel = %outcome.channel, "主动消息通道投递：通道接收端未装配，丢弃");
        return DeliveryOutcome::Dropped;
    };
    match sink.deliver(&message).await {
        Ok(()) => DeliveryOutcome::Delivered,
        Err(e) => {
            warn!(
                channel = %outcome.channel,
                error = %e,
                "主动消息通道投递失败（不记账）"
            );
            DeliveryOutcome::PlatformFailed
        }
    }
}

/// 效价符号：正（大于阈值）→ `1`；负（小于负阈值）→ `-1`；否则 `0`（中性或未知）。
fn valence_sign(valence: f64) -> i8 {
    if valence > VALENCE_SIGN_EPSILON {
        1
    } else if valence < -VALENCE_SIGN_EPSILON {
        -1
    } else {
        0
    }
}

/// 记录一次生成成功的投放记账。
///
/// 口径:
/// - `Delivered` / `LocalFailed`：记 `last_sent_at` / 当日计数 / 连续未回应次数 /
///   候选效价符号——消息已落库应用内可见（本地失败亦如此），避免下一窗口对同一
///   人格重复生成；退避计数以此累计；
/// - `Dropped` / `PlatformFailed`：全部不记（消息未送达不消耗额度，允许下一窗口重试）；
/// - 近期选题仅在投递成功且选题键非空时记录（投递失败不计冷却，允许下窗重试）。
fn record_delivery(
    st: &mut ProactiveState,
    now: i64,
    source: &str,
    topic_key: Option<&str>,
    delivery: &DeliveryOutcome,
    valence: f64,
) {
    // 丢弃 / 平台失败：不记任何状态（仅调用方日志留痕）
    if !matches!(
        delivery,
        DeliveryOutcome::Delivered | DeliveryOutcome::LocalFailed
    ) {
        debug!(source, "主动调度：投递未送达或被丢弃，本轮不记账");
        return;
    }

    st.last_sent_at = Some(now);
    st.daily_count = st.daily_count.saturating_add(1);
    st.daily_date = state::local_date_str(now);
    st.silence_streak = st.silence_streak.saturating_add(1);
    st.last_valence_sign = valence_sign(valence);

    if matches!(delivery, DeliveryOutcome::Delivered) {
        if let Some(key) = topic_key.map(str::trim).filter(|key| !key.is_empty()) {
            st.recent_topics.push(RecentTopic {
                source: source.to_string(),
                key: key.to_string(),
                sent_at: now,
            });
            if st.recent_topics.len() > RECENT_TOPICS_CAP {
                let overflow = st.recent_topics.len() - RECENT_TOPICS_CAP;
                st.recent_topics.drain(..overflow);
            }
        }
    }
}

// =========================================================
// 后台调度循环
// =========================================================

/// 启动主动对话后台调度任务。
///
/// 逻辑:
/// - 首轮延迟 `first_delay_seconds` 秒执行（避开宿主启动阶段）；
/// - 之后每 `interval_seconds` 秒执行一轮；单轮失败记 error 且不终止
///   （下一轮重试）；
/// - 停止位置位后退出；首轮延迟与轮次等待均按 60 秒分片感知停止位。
///
/// 参数:
/// - `engine`: 服务层引擎（`Arc` 共享）。
/// - `shutdown`: 宿主停止位（true = 循环应在下一轮退出）。
/// - `first_delay_seconds`: 首轮检查前的延迟秒数。
/// - `interval_seconds`: 两轮检查之间的间隔秒数。
/// - `picker`: 选题提供者（宿主装配）。
pub(crate) fn spawn(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
    first_delay_seconds: u64,
    interval_seconds: u64,
    picker: Arc<dyn TopicPicker>,
) -> tokio::task::JoinHandle<()> {
    info!(
        first_delay_seconds,
        interval_seconds, "主动对话调度任务启动"
    );

    tokio::spawn(async move {
        // 首次延迟：避免在宿主启动阶段触发；等待期间收到停止信号直接退出
        if !interruptible_sleep(first_delay_seconds, &shutdown).await {
            info!("主动对话调度任务在首轮延迟期间收到停止信号，退出");
            return;
        }

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("主动对话调度任务收到停止信号，退出");
                return;
            }

            match run_tick(&engine, now_ms(), picker.as_ref()).await {
                Ok(summary) => {
                    if summary.attempts > 0 || summary.generated > 0 {
                        info!(
                            personas = summary.personas,
                            attempts = summary.attempts,
                            generated = summary.generated,
                            delivered = summary.delivered,
                            "主动对话调度：本轮完成"
                        );
                    } else {
                        debug!(personas = summary.personas, "主动对话调度：本轮无候选");
                    }
                }
                Err(e) => {
                    error!(error = %e, "主动对话调度：本轮失败（下轮重试）");
                }
            }

            // 等待下一轮；0 间隔热转由装配层夹取兜底
            interruptible_sleep(interval_seconds, &shutdown).await;
        }
    })
}

/// 可中断等待：按 60 秒分片睡眠，每片感知停止位。
///
/// 返回:
/// - `true`: 睡满指定秒数；`false`: 等待期间收到停止信号（调用方应立即退出）。
async fn interruptible_sleep(seconds: u64, shutdown: &AtomicBool) -> bool {
    let mut remaining = seconds;
    while remaining > 0 {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        let chunk = remaining.min(SHUTDOWN_POLL_CHUNK_SECONDS);
        tokio::time::sleep(Duration::from_secs(chunk)).await;
        remaining = remaining.saturating_sub(chunk);
    }
    !shutdown.load(Ordering::Relaxed)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
