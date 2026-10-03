//! crates/ramaria-service/src/proactive/judge.rs - Ramaria 主动对话 AI 判据
//!
//! 设计特点:
//! - 两段式决策的第二段：算法候选与安全网之后、生成之前的一次轻量 LLM 调用
//! - 只读形态：输入为信号简报与候选摘要（不含消息原文），裁决由调用方消费
//! - 空候选不发起调用；结果三态（开口 / 静默 / 失败）供调用方按记账口径消费
//! - 三步递进解析（直接 JSON → 剥离 think → 提取首个 JSON 对象），复用记忆模块工具
//! - 输出受控：候选编号必须回引输入，angle / tone 截断，reason_bucket 白名单归一
//! - 隐私：日志只记 speak / reason_bucket 与响应长度，不记候选摘要与原始响应

use ramaria_core::traits::ChatRequest;
use ramaria_memory::{extract_first_json_object, strip_thinking};
use tracing::{info, warn};
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// 常量
// =========================================================

/// 判据输入的候选上限（超出截断；控制 prompt 长度）。
const JUDGE_MAX_CANDIDATES: usize = 5;
/// angle / tone 的字符截断上限（防超长输出拖累生成注入）。
const JUDGE_MAX_FIELD_CHARS: usize = 20;
/// 效价标注阈值：`|valence|` 超过该值标注正向 / 负向，否则中性。
const JUDGE_VALENCE_EPSILON: f64 = 0.1;
/// 判据调用温度（低温度：稳定、克制的裁决）。
const JUDGE_TEMPERATURE: f64 = 0.2;
/// 判据输出 token 上限（一次轻量调用）。
const JUDGE_MAX_TOKENS: u32 = 256;
/// 原因类别白名单（归一后入日志；未命中落 `unknown`）。
const JUDGE_REASON_BUCKETS: [&str; 5] = [
    "speak",
    "no_value",
    "too_recent",
    "user_busy",
    "avoid_interrupt",
];
/// 判据系统提示词（姿态、规则与输出 schema 的单一来源）。
const JUDGE_SYSTEM_PROMPT: &str = r#"你是陪伴型 AI 的主动对话判据。你只做一件事：在给出的候选话题中，判断此刻是否适合主动开口，并选择要提起的话题。

姿态：愿意开口，但克制。没有值得提起的话题时，安静比硬找话题更好。

规则：
- 若适合开口：speak = true，从候选里选一个 candidate_id，给出简短的角度 angle 与语气 tone（各不超过 10 个字）。
- 若不适合开口：speak = false，candidate_id / angle / tone 留空字符串。
- reason_bucket：开口时用 "speak"；沉默时从 "no_value"（没有值得提起的内容）、"too_recent"（刚聊过）、"user_busy"（推测用户在忙）、"avoid_interrupt"（不宜打扰）中选一个。

只输出一个 JSON 对象，不要任何其它文字：
{"speak": true, "candidate_id": "c0", "angle": "关心近况", "tone": "温和", "reason_bucket": "speak"}"#;

// =========================================================
// 输入与输出形态
// =========================================================

/// 判据输入信号（简报；不含任何消息原文）。
///
/// 字段约定:
/// - `hour`: 本地小时（0~23）。
/// - `activity_weight`: 当前时段软加权权重（0.0~1.0）。
/// - `hours_since_last_chat`: 距上次对话小时数（None = 无历史）。
/// - `silence_streak`: 连续未回应主动消息次数。
/// - `daily_count`: 当日已主动投递条数。
pub(crate) struct JudgeSignals {
    pub hour: u32,
    pub activity_weight: f64,
    pub hours_since_last_chat: Option<f64>,
    pub silence_streak: u32,
    pub daily_count: u32,
}

/// 判据候选（只含摘要不含原文；id 由调用方编号）。
///
/// 字段约定:
/// - `id`: 候选编号（如 `c0`），判据输出回引。
/// - `source`: 来源标识（`event` / `unresolved` / `time_node` / `rule` / `light_touch`）。
/// - `anchor`: 情境摘要（None = 轻触达无具体话题）。
/// - `valence`: 候选效价（-1.0~1.0；符号决定正 / 负向标注）。
pub(crate) struct JudgeCandidate {
    pub id: String,
    pub source: String,
    pub anchor: Option<String>,
    pub valence: f64,
}

