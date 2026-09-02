//! crates/ramaria-memory/src/behavior/feedback.rs - 弱反馈校准纯计算（H2，D-V17-008）
//!
//! 设计特点:
//! - S2 纠正信号 → 候选复审触发判定（不自动覆盖规则，仅产生复审标记）
//! - S3 继续信号 → 滑动窗口趋势统计（连续 ≥N 次继续后 M 次不继续 → 标记复审）
//! - 全部为纯计算（零 I/O、零 LLM），供 app 层编排落库与测试
//! - 只处理结构化字段（规则 id / 信号类型），不记录任何对话原文
//!
//! 边界:
//! - 本模块只产出"复审判定结果"；落库与反馈日志写入由 app 层执行。
//! - `auto_apply_weak_feedback` 关闭时，app 层只写 feedback_log，
//!   本模块的复审标记不落库（零自动修改，回归红线 5）。

use ramaria_core::behavior::SignalType;
use ramaria_core::config::FeedbackConfig;

// =========================================================
// 候选复审标记
// =========================================================

/// 候选复审触发原因（弱反馈校准的派生状态）。
///
/// 字段约定:
/// - `S2Correction`: 用户纠正消息（S2）指向该规则 → 建议人工复审该规则。
/// - `S3TrendStop`: S3 趋势异常——连续继续后转沉默 → 建议人工复审该规则
///   （可能回复不再吸引用户继续）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReason {
    /// S2 纠正信号触发候选复审
    S2Correction,
    /// S3 趋势异常触发候选复审
    S3TrendStop,
}

impl ReviewReason {
    /// 返回稳定的字符串标识（供日志与展示）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::S2Correction => "s2_correction",
            Self::S3TrendStop => "s3_trend_stop",
        }
    }
}

impl std::fmt::Display for ReviewReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条候选复审标记（规则 → 复审原因）。
///
/// 职责:
/// - 表示某条行为规则因弱反馈被标记为"候选复审"（不自动覆盖）。
/// - app 层将其持久化到 settings（JSON 队列），供前端/审计展示。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewCandidate {
    /// 行为规则 id
    pub rule_id: i64,
    /// 复审原因
    pub reason: ReviewReason,
    /// 触发时间（Unix 毫秒）
    pub created_at_ms: i64,
    /// 触发时所属人格
    pub persona_uid: String,
}

impl ReviewCandidate {
    /// 创建一条候选复审标记。
    pub fn new(rule_id: i64, reason: ReviewReason, persona_uid: impl Into<String>) -> Self {
        Self {
            rule_id,
            reason,
            created_at_ms: ramaria_core::types::now_ms(),
            persona_uid: persona_uid.into(),
        }
    }
}

// =========================================================
// S2 候选复审触发判定
// =========================================================

/// S2 纠正信号是否触发候选复审。
///
/// 规则:
/// - 纠正信号本身即提示该规则可能错误 → 返回 `Some(S2Correction)`（候选复审）。
/// - 不自动覆盖规则（`auto_apply` 关闭时 app 层只记录 feedback_log）。
///
/// 参数:
/// - `rule_id`: 被纠正指向的行为规则 id。
/// - `persona_uid`: 所属人格。
///
/// 返回:
/// - `Some(review_candidate)`: 触发候选复审（供 app 层按开关落库）。
/// - `None`: 无目标规则（无法定位规则时不触发）。
pub fn s2_correction_review_candidate(
    rule_id: i64,
    persona_uid: impl Into<String>,
) -> Option<ReviewCandidate> {
    if rule_id <= 0 {
        return None;
    }
    Some(ReviewCandidate::new(
        rule_id,
        ReviewReason::S2Correction,
        persona_uid,
    ))
}

// =========================================================
// S3 滑动窗口趋势统计
// =========================================================

/// 单回合的 S3 继续结果。
///
/// 语义:
/// - `Continue`: 用户在上一条助手回复后 60s 内继续发言（非纠正）——回复吸引继续。
/// - `NotContinue`: 用户未在窗口内继续发言（间隔超时 / 会话结束）——回复未促成继续。
///
/// 序列化: 供趋势历史持久化（settings JSON 数组，不含原文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// 用户继续发言（S3 命中）
    Continue,
    /// 用户未继续发言
    NotContinue,
}

