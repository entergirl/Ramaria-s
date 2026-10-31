//! crates/ramaria-eval/src/corpus/lifebench/parse.rs - Ramaria LifeBench 语料解析
//!
//! 设计特点:
//! - 原始 JSON 只反序列化一次，随后全部转为强类型结构
//! - session 键按序号排序并强制从 1 连续，不依赖文件内键顺序
//! - 消息、QA 与 evidence 逐条校验，失败返回带定位的 `Validation`
//! - artifact 到日期建映射，保证 evidence 不引用提问之后的会话
//! - 只读本地文件，不联网、不缓存

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::NaiveDate;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::ChatRole;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::CorpusLanguage;
use super::{Message, Qa, Sample, ScorePoint, Session};

/// 每条 QA 的分值总和。
const QA_SCORE_TOTAL: f64 = 10.0;
/// 分值合计的比较容差（分值以 0.5 为步长，容差只吸收浮点表示误差）。
const QA_SCORE_EPSILON: f64 = 1e-6;
/// 类别码上界（0–8 共九类）。
const CATEGORY_MAX: u8 = 8;
/// 不可回答类别码。
const CATEGORY_UNANSWERABLE: u8 = 4;
/// question_id 的固定前缀。
const QUESTION_PREFIX: &str = "lifebench_v5_";
/// 日期格式。
const DATE_FORMAT: &str = "%Y-%m-%d";

// =========================================================
// 原始结构
// =========================================================

#[derive(Debug, Deserialize)]
struct RawSample {
    sample_id: String,
    conversation: Map<String, Value>,
    qa: Vec<RawQa>,
}

#[derive(Debug, Deserialize)]
struct RawQa {
    question_id: String,
    ask_time: String,
    question: String,
    answer: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    category: Vec<u8>,
    #[serde(default)]
    question_type: Vec<String>,
    #[serde(default)]
    score_points: Vec<RawScorePoint>,
}

#[derive(Debug, Deserialize)]
struct RawScorePoint {
    description: String,
    score: f64,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    speaker: String,
    dia_id: String,
    text: String,
}

// =========================================================
// 入口
// =========================================================

/// 读取并校验一份语料文件。
///
/// 参数:
/// - `path`: 语料 JSON 路径。
///
/// 返回:
/// - 成功时按文件内 sample 顺序返回；
/// - 读取失败返回 `Io`，JSON 或语义非法返回 `Validation`。
pub fn load(path: &Path) -> RamariaResult<Vec<Sample>> {
    let bytes = std::fs::read(path).map_err(|error| {
        RamariaError::io(format!("读取语料失败: {}", path.display()), Some(error))
    })?;
    let samples: Vec<RawSample> = serde_json::from_slice(&bytes).map_err(|error| {
        RamariaError::validation(format!("解析语料失败: {}（{error}）", path.display()))
    })?;
    samples.into_iter().map(parse_sample).collect()
}

/// 按语言读取语料根目录下的对应文件。
///
/// 参数:
/// - `root`: 语料根目录；
/// - `language`: 目标语言。
pub fn load_language(root: &Path, language: CorpusLanguage) -> RamariaResult<Vec<Sample>> {
    load(&root.join(language.file_name()))
}

// =========================================================
// 单个 sample
// =========================================================

fn parse_sample(raw: RawSample) -> RamariaResult<Sample> {
    let RawSample {
        sample_id,
        conversation,
        qa,
    } = raw;
    let (speaker_a, speaker_b) = parse_speakers(&sample_id, &conversation)?;
    let sessions = parse_sessions(&sample_id, conversation, &speaker_a, &speaker_b)?;
    let artifacts = collect_artifacts(&sample_id, &sessions)?;

    let mut user_key: Option<String> = None;
    let mut seen: HashSet<String> = HashSet::new();
    let mut parsed = Vec::with_capacity(qa.len());
    for raw_qa in qa {
        let Some(user) = user_key_of(&raw_qa.question_id).map(str::to_string) else {
            return Err(invalid_qa(
                &sample_id,
                &raw_qa.question_id,
                "question_id 不符合 lifebench_v5_{user}_qa{n} 格式",
            ));
        };
        match &user_key {
            Some(existing) if *existing != user => {
                return Err(invalid_qa(
                    &sample_id,
                    &raw_qa.question_id,
                    format!("user_key 与同 sample 其他 QA 不一致（{existing} / {user}）"),
                ));
            }
            Some(_) => {}
            None => user_key = Some(user),
        }
        parsed.push(parse_qa(&sample_id, raw_qa, &artifacts, &mut seen)?);
    }

    let user_key = user_key.ok_or_else(|| invalid(&sample_id, "qa 为空，无法确定 user_key"))?;
    Ok(Sample {
        sample_id,
        user_key,
        speaker_a,
        speaker_b,
        sessions,
        qa: parsed,
    })
}