/// 判据裁决（`speak=true` 且候选有效时产出）。
///
/// 字段约定:
/// - `candidate_id`: 回引输入的候选编号（已确认在候选中）。
/// - `angle`: 开口角度（trim 后空 → None；按字符截断）。
/// - `tone`: 说话语气（trim 后空 → None；按字符截断）。
/// - `reason_bucket`: 白名单归一后的原因类别（未知 → `unknown`）。
pub(crate) struct JudgeDecision {
    pub candidate_id: String,
    pub angle: Option<String>,
    pub tone: Option<String>,
    /// 原因类别（调参观测字段；当前由判据日志与测试消费）。
    #[allow(dead_code)]
    pub reason_bucket: &'static str,
}

/// 判据结果三态（判据统计口径：失败不计 no，避免污染调参观测）。
pub(crate) enum JudgeOutcome {
    /// 裁决开口且候选有效。
    Speak(JudgeDecision),
    /// 可靠沉默（speak=false）——计 no。
    Silent,
    /// 调用 / 解析失败或编号无效——不计 yes/no。
    Failed,
}

/// 判据输出形态（宽松解析：缺字段回退默认，多余字段忽略）。
#[derive(serde::Deserialize)]
struct JudgeOutput {
    #[serde(default)]
    speak: bool,
    #[serde(default)]
    candidate_id: String,
    #[serde(default)]
    angle: Option<String>,
    #[serde(default)]
    tone: Option<String>,
    #[serde(default)]
    reason_bucket: Option<String>,
}

// =========================================================
// 主入口
// =========================================================

/// 执行一次 AI 判据调用（一次轻量 LLM 调用）。
///
/// 流程:
/// 1. 候选空 → 直接 Failed（不调用 LLM）；
/// 2. 组装信号简报 + 候选列表（只含摘要；来源与效价转为中文标注）；
/// 3. 调用 LLM（失败记 warn 后 Failed——系统级不可用时自然沉默）；
/// 4. 三步递进解析（直接 JSON → 剥离 think → 提取首个 JSON 对象）；
/// 5. 校验：speak=false → Silent；candidate_id 不在候选内 → 记 warn 后 Failed；
///    reason_bucket 白名单归一（未知 → `unknown`）；angle/tone trim + 截断；
/// 6. 成功记 info（speak 与 reason_bucket）。
///
/// 参数:
/// - `engine`: 服务层引擎（取 LLM provider 快照）。
/// - `candidates`: 候选列表（超出上限截断；空列表不发起调用）。
/// - `signals`: 情境信号简报。
///
/// 返回:
/// - `JudgeOutcome::Speak(decision)`: 判据裁决「开口」且候选编号有效；
/// - `JudgeOutcome::Silent`: 判据可靠沉默；
/// - `JudgeOutcome::Failed`: 候选为空 / 调用或解析失败 / 编号无效。
pub(crate) async fn decide(
    engine: &Engine,
    candidates: &[JudgeCandidate],
    signals: &JudgeSignals,
) -> JudgeOutcome {
    if candidates.is_empty() {
        return JudgeOutcome::Failed;
    }
    let candidates = &candidates[..candidates.len().min(JUDGE_MAX_CANDIDATES)];
    let user_message = build_user_message(candidates, signals);
    let request = ChatRequest {
        system_prompt: JUDGE_SYSTEM_PROMPT.to_string(),
        memory_context: None,
        history: vec![],
        user_message,
        temperature: JUDGE_TEMPERATURE,
        max_tokens: JUDGE_MAX_TOKENS,
        request_id: Uuid::new_v4(),
        template_version: ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
    };

    let raw = match engine.llm_ref().chat(&request).await {
        Ok(raw) => raw,
        Err(e) => {
            warn!(error = %e, "主动判据：LLM 调用失败，本轮静默");
            return JudgeOutcome::Failed;
        }
    };

    let Some(parsed) = parse_output(&raw) else {
        // 隐私红线：原始响应不落日志，仅记长度供诊断
        warn!(
            response_len = raw.chars().count(),
            "主动判据：输出解析失败，本轮静默（原始响应不记录）"
        );
        return JudgeOutcome::Failed;
    };

    let reason_bucket = normalize_reason_bucket(parsed.reason_bucket.as_deref());
    if !parsed.speak {
        info!(speak = false, reason_bucket, "主动判据：裁决完成");
        return JudgeOutcome::Silent;
    }

    let candidate_id = parsed.candidate_id.trim();
    if !candidates
        .iter()
        .any(|candidate| candidate.id == candidate_id)
    {
        warn!("主动判据：裁决候选编号不在候选中，本轮静默");
        return JudgeOutcome::Failed;
    }

    info!(speak = true, reason_bucket, "主动判据：裁决完成");
    JudgeOutcome::Speak(JudgeDecision {
        candidate_id: candidate_id.to_string(),
        angle: normalize_field(parsed.angle.as_deref()),
        tone: normalize_field(parsed.tone.as_deref()),
        reason_bucket,
    })
}

