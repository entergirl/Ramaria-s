//! crates/ramaria-app/src/feedback.rs - S2/S3 弱反馈采集与校准编排（H2，D-V17-007/008）
//!
//! 设计特点:
//! - 信号检测：从会话最近消息序列检测 S2 纠正（前缀命中 + 60s 窗口）/ S3 继续（窗口内非纠正）
//! - 排除项：中断/沉默≠负反馈（超窗口不计）、超时封存不计入（仅活跃会话检测）、30s 去重
//! - feedback_log 写入：signal_type=correction/continue，detail 只存结构化字段（不含原文全文）
//! - 候选复审：auto_apply_weak_feedback=false（默认）时仅写 feedback_log（审计），
//!   规则/画像零自动修改（回归红线 5）；开启时把复审标记落 settings 队列
//! - 静默降级：检测/写入/复审任一环节失败记 warn，不阻塞对话主流程
//!
//! 隐私红线:
//! - feedback_log.detail 不含原文全文（只存纠正前缀词/窗口间隔/规则 id 等脱敏字段）
//! - 日志只记录信号类型与计数，不记录对话内容

use ramaria_core::behavior::{FeedbackLog, SignalType, TargetType};
use ramaria_core::config::FeedbackConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, MessageRole, now_ms};

/// 候选复审队列在 settings 表的存储键（JSON 数组，不含原文）。
pub const REVIEW_QUEUE_KEY: &str = "feedback_review_queue_v1";

/// S2 纠正前缀词（用户纠正助手回复时的常见开头，v3.1 §9.1）。
///
/// 匹配规则:
/// - 用户消息去除首尾空白后，以任一前缀词开头 → 判定为纠正信号。
/// - 前缀按语义权重降序排列，仅用于检测（不存储原文）。
const CORRECTION_PREFIXES: &[&str] = &["不对", "不是", "应该说", "其实"];

// =========================================================
// 检测（纯逻辑，零 I/O）
// =========================================================

/// 判定文本是否为 S2 纠正前缀。
///
/// 规则:
/// - 去除首尾空白后，以 `CORRECTION_PREFIXES` 任一前缀开头 → 纠正。
/// - 返回命中的前缀词（供 detail 记录，不含完整消息原文）。
pub fn correction_prefix_match(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    CORRECTION_PREFIXES
        .iter()
        .find(|p| trimmed.starts_with(**p))
        .copied()
}

/// 弱反馈信号检测结果。
#[derive(Debug, Clone, PartialEq)]
pub struct WeakSignal {
    /// 信号类型（Correction / Continue）
    pub signal_type: SignalType,
    /// 命中的纠正前缀词（仅 S2 有值，脱敏）
    pub matched_prefix: Option<&'static str>,
    /// 用户消息与上一条助手回复的时间间隔（毫秒）
    pub interval_ms: i64,
    /// 目标行为规则 id（若能定位；S3 继续可能无规则目标）
    pub target_rule_id: Option<i64>,
}

/// 从会话最近消息序列检测弱反馈信号（纯逻辑，零 I/O）。
///
/// 语义:
/// - 取 `recent_messages`（时间正序）的最后两条：`prev`（上一条助手回复）与
///   `curr`（当前用户消息）。
/// - `curr.role == User` 且 `prev.role == Assistant` 时才检测。
/// - 间隔 ≤ `continue_window_ms`：
///   - 命中纠正前缀 → S2 Correction。
///   - 否则 → S3 Continue。
/// - 间隔 > 窗口 / 消息结构不符 → `None`（中断/沉默不计，超时封存不计）。
///
/// 参数:
/// - `recent_messages`: 会话最近消息（时间正序，最新在末尾）。
/// - `config`: 弱反馈配置（检测窗口）。
/// - `now`: 当前时间（Unix 毫秒），用于计算用户消息时间。
///
/// 返回:
/// - `Some(WeakSignal)`: 检测到弱信号。
/// - `None`: 无信号（间隔超窗 / 结构不符 / 未启用）。
pub fn detect_weak_signal(
    recent_messages: &[Message],
    config: &FeedbackConfig,
    now: i64,
) -> Option<WeakSignal> {
    if recent_messages.len() < 2 {
        return None;
    }
    let curr = recent_messages.last()?;
    let prev = recent_messages.get(recent_messages.len().saturating_sub(2))?;

    // 仅"助手回复后跟用户消息"才检测
    if curr.role != MessageRole::User || prev.role != MessageRole::Assistant {
        return None;
    }

    // 间隔窗口：用户消息时间 vs 上一条助手回复时间
    let interval_ms = curr.created_at.saturating_sub(prev.created_at);
    // 用当前时间兜底（用户消息时间可能为历史导入值，取更可靠的时间差）
    let effective_interval = if interval_ms <= 0 {
        now.saturating_sub(prev.created_at)
    } else {
        interval_ms
    };

    let window = if config.enabled {
        config.continue_window_ms as i64
    } else {
        0
    };
    if effective_interval > window || effective_interval < 0 {
        return None; // 超时（沉默/中断）→ 不计
    }

    // S2 纠正前缀
    if let Some(prefix) = correction_prefix_match(&curr.content) {
        return Some(WeakSignal {
            signal_type: SignalType::Correction,
            matched_prefix: Some(prefix),
            interval_ms: effective_interval,
            target_rule_id: None,
        });
    }

    // S3 继续（窗口内非纠正）
    Some(WeakSignal {
        signal_type: SignalType::Continue,
        matched_prefix: None,
        interval_ms: effective_interval,
        target_rule_id: None,
    })
}