/// 从 S3 弱信号是否命中映射为回合结果。
///
/// 说明:
/// - 检测到 S3 继续信号（非纠正、窗口内）→ `Continue`。
/// - 未检测到继续（超时/纠正）→ `NotContinue`（纠正另由 S2 路径处理）。
pub fn turn_outcome_from_signal(signal: Option<SignalType>) -> TurnOutcome {
    match signal {
        Some(SignalType::Continue) => TurnOutcome::Continue,
        _ => TurnOutcome::NotContinue,
    }
}

/// S3 趋势复审判定（滑动窗口）。
///
/// 规则（D-V17-008）:
/// - 在最近 `window` 个回合结果中，若存在"连续 ≥ `continue_trigger` 次 Continue
///   后紧跟连续 ≥ `stop_trigger` 次 NotContinue"的模式 → 标记复审
///   （回复曾持续吸引用户，后转为沉默，可能回复质量下降）。
///
/// 参数:
/// - `outcomes`: 回合结果序列（时间正序，最近在末尾；通常取滑动窗口）。
/// - `continue_trigger`: 连续继续触发数（默认 5）。
/// - `stop_trigger`: 随后连续不继续数（默认 4）。
///
/// 返回:
/// - `true`: 检测到 S3 趋势异常（建议候选复审）。
/// - `false`: 无该模式。
pub fn detect_s3_review_trend(
    outcomes: &[TurnOutcome],
    continue_trigger: usize,
    stop_trigger: usize,
) -> bool {
    if continue_trigger == 0 || stop_trigger == 0 {
        return false;
    }
    // 只取窗口尾部（保留最近 window 个），旧结果不参与判定
    let window = outcomes.len();
    let mut i = 0usize;
    while i < window {
        // 统计连续 Continue 数
        if outcomes[i] == TurnOutcome::Continue {
            let mut cont = 0usize;
            while i < window && outcomes[i] == TurnOutcome::Continue {
                cont += 1;
                i += 1;
            }
            if cont >= continue_trigger {
                // 紧接着统计连续 NotContinue 数
                let mut stop = 0usize;
                while i < window && outcomes[i] == TurnOutcome::NotContinue {
                    stop += 1;
                    i += 1;
                }
                if stop >= stop_trigger {
                    return true;
                }
            }
        } else {
            i += 1;
        }
    }
    false
}