// =========================================================
// 私有辅助
// =========================================================

/// 组装判据用户消息：情境简报 + 候选列表（只含摘要与标注，不含原文）。
fn build_user_message(candidates: &[JudgeCandidate], signals: &JudgeSignals) -> String {
    let mut message = String::new();
    message.push_str("## 当前情境\n");
    message.push_str(&format!("- 本地时间：{} 点\n", signals.hour));
    message.push_str(&format!(
        "- 用户活跃权重：{:.2}（越高表示用户通常在这个时段活跃）\n",
        signals.activity_weight
    ));
    match signals.hours_since_last_chat {
        Some(hours) => message.push_str(&format!("- 距上次对话：{hours:.1} 小时\n")),
        None => message.push_str("- 距上次对话：无历史\n"),
    }
    message.push_str(&format!(
        "- 连续未回应主动消息：{} 次\n",
        signals.silence_streak
    ));
    message.push_str(&format!("- 今日已主动投递：{} 次\n", signals.daily_count));

    message.push_str("\n## 候选话题\n");
    for candidate in candidates {
        let anchor = candidate
            .anchor
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("（无具体话题，可作轻量问候）");
        message.push_str(&format!(
            "{} [{}][{}] {}\n",
            candidate.id,
            source_label(&candidate.source),
            valence_label(candidate.valence),
            anchor
        ));
    }
    message
}

/// 来源中文标注（未知来源原样输出，防御性回退）。
fn source_label(source: &str) -> &str {
    match source {
        "event" => "高显著事件",
        "unresolved" => "未了结事件",
        "time_node" => "时间节点",
        "rule" => "行为规则",
        "light_touch" => "轻触达",
        _ => source,
    }
}

/// 效价标注：正向 / 负向 / 中性（阈值 `JUDGE_VALENCE_EPSILON`）。
fn valence_label(valence: f64) -> &'static str {
    if valence > JUDGE_VALENCE_EPSILON {
        "正向"
    } else if valence < -JUDGE_VALENCE_EPSILON {
        "负向"
    } else {
        "中性"
    }
}

/// 三步递进解析：直接 JSON → 剥离 think 标签 → 提取首个 JSON 对象。
fn parse_output(raw: &str) -> Option<JudgeOutput> {
    // 步骤 1: 直接解析
    if let Ok(parsed) = serde_json::from_str::<JudgeOutput>(raw) {
        return Some(parsed);
    }

    // 步骤 2: 剥离 think 标签后重试
    let stripped = strip_thinking(raw);
    if stripped != raw {
        if let Ok(parsed) = serde_json::from_str::<JudgeOutput>(&stripped) {
            return Some(parsed);
        }
    }

    // 步骤 3: 提取首个 JSON 对象后解析
    if let Some(segment) = extract_first_json_object(raw) {
        if let Ok(parsed) = serde_json::from_str::<JudgeOutput>(&segment) {
            return Some(parsed);
        }
    }

    None
}

/// 原因类别白名单归一（trim 后未命中白名单 → `unknown`）。
fn normalize_reason_bucket(raw: Option<&str>) -> &'static str {
    let Some(bucket) = raw.map(str::trim) else {
        return "unknown";
    };
    JUDGE_REASON_BUCKETS
        .iter()
        .copied()
        .find(|allowed| *allowed == bucket)
        .unwrap_or("unknown")
}

/// angle / tone 归一：trim 后空串视为未提供；超长按字符边界截断。
fn normalize_field(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).filter(|text| !text.is_empty())?;
    Some(trimmed.chars().take(JUDGE_MAX_FIELD_CHARS).collect())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