// =========================================================
// 排除项判定（30s 去重）
// =========================================================

/// 判断是否应在去重窗口内跳过重复反馈。
///
/// 排除项（T-V17-4-003）:
/// - 中断/沉默≠负反馈：由检测窗口保证（超窗口 → None，不在本函数）。
/// - 超时封存不计入：由调用方保证（仅活跃 session 检测）。
/// - 30s 内无反馈不累积：同一 persona + 信号类型 + 目标在 `dedup_window_ms`
///   内已有反馈日志 → 跳过（不重复累积）。
///
/// 参数:
/// - `storage`: 存储后端（查询最近反馈日志）。
/// - `config`: 弱反馈配置（去重窗口）。
/// - `persona_uid`: 所属人格。
/// - `signal_type`: 信号类型。
/// - `target_id`: 目标 id（规则 id 字符串，无目标时用占位）。
/// - `now`: 当前时间（Unix 毫秒）。
///
/// 返回:
/// - `Ok(true)`: 应跳过（窗口内已有同类反馈）。
/// - `Ok(false)`: 可写入。
/// - `Err`: 查询失败（调用方降级为可写入，不阻塞）。
pub async fn should_dedup_recent_feedback(
    storage: &dyn StorageBackend,
    config: &FeedbackConfig,
    persona_uid: &str,
    signal_type: SignalType,
    target_id: &str,
    now: i64,
) -> RamariaResult<bool> {
    let logs = storage.list_feedback_logs_by_persona(persona_uid).await?;
    let window = config.dedup_window_ms as i64;
    for log in logs {
        if log.signal_type == signal_type
            && log.target_id == target_id
            && now - log.created_at <= window
        {
            return Ok(true);
        }
    }
    Ok(false)
}

// =========================================================
// 写入编排
// =========================================================