/// 把 S3 趋势结果映射为候选复审标记（供 app 层按开关落库）。
///
/// 返回:
/// - `Some(ReviewCandidate)` 当检测到趋势异常且 rule_id 有效。
/// - `None` 否则。
pub fn s3_trend_review_candidate(
    outcomes: &[TurnOutcome],
    config: &FeedbackConfig,
    rule_id: i64,
    persona_uid: impl Into<String>,
) -> Option<ReviewCandidate> {
    if rule_id <= 0 {
        return None;
    }
    // 只取最近滑动窗口大小
    let w = config.s3_trend_window as usize;
    let start = outcomes.len().saturating_sub(w);
    let window_outcomes = &outcomes[start..];
    if detect_s3_review_trend(
        window_outcomes,
        config.s3_continue_trigger as usize,
        config.s3_stop_trigger as usize,
    ) {
        Some(ReviewCandidate::new(
            rule_id,
            ReviewReason::S3TrendStop,
            persona_uid,
        ))
    } else {
        None
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_reason_serde_and_display() {
        assert_eq!(
            serde_json::to_string(&ReviewReason::S2Correction).unwrap(),
            r#""s2_correction""#
        );
        assert_eq!(
            serde_json::to_string(&ReviewReason::S3TrendStop).unwrap(),
            r#""s3_trend_stop""#
        );
        assert_eq!(ReviewReason::S2Correction.as_str(), "s2_correction");
        assert_eq!(ReviewReason::S3TrendStop.to_string(), "s3_trend_stop");
    }

    #[test]
    fn review_candidate_roundtrip_json() {
        let c = ReviewCandidate::new(7, ReviewReason::S2Correction, "char-0001");
        assert_eq!(c.rule_id, 7);
        assert_eq!(c.reason, ReviewReason::S2Correction);
        assert_eq!(c.persona_uid, "char-0001");
        assert!(c.created_at_ms > 0);
        let json = serde_json::to_string(&c).unwrap();
        let back: ReviewCandidate = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    // ---- S2 候选复审 ----

    #[test]
    fn s2_correction_triggers_candidate_review() {
        let c = s2_correction_review_candidate(5, "char-0001");
        assert!(c.is_some());
        assert_eq!(c.unwrap().reason, ReviewReason::S2Correction);
    }

    #[test]
    fn s2_correction_no_candidate_without_rule() {
        // 无有效规则 id（0 / 负）→ 不触发候选复审
        assert!(s2_correction_review_candidate(0, "char-0001").is_none());
        assert!(s2_correction_review_candidate(-3, "char-0001").is_none());
    }

    // ---- TurnOutcome 映射 ----

    #[test]
    fn turn_outcome_maps_continue_signal() {
        assert_eq!(
            turn_outcome_from_signal(Some(SignalType::Continue)),
            TurnOutcome::Continue
        );
        // 无信号 / 纠正 / 强信号 → NotContinue（纠正另由 S2 处理）
        assert_eq!(turn_outcome_from_signal(None), TurnOutcome::NotContinue);
        assert_eq!(
            turn_outcome_from_signal(Some(SignalType::Correction)),
            TurnOutcome::NotContinue
        );
        assert_eq!(
            turn_outcome_from_signal(Some(SignalType::Edit)),
            TurnOutcome::NotContinue
        );
    }

    // ---- S3 趋势判定 ----

    #[test]
    fn s3_trend_detects_continue_then_stop() {
        // 连续 5 次继续后 4 次不继续 → 趋势异常
        let outcomes: Vec<TurnOutcome> = vec![
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
        ];
        assert!(detect_s3_review_trend(&outcomes, 5, 4));
    }

    #[test]
    fn s3_trend_no_detection_without_enough_stop() {
        // 连续 5 次继续后仅 2 次不继续 → 不满足 stop_trigger=4
        let outcomes: Vec<TurnOutcome> = vec![
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
        ];
        assert!(!detect_s3_review_trend(&outcomes, 5, 4));
    }

    #[test]
    fn s3_trend_no_detection_without_enough_continue() {
        // 连续 3 次继续后 4 次不继续 → 不满足 continue_trigger=5
        let outcomes: Vec<TurnOutcome> = vec![
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
        ];
        assert!(!detect_s3_review_trend(&outcomes, 5, 4));
    }

    #[test]
    fn s3_trend_window_respected() {
        // 趋势发生在窗口外（更早），当前窗口尾部无该模式 → 不触发
        // 旧结果被窗口截断；只取最近 window 个。
        let mut outcomes: Vec<TurnOutcome> = Vec::new();
        // 前 10 个构造 continue→stop 模式（在窗口外）
        outcomes.extend(std::iter::repeat_n(TurnOutcome::Continue, 5));
        outcomes.extend(std::iter::repeat_n(TurnOutcome::NotContinue, 4));
        // 后 8 个为最近结果（全是 Continue，无 stop 尾）
        outcomes.extend(std::iter::repeat_n(TurnOutcome::Continue, 8));
        // window=10：最近 10 个全是 Continue → 无 continue→stop 模式
        let w = outcomes.len().min(10);
        let tail = &outcomes[outcomes.len() - w..];
        assert!(!detect_s3_review_trend(tail, 5, 4));
    }

    #[test]
    fn s3_trend_zero_trigger_returns_false() {
        let outcomes = vec![TurnOutcome::Continue];
        assert!(!detect_s3_review_trend(&outcomes, 0, 4));
        assert!(!detect_s3_review_trend(&outcomes, 5, 0));
    }

    #[test]
    fn s3_trend_candidate_when_detected() {
        let outcomes: Vec<TurnOutcome> = vec![
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
        ];
        let cfg = FeedbackConfig::default();
        let c = s3_trend_review_candidate(&outcomes, &cfg, 3, "char-0001");
        assert!(c.is_some());
        assert_eq!(c.unwrap().reason, ReviewReason::S3TrendStop);
    }

    #[test]
    fn s3_trend_no_candidate_when_not_detected() {
        let outcomes: Vec<TurnOutcome> = std::iter::repeat_n(TurnOutcome::Continue, 8).collect();
        let cfg = FeedbackConfig::default();
        assert!(s3_trend_review_candidate(&outcomes, &cfg, 3, "char-0001").is_none());
    }

    #[test]
    fn s3_trend_no_candidate_without_rule() {
        let outcomes: Vec<TurnOutcome> = vec![
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::Continue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
            TurnOutcome::NotContinue,
        ];
        let cfg = FeedbackConfig::default();
        assert!(s3_trend_review_candidate(&outcomes, &cfg, 0, "char-0001").is_none());
    }
}