fn parse_speakers(
    sample_id: &str,
    conversation: &Map<String, Value>,
) -> RamariaResult<(String, String)> {
    let read = |key: &str| -> RamariaResult<String> {
        conversation
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid(sample_id, format!("conversation.{key} 缺失或为空")))
    };
    let speaker_a = read("speaker_a")?;
    let speaker_b = read("speaker_b")?;
    if speaker_a == speaker_b {
        return Err(invalid(sample_id, "speaker_a 与 speaker_b 相同"));
    }
    Ok((speaker_a, speaker_b))
}

fn parse_sessions(
    sample_id: &str,
    conversation: Map<String, Value>,
    speaker_a: &str,
    speaker_b: &str,
) -> RamariaResult<Vec<Session>> {
    let mut session_values: BTreeMap<u32, Value> = BTreeMap::new();
    let mut session_dates: BTreeMap<u32, String> = BTreeMap::new();

    for (key, value) in conversation {
        let Some(suffix) = key.strip_prefix("session_") else {
            continue;
        };
        let (index_text, field) = match suffix.split_once('_') {
            Some((index, field)) => (index, Some(field)),
            None => (suffix, None),
        };
        let index: u32 = index_text
            .parse()
            .map_err(|_| invalid(sample_id, format!("无法识别的 session 键: {key}")))?;
        match field {
            None => {
                session_values.insert(index, value);
            }
            Some("date_time") => {
                let text = value
                    .as_str()
                    .ok_or_else(|| invalid(sample_id, format!("{key} 必须是日期字符串")))?;
                session_dates.insert(index, text.to_string());
            }
            Some(other) => {
                return Err(invalid(
                    sample_id,
                    format!("无法识别的 session 键: {key}（后缀 {other}）"),
                ));
            }
        }
    }

    if session_values.is_empty() {
        return Err(invalid(sample_id, "conversation 未包含任何 session"));
    }
    let indexes: Vec<u32> = session_values.keys().copied().collect();
    let expected: Vec<u32> = (1..=session_values.len() as u32).collect();
    if indexes != expected {
        return Err(invalid(
            sample_id,
            format!("session 序号必须从 1 连续，实际 {indexes:?}"),
        ));
    }

    let mut sessions = Vec::with_capacity(session_values.len());
    for (index, value) in session_values {
        let date_text = session_dates.get(&index).ok_or_else(|| {
            invalid(
                sample_id,
                format!("session_{index} 缺少 session_{index}_date_time"),
            )
        })?;
        let date = NaiveDate::parse_from_str(date_text, DATE_FORMAT).map_err(|_| {
            invalid(
                sample_id,
                format!("session_{index} 的日期无法解析: {date_text}"),
            )
        })?;
        let raw_messages: Vec<RawMessage> = serde_json::from_value(value).map_err(|error| {
            invalid(
                sample_id,
                format!("session_{index} 的消息结构非法: {error}"),
            )
        })?;
        let mut messages = Vec::with_capacity(raw_messages.len());
        for raw in raw_messages {
            messages.push(convert_message(
                sample_id, index, date, speaker_a, speaker_b, raw,
            )?);
        }
        sessions.push(Session {
            index,
            date,
            messages,
        });
    }
    Ok(sessions)
}

fn convert_message(
    sample_id: &str,
    index: u32,
    date: NaiveDate,
    speaker_a: &str,
    speaker_b: &str,
    raw: RawMessage,
) -> RamariaResult<Message> {
    let Some((date_text, _)) = raw.dia_id.split_once('_') else {
        return Err(invalid(
            sample_id,
            format!("session_{index} 的 dia_id 缺少日期前缀: {}", raw.dia_id),
        ));
    };
    let message_date = NaiveDate::parse_from_str(date_text, DATE_FORMAT).map_err(|_| {
        invalid(
            sample_id,
            format!("session_{index} 的 dia_id 日期无法解析: {}", raw.dia_id),
        )
    })?;
    if message_date != date {
        return Err(invalid(
            sample_id,
            format!(
                "session_{index} 的 dia_id 日期与 session 日期不一致: {}",
                raw.dia_id
            ),
        ));
    }
    let role = if raw.speaker == speaker_a {
        ChatRole::User
    } else if raw.speaker == speaker_b {
        ChatRole::Assistant
    } else {
        return Err(invalid(
            sample_id,
            format!(
                "session_{index} 的 speaker 未在 speaker_a / speaker_b 中: {}",
                raw.speaker
            ),
        ));
    };
    if raw.text.trim().is_empty() {
        return Err(invalid(
            sample_id,
            format!("session_{index} 的消息正文为空: {}", raw.dia_id),
        ));
    }
    Ok(Message {
        role,
        speaker: raw.speaker,
        dia_id: raw.dia_id,
        text: raw.text,
    })
}