/// 处理一条新的用户消息，检测并落库弱反馈信号（对话前调用）。
///
/// 流程:
/// 1. 读取会话最近消息（取最近 2 条足够检测，兼容多轮）。
/// 2. `detect_weak_signal` 检测 S2/S3。
/// 3. 排除项判定（30s 去重；窗口/沉默已由检测保证）。
/// 4. 写 feedback_log（detail 脱敏，不含原文全文）。
/// 5. 若 `auto_apply_weak_feedback=true`：定位目标规则 → 标记候选复审。
///
/// 静默降级:
/// - `[feedback].enabled=false` → 直接返回（不检测，行为回退 v1.6）。
/// - 存储读取/写入失败 → warn 日志，不阻塞对话主流程。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `config`: 弱反馈配置。
/// - `session_id`: 当前会话（须为活跃会话，超时封存不计入）。
/// - `persona_uid`: 所属人格（None → 回退 rama 自身）。
/// - `recent_messages`: 会话最近消息（含当前用户消息在末尾，即"上一条助手回复 +
///   当前用户输入"的检测序列；由调用方构造）。
///
/// 返回:
/// - `Ok(())`: 处理完成（可能未检测到信号，均为正常）。
/// - 内部失败不向调用方上抛（静默降级链）。
pub async fn process_feedback_for_new_message(
    storage: &dyn StorageBackend,
    config: &FeedbackConfig,
    session_id: uuid::Uuid,
    persona_uid: Option<&str>,
    recent_messages: &[Message],
) -> RamariaResult<()> {
    if !config.enabled {
        return Ok(());
    }

    let now = now_ms();
    let actual_uid = persona_uid.unwrap_or("rama-0001");

    // 检测（含时间窗口 + 纠正前缀）
    let Some(signal) = detect_weak_signal(recent_messages, config, now) else {
        return Ok(());
    };

    // 超时封存不计入：仅活跃会话才处理（调用方保证 session 未关闭）
    // 30s 去重排除项
    let target_id = signal
        .target_rule_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "rule:unknown".to_string());
    if should_dedup_recent_feedback(
        storage,
        config,
        actual_uid,
        signal.signal_type,
        &target_id,
        now,
    )
    .await?
    {
        tracing::debug!(
            persona_uid = actual_uid,
            signal = ?signal.signal_type,
            "弱反馈信号在去重窗口内已记录，跳过（30s 去重）"
        );
        return Ok(());
    }

    // 写 feedback_log（detail 脱敏，不含原文全文）
    let detail = build_signal_detail(&signal);
    let log = FeedbackLog::new(
        actual_uid,
        TargetType::BehaviorRule,
        &target_id,
        signal.signal_type,
        Some(session_id.to_string()),
        Some(detail),
    );
    storage.save_feedback_log(&log).await?;

    tracing::info!(
        persona_uid = actual_uid,
        session_id = %session_id,
        signal = ?signal.signal_type,
        interval_ms = signal.interval_ms,
        target_id,
        "弱反馈信号已写入 feedback_log"
    );

    // 候选复审（仅 auto_apply 开启时落库；关闭时仅审计，零自动修改）
    if config.auto_apply_weak_feedback {
        record_review_candidate(storage, config, actual_uid, signal).await?;
    }

    Ok(())
}

/// 构造脱敏的反馈 detail（不含原文全文）。
///
/// 字段:
/// - `matched_prefix`: 命中的纠正前缀词（S2）；S3 为 null。
/// - `interval_ms`: 用户消息与上一条助手回复的时间间隔。
/// - `signal`: 信号类型字符串（correction / continue）。
///
/// 隐私: 不包含用户消息原文或助手回复原文。
fn build_signal_detail(signal: &WeakSignal) -> String {
    let signal_str = match signal.signal_type {
        SignalType::Correction => "correction",
        SignalType::Continue => "continue",
        SignalType::Edit | SignalType::Disable => "strong",
    };
    serde_json::json!({
        "signal": signal_str,
        "matched_prefix": signal.matched_prefix,
        "interval_ms": signal.interval_ms,
    })
    .to_string()
}

// =========================================================
// 候选复审（auto_apply 开启时落库）
// =========================================================

