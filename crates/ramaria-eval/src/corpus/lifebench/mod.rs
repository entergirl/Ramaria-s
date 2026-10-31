//! crates/ramaria-eval/src/corpus/lifebench/mod.rs - Ramaria LifeBench 语料适配器
//!
//! 设计特点:
//! - 会话、消息与 QA 转成带语义的强类型结构，裸 JSON 不出本模块
//! - 消息角色由 speaker 与 sample 的 speaker_a / speaker_b 映射，不依赖文本
//! - artifact 标识为 dia_id 去掉日期前缀，供检索层按单元计分
//! - 中英对齐只看 question_id 与 gold 字段，人名与正文不参与
//! - 解析只读本地文件，不联网、不缓存

mod align;
mod parse;

use chrono::NaiveDate;
use ramaria_service::ChatRole;

pub use align::{AlignmentReport, align};
pub use parse::{load, load_language};

/// 一个 LifeBench sample（一个用户的一整年会话与全部 QA）。
///
/// 字段约定:
/// - `sample_id`: 数据集内的样本身份，中英两版为不同译名，不能作对齐键；
/// - `user_key`: 从 `question_id` 提取的用户标识；
/// - `speaker_a` / `speaker_b`: 会话双方原始称呼；
/// - `sessions`: 按序号升序，日期无缺口；
/// - `qa`: 保持文件内顺序，`question_id` 唯一。
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub sample_id: String,
    pub user_key: String,
    pub speaker_a: String,
    pub speaker_b: String,
    pub sessions: Vec<Session>,
    pub qa: Vec<Qa>,
}

impl Sample {
    /// 检索与注入使用的 persona 标识。
    pub fn persona_uid(&self) -> String {
        format!("lifebench-{}", self.user_key)
    }
}

/// 一天之内的会话。
///
/// 字段约定:
/// - `index`: session 序号，从 1 连续；
/// - `date`: 会话日期，同时是消息 dia_id 的日期前缀；
/// - `messages`: 按文件内顺序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub index: u32,
    pub date: NaiveDate,
    pub messages: Vec<Message>,
}

/// 一条会话消息。
///
/// 字段约定:
/// - `role`: 由 speaker 映射，speaker_a 为 User、speaker_b 为 Assistant；
/// - `speaker`: 原始称呼，保留以便回溯；
/// - `dia_id`: `YYYY-MM-DD_artifact`，同一 artifact 可含多条消息；
/// - `text`: 非空正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: ChatRole,
    pub speaker: String,
    pub dia_id: String,
    pub text: String,
}

impl Message {
    /// 去掉日期前缀后的 artifact 标识（检索层计分单元）。
    ///
    /// 返回:
    /// - `2025-01-02_chat1` → `chat1`；无前缀时原样返回。
    pub fn artifact_id(&self) -> String {
        match self.dia_id.split_once('_') {
            Some((_, artifact)) => artifact.to_string(),
            None => self.dia_id.clone(),
        }
    }
}

/// 一条 QA。
///
/// 字段约定:
/// - `question_id`: `lifebench_v5_{user}_qa{n}`，中英同序同值，作为对齐键；
/// - `ask_time`: 提问日，无时分秒，查询时钟取该日结束时刻；
/// - `evidence`: artifact 标识集合，不可回答题为空；
/// - `category`: 类别码数组，取值 0–8，4 表示不可回答；
/// - `score_points`: 分项判分要点，分值合计 10，存在 0.5 步长。
#[derive(Debug, Clone, PartialEq)]
pub struct Qa {
    pub question_id: String,
    pub ask_time: NaiveDate,
    pub question: String,
    pub answer: String,
    pub evidence: Vec<String>,
    pub category: Vec<u8>,
    pub question_type: Vec<String>,
    pub score_points: Vec<ScorePoint>,
}

/// 一个分项判分要点。
///
/// 字段约定:
/// - `score`: 该要点的分值，可为 0.5 步长的小数；
/// - 同题全部分值合计为 10。
#[derive(Debug, Clone, PartialEq)]
pub struct ScorePoint {
    pub description: String,
    pub score: f64,
}