/// 建立 artifact → 会话日期的映射；跨日重名视为结构错误。
fn collect_artifacts(
    sample_id: &str,
    sessions: &[Session],
) -> RamariaResult<HashMap<String, NaiveDate>> {
    let mut artifacts: HashMap<String, NaiveDate> = HashMap::new();
    for session in sessions {
        for message in &session.messages {
            let artifact = message.artifact_id();
            match artifacts.get(&artifact) {
                Some(existing) if *existing != session.date => {
                    return Err(invalid(
                        sample_id,
                        format!(
                            "artifact 跨日重名: {artifact}（{} / {}）",
                            existing, session.date
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    artifacts.insert(artifact, session.date);
                }
            }
        }
    }
    Ok(artifacts)
}

// =========================================================
// QA
// =========================================================

fn parse_qa(
    sample_id: &str,
    raw: RawQa,
    artifacts: &HashMap<String, NaiveDate>,
    seen: &mut HashSet<String>,
) -> RamariaResult<Qa> {
    let question_id = raw.question_id;
    if !seen.insert(question_id.clone()) {
        return Err(invalid_qa(sample_id, &question_id, "question_id 重复"));
    }
    let ask_time = NaiveDate::parse_from_str(&raw.ask_time, DATE_FORMAT).map_err(|_| {
        invalid_qa(
            sample_id,
            &question_id,
            format!("ask_time 无法解析: {}", raw.ask_time),
        )
    })?;
    if let Some(code) = raw.category.iter().find(|code| **code > CATEGORY_MAX) {
        return Err(invalid_qa(
            sample_id,
            &question_id,
            format!("category 超出 0–{CATEGORY_MAX}: {code}"),
        ));
    }

    let total: f64 = raw.score_points.iter().map(|point| point.score).sum();
    if (total - QA_SCORE_TOTAL).abs() > QA_SCORE_EPSILON {
        return Err(invalid_qa(
            sample_id,
            &question_id,
            format!("score_points 合计应为 {QA_SCORE_TOTAL}，实际 {total}"),
        ));
    }

    let unanswerable = raw.category.contains(&CATEGORY_UNANSWERABLE);
    if unanswerable && !raw.evidence.is_empty() {
        return Err(invalid_qa(
            sample_id,
            &question_id,
            "不可回答题（category 含 4）的 evidence 必须为空",
        ));
    }
    if !unanswerable && raw.evidence.is_empty() {
        return Err(invalid_qa(
            sample_id,
            &question_id,
            "非不可回答题必须有 evidence",
        ));
    }
    for evidence in &raw.evidence {
        let Some(date) = artifacts.get(evidence) else {
            return Err(invalid_qa(
                sample_id,
                &question_id,
                format!("evidence 未命中任何 artifact: {evidence}"),
            ));
        };
        if *date > ask_time {
            return Err(invalid_qa(
                sample_id,
                &question_id,
                format!("evidence 会话日期 {date} 晚于提问日 {ask_time}: {evidence}"),
            ));
        }
    }

    Ok(Qa {
        question_id,
        ask_time,
        question: raw.question,
        answer: raw.answer,
        evidence: raw.evidence,
        category: raw.category,
        question_type: raw.question_type,
        score_points: raw
            .score_points
            .into_iter()
            .map(|point| ScorePoint {
                description: point.description,
                score: point.score,
            })
            .collect(),
    })
}

/// 从 question_id 提取 user_key（`lifebench_v5_{user}_qa{n}`）。
fn user_key_of(question_id: &str) -> Option<&str> {
    let rest = question_id.strip_prefix(QUESTION_PREFIX)?;
    let (user, sequence) = rest.rsplit_once("_qa")?;
    if user.is_empty() || sequence.is_empty() || !sequence.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(user)
}

// =========================================================
// 错误
// =========================================================

fn invalid(sample_id: &str, detail: impl std::fmt::Display) -> RamariaError {
    RamariaError::validation(format!("语料 sample {sample_id}: {detail}"))
}

fn invalid_qa(sample_id: &str, question_id: &str, detail: impl std::fmt::Display) -> RamariaError {
    RamariaError::validation(format!(
        "语料 sample {sample_id} 的 question {question_id}: {detail}"
    ))
}