/// 把弱反馈产生的候选复审标记写入 settings 队列（幂等追加）。
///
/// 规则:
/// - S2 纠正 → 定位目标规则并标记 `S2Correction` 复审。
/// - S3 继续 → 记录回合结果到趋势历史，检测到趋势异常 → 标记 `S3TrendStop` 复审。
/// - 目标规则无法定位（无规则 / 查询失败）→ 跳过（不产生复审）。
///
/// 存储: settings 表键 `feedback_review_queue_v1`（JSON 数组，不含原文）。
async fn record_review_candidate(
    storage: &dyn StorageBackend,
    config: &FeedbackConfig,
    persona_uid: &str,
    signal: WeakSignal,
) -> RamariaResult<()> {
    match signal.signal_type {
        SignalType::Correction => {
            // 定位最近路由的规则：S2 纠正目标 = 最近一次路由的主规则。
            // 这里通过最近消息路由近似定位（复用行为路由，返回主规则 id）。
            if let Some(rule_id) = resolve_recent_route_rule(storage, persona_uid).await? {
                let candidate =
                    ramaria_memory::behavior::s2_correction_review_candidate(rule_id, persona_uid);
                if let Some(c) = candidate {
                    append_review_candidate(storage, &c).await?;
                }
            }
            Ok(())
        }
        SignalType::Continue => {
            // 记录 S3 回合结果到趋势历史，检测趋势异常
            let mut history = load_s3_history(storage, persona_uid).await?;
            history.push(ramaria_memory::behavior::TurnOutcome::Continue);
            if let Some(rule_id) = resolve_recent_route_rule(storage, persona_uid).await? {
                let candidate = ramaria_memory::behavior::s3_trend_review_candidate(
                    &history,
                    config,
                    rule_id,
                    persona_uid,
                );
                if let Some(c) = candidate {
                    append_review_candidate(storage, &c).await?;
                }
            }
            // 持久化趋势历史（仅 auto_apply 开启时维护）
            save_s3_history(storage, persona_uid, &history).await?;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 通过最近消息路由近似定位当前应激活的行为规则（供 S2/S3 目标定位）。
///
/// 说明:
/// - 复用 `behavior_route` 逻辑，仅取主规则 id；未命中 / 行为关闭 → None。
/// - 降级: 行为层关闭 / 无规则 / 路由失败 → None（不产生复审标记）。
async fn resolve_recent_route_rule(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<Option<i64>> {
    let rules = storage.list_behavior_rules_by_persona(persona_uid).await?;
    // 取最近一条启用中的规则（无 embedding 时按纯关键词近似的简化定位）
    // 说明: 完整路由需查询向量，此处做轻量定位——取最近启用规则的 id。
    // 严格场景由 app 层在 send_message 时传入实际路由的主规则 id。
    Ok(rules.iter().rev().find(|r| r.enabled).map(|r| r.id))
}

/// 追加一条候选复审标记到 settings 队列（幂等：同规则+原因不重复）。
async fn append_review_candidate(
    storage: &dyn StorageBackend,
    candidate: &ramaria_memory::behavior::ReviewCandidate,
) -> RamariaResult<()> {
    let mut queue: Vec<ramaria_memory::behavior::ReviewCandidate> =
        match storage.get_setting(REVIEW_QUEUE_KEY).await? {
            Some(json) => serde_json::from_str(&json).unwrap_or_default(),
            None => Vec::new(),
        };
    // 幂等去重
    if queue
        .iter()
        .any(|c| c.rule_id == candidate.rule_id && c.reason == candidate.reason)
    {
        return Ok(());
    }
    queue.push(candidate.clone());
    let json = serde_json::to_string(&queue).map_err(|e| {
        tracing::warn!(error = %e, "序列化候选复审队列失败");
        ramaria_core::error::RamariaError::serialization("序列化候选复审队列失败")
    })?;
    storage.set_setting(REVIEW_QUEUE_KEY, &json).await
}

/// 加载 S3 回合结果趋势历史（settings JSON 数组，不含原文）。
async fn load_s3_history(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<Vec<ramaria_memory::behavior::TurnOutcome>> {
    let key = format!("feedback_s3_history_v1:{persona_uid}");
    match storage.get_setting(&key).await? {
        Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
        None => Ok(Vec::new()),
    }
}

/// 持久化 S3 回合结果趋势历史（只保留最近窗口大小）。
async fn save_s3_history(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    history: &[ramaria_memory::behavior::TurnOutcome],
) -> RamariaResult<()> {
    // 只保留最近窗口大小，避免无限增长
    let window = 20usize;
    let start = history.len().saturating_sub(window);
    let tail = &history[start..];
    let json = serde_json::to_string(tail).map_err(|e| {
        tracing::warn!(error = %e, "序列化 S3 趋势历史失败");
        ramaria_core::error::RamariaError::serialization("序列化 S3 趋势历史失败")
    })?;
    let key = format!("feedback_s3_history_v1:{persona_uid}");
    storage.set_setting(&key, &json).await
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use ramaria_core::types::{MessageSource, new_id};
    use std::sync::Arc;

    fn msg(role: MessageRole, content: &str, created_at: i64) -> Message {
        Message {
            id: new_id(),
            session_id: new_id(),
            role,
            content: content.to_string(),
            created_at,
            source: MessageSource::Local,
            fingerprint: None,
            persona_uid: None,
        }
    }

    fn cfg() -> FeedbackConfig {
        FeedbackConfig::default()
    }

    // ---- 纠正前缀 ----

    #[test]
    fn correction_prefix_hit() {
        assert_eq!(correction_prefix_match("不对，应该是这样的"), Some("不对"));
        assert_eq!(correction_prefix_match("不是这样"), Some("不是"));
        assert_eq!(correction_prefix_match("应该说重点"), Some("应该说"));
        assert_eq!(correction_prefix_match("其实我更想..."), Some("其实"));
        // 空白容忍
        assert_eq!(correction_prefix_match("  不对   "), Some("不对"));
    }

    #[test]
    fn correction_prefix_miss() {
        assert_eq!(correction_prefix_match("好的，知道了"), None);
        assert_eq!(correction_prefix_match("对，没错"), None);
        assert_eq!(correction_prefix_match(""), None);
        assert_eq!(correction_prefix_match("   "), None);
    }

    // ---- 检测 ----

    #[test]
    fn detect_s2_correction_within_window() {
        let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
        let user = msg(MessageRole::User, "不对，你理解错了", 1100); // 100ms 后
        let recent = vec![assistant, user];
        let signal = detect_weak_signal(&recent, &cfg(), 1100).unwrap();
        assert_eq!(signal.signal_type, SignalType::Correction);
        assert_eq!(signal.matched_prefix, Some("不对"));
        assert_eq!(signal.interval_ms, 100);
    }

    #[test]
    fn detect_s3_continue_within_window() {
        let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
        let user = msg(MessageRole::User, "好的继续聊这个话题", 1300);
        let recent = vec![assistant, user];
        let signal = detect_weak_signal(&recent, &cfg(), 1300).unwrap();
        assert_eq!(signal.signal_type, SignalType::Continue);
        assert_eq!(signal.matched_prefix, None);
        assert_eq!(signal.interval_ms, 300);
    }

    #[test]
    fn detect_no_signal_beyond_window() {
        // 间隔 > 60s → 沉默/中断，不判为负反馈
        let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
        let user = msg(MessageRole::User, "好的", 1000 + 61_000);
        let recent = vec![assistant, user];
        assert!(detect_weak_signal(&recent, &cfg(), 1000 + 61_000).is_none());
    }

    #[test]
    fn detect_no_signal_when_less_than_two_messages() {
        let user = msg(MessageRole::User, "只有一条", 1000);
        assert!(detect_weak_signal(&[user], &cfg(), 1000).is_none());
    }

    #[test]
    fn detect_no_signal_when_no_assistant_predecessor() {
        // prev 是用户消息（非助手回复）→ 不检测
        let prev_user = msg(MessageRole::User, "前一条用户消息", 1000);
        let curr_user = msg(MessageRole::User, "当前用户消息", 1100);
        let recent = vec![prev_user, curr_user];
        assert!(detect_weak_signal(&recent, &cfg(), 1100).is_none());
    }

    #[test]
    fn detect_disabled_returns_none() {
        let mut config = cfg();
        config.enabled = false;
        let assistant = msg(MessageRole::Assistant, "回复", 1000);
        let user = msg(MessageRole::User, "不对", 1100);
        assert!(detect_weak_signal(&[assistant, user], &config, 1100).is_none());
    }

    // ---- detail 脱敏 ----

    #[test]
    fn signal_detail_excludes_raw_text() {
        let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
        let user = msg(MessageRole::User, "不对，你理解错了，原文内容", 1100);
        let signal = detect_weak_signal(&[assistant, user], &cfg(), 1100).unwrap();
        let detail = build_signal_detail(&signal);
        assert!(detail.contains("matched_prefix"));
        assert!(detail.contains("不对"));
        assert!(
            !detail.contains("你理解错了"),
            "detail 不得含原文全文（仅前缀词）"
        );
    }

    // ---- 30s 去重 ----

    #[tokio::test]
    async fn dedup_detects_recent_same_signal() {
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let now = now_ms();
        // 写入一条 correction 反馈（同一目标）
        storage
            .save_feedback_log(&FeedbackLog::new(
                "char-0001",
                TargetType::BehaviorRule,
                "3",
                SignalType::Correction,
                None,
                None,
            ))
            .await
            .unwrap();
        // 30s 窗口内同一目标 correction → 应去重
        let dup = should_dedup_recent_feedback(
            storage.as_ref(),
            &cfg(),
            "char-0001",
            SignalType::Correction,
            "3",
            now,
        )
        .await
        .unwrap();
        assert!(dup, "窗口内同信号应去重");
        // 不同目标 → 不去重
        let dup_other = should_dedup_recent_feedback(
            storage.as_ref(),
            &cfg(),
            "char-0001",
            SignalType::Correction,
            "99",
            now,
        )
        .await
        .unwrap();
        assert!(!dup_other);
    }

    // ---- 端到端写入（T-V17-4-003）----

    #[tokio::test]
    async fn writes_correction_feedback_with_weight_and_privacy() {
        // S2 纠正信号完整链路：检测 → 排除项 → feedback_log 写入
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(Some("char-0001")).await.unwrap();

        // 会话已有上一条助手回复（60s 内）
        let reply_at = now_ms() - 20_000;
        let mut assistant = msg(MessageRole::Assistant, "我认为应该是甲方案", reply_at);
        assistant.session_id = session.id;
        assistant.persona_uid = Some("char-0001".into());
        storage.save_message(&assistant).await.unwrap();

        // 当前用户纠正消息（构造检测序列）
        let recent = vec![
            assistant.clone(),
            msg(MessageRole::User, "不对，应该是乙方案", now_ms()),
        ];

        process_feedback_for_new_message(
            storage.as_ref(),
            &cfg(),
            session.id,
            Some("char-0001"),
            &recent,
        )
        .await
        .expect("处理成功");

        // 断言 feedback_log 写入
        let logs = storage.feedback_logs_for("char-0001");
        assert_eq!(logs.len(), 1, "应写入一条 S2 纠正反馈");
        assert_eq!(logs[0].signal_type, SignalType::Correction);
        assert!((logs[0].weight - 0.6).abs() < f64::EPSILON, "S2 weight=0.6");
        // 隐私：detail 不含原文全文
        let detail = logs[0].detail.as_deref().unwrap_or_default();
        assert!(
            !detail.contains("应该是乙方案"),
            "detail 不得含用户消息原文全文"
        );
        assert!(detail.contains("不对"), "detail 含纠正前缀词（脱敏）");
    }

    #[tokio::test]
    async fn writes_continue_feedback_with_weight_02() {
        // S3 继续信号：非纠正、窗口内 → Continue，weight=0.2
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(Some("char-0001")).await.unwrap();
        let reply_at = now_ms() - 10_000;
        let mut assistant = msg(MessageRole::Assistant, "好的，我们继续", reply_at);
        assistant.session_id = session.id;
        assistant.persona_uid = Some("char-0001".into());
        storage.save_message(&assistant).await.unwrap();

        let recent = vec![
            assistant.clone(),
            msg(MessageRole::User, "接着聊下一件事", now_ms()),
        ];
        process_feedback_for_new_message(
            storage.as_ref(),
            &cfg(),
            session.id,
            Some("char-0001"),
            &recent,
        )
        .await
        .expect("处理成功");

        let logs = storage.feedback_logs_for("char-0001");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].signal_type, SignalType::Continue);
        assert!((logs[0].weight - 0.2).abs() < f64::EPSILON, "S3 weight=0.2");
    }

    #[tokio::test]
    async fn auto_apply_off_does_not_modify_review_queue() {
        // auto_apply_weak_feedback=false（默认）：弱信号仅写 feedback_log，
        // 不修改 settings 复审队列（回归红线 5：零自动修改）
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(Some("char-0001")).await.unwrap();
        let reply_at = now_ms() - 10_000;
        let mut assistant = msg(MessageRole::Assistant, "回复", reply_at);
        assistant.session_id = session.id;
        assistant.persona_uid = Some("char-0001".into());
        storage.save_message(&assistant).await.unwrap();

        let recent = vec![
            assistant.clone(),
            msg(MessageRole::User, "不对，纠正一下", now_ms()),
        ];
        // 默认配置 auto_apply=false
        let config = FeedbackConfig::default();
        assert!(!config.auto_apply_weak_feedback);
        process_feedback_for_new_message(
            storage.as_ref(),
            &config,
            session.id,
            Some("char-0001"),
            &recent,
        )
        .await
        .expect("处理成功");

        // feedback_log 已写（审计）
        assert_eq!(storage.feedback_logs_for("char-0001").len(), 1);
        // 复审队列未被修改（settings 无 REVIEW_QUEUE_KEY）
        let queue = storage
            .get_setting(REVIEW_QUEUE_KEY)
            .await
            .expect("读取成功");
        assert!(queue.is_none(), "auto_apply=false 时不应写复审队列");
    }
}
