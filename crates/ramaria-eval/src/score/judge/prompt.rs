//! crates/ramaria-eval/src/score/judge/prompt.rs - Ramaria 评测判分提示词
//!
//! 设计特点:
//! - 提示词全部常量化：任务名写在 system 首行，模板摘要进产物，口径变化可追
//! - 输出契约写明 JSON 形状：解析只认严格 JSON，不做代码块剥离
//! - 用户侧拼装只放本题原料，不带条件轴与库信息，避免判分被实验条件干扰
//! - 重试只在用户消息末尾追加一句约束，system 保持不变，两次调用同模板
//! - 语气维模板与分项模板同一套：系统提示、用户消息与输出解析各一处

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::baseline::QrelsScorePoint;

use super::{JudgeTask, JudgeValue, PointVote};

/// 判分请求模板版本（写入请求，不参与缓存键；缓存键用模板摘要）。
pub const JUDGE_TEMPLATE_VERSION: &str = "eval-judge-1";

/// 解析失败后重试时追加在用户消息末尾的约束。
pub const RETRY_SUFFIX: &str = "只输出 JSON，不要解释";

/// 分项要点判分的系统提示。
pub const RUBRIC_SYSTEM: &str = "任务: rubric_points
你是严格的分项评分助手。逐条核对参考回复给出的要点是否被待评回复覆盖。
只输出一个 JSON 对象，格式: {\"points\":[{\"index\":0,\"hit\":true,\"reason\":\"...\"}]}
规则:
- 每条要点输出一项，index 与输入要点序号一致，不得增删或重复；
- hit 为 true 表示该要点已被待评回复明确覆盖，语义等价即可，不要求措辞相同；
- reason 一句话说明判定依据，不复制回复原文；
- 不输出 JSON 以外的任何文字。";

/// 不可回答判定的系统提示。
pub const UNANSWERABLE_SYSTEM: &str = "任务: unanswerable
你判断待评回复是否明确表示无法回答该问题。
只输出一个 JSON 对象，格式: {\"refused\":true,\"reason\":\"...\"}
规则:
- refused 为 true 表示回复明确表示不知道、无法回答、信息不足，或拒绝作答；
- 回复只要仍给出具体信息或推测，即使把握不大，也记 false；
- reason 一句话说明判定依据；
- 不输出 JSON 以外的任何文字。";

/// 冲突检测判定的系统提示。
pub const CONFLICT_SYSTEM: &str = "任务: conflict
你判断待评回复是否把问题里的错误前提当作事实接受。
输入给出问题、参考答案与待评回复；参考答案是事实依据，问题中的说法与参考答案冲突时即为错误前提。
只输出一个 JSON 对象，格式: {\"accepted_premise\":false,\"reason\":\"...\"}
规则:
- 先比对问题与参考答案：两者一致说明问题没有错误前提，直接记 false；
- 两者冲突时，回复若按问题里的错误说法作答（重复、确认或据此推理），记 true；
- 回复指出该说法有误、给出参考答案的事实、或拒绝按该前提回答，记 false；
- reason 一句话说明判定依据；
- 不输出 JSON 以外的任何文字。";

/// 语气维判分的系统提示（1~5 档 rubric + few-shot 锚定）。
///
/// 说明:
/// - 前半段是 1~5 档语义锚定与长度中性约束，后半段是四个短句社交示例；
/// - 只比语气风格，不比内容多少与长短：高情感语料的回复普遍很短，按长度判分会
///   把"压到真实短句长度"误判成语气变差；
/// - 输出只认一个整数分，解析侧对纯整数与 JSON 两种形态都接受。
pub const TONE_SYSTEM: &str = "\
请按 1~5 分评估「候选回复」在语气、风格上与「参考回复」的相似程度（只比语气风格，不比内容多少、不比长短）：
1 分：语气/风格完全不像参考（生硬、机器人口吻、书面助手腔）；
2 分：略有相似但有明显偏差；
3 分：基本相似，偶有偏差；
4 分：语气/风格较贴近参考，偏差少；
5 分：语气/风格高度贴近参考，几乎难辨。
重要：不要按回复长短判分。日常社交聊天里回复常常很短，参考回复很短时，
同样简短的候选回复完全可能是 5 分；写得冗长、像在解释或总结的回复反而应扣分。
只输出一个整数分数（1~5），不要输出任何其他文字。

【示例 1】（参考很短、候选同样简短且风格一致 → 高分）
参考回复：对啊对啊
候选回复：对对对
分数：5
【示例 2】（候选写成书面助手腔、比参考啰嗦很多 → 低分）
参考回复：我找一下
候选回复：好的，我这就去数据库里帮您查询相关记录，还请您稍等片刻。
分数：1
【示例 3】（候选长度接近但语气仍有偏差 → 中间分）
参考回复：哦哦
候选回复：原来是这样啊，我明白了。
分数：3
【示例 4】（候选与参考长度不同但语气风格贴近 → 高分）
参考回复：快九点吧
候选回复：九点左右到
分数：4";

/// 一条判分模板摘要。
///
/// 字段约定:
/// - `task`: 任务名（与 `JudgeTask::as_str` 一致）；
/// - `sha256`: 系统提示文本的 sha256（十六进制小写）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateHash {
    pub task: String,
    pub sha256: String,
}

/// 系统提示文本的 sha256（十六进制小写）。
pub fn template_sha256(task: JudgeTask) -> String {
    format!("{:x}", Sha256::digest(task.system().as_bytes()))
}

/// 本次判分实际使用的模板摘要（按任务固定顺序）。
pub fn templates() -> Vec<TemplateHash> {
    [
        JudgeTask::RubricPoints,
        JudgeTask::Unanswerable,
        JudgeTask::Conflict,
        JudgeTask::Tone,
    ]
    .into_iter()
    .map(|task| TemplateHash {
        task: task.as_str().to_string(),
        sha256: template_sha256(task),
    })
    .collect()
}

// =========================================================
// 用户消息拼装
// =========================================================

/// 分项要点判分的用户消息。
///
/// 参数:
/// - `question`: 题目原文；
/// - `gold_answer`: 参考答案；
/// - `reply`: 待评回复；
/// - `points`: 本题全部要点（序号即数组下标）。
pub fn rubric_user(
    question: &str,
    gold_answer: &str,
    reply: &str,
    points: &[QrelsScorePoint],
) -> String {
    let mut body = String::new();
    body.push_str("问题: ");
    body.push_str(question);
    body.push_str("\n参考回复: ");
    body.push_str(gold_answer);
    body.push_str("\n待评回复: ");
    body.push_str(reply);
    body.push_str("\n要点:");
    for (index, point) in points.iter().enumerate() {
        body.push_str(&format!(
            "\n{index}. {}（{} 分）",
            point.description, point.score
        ));
    }
    body
}

/// 不可回答判定的用户消息。
pub fn unanswerable_user(question: &str, reply: &str) -> String {
    format!("问题: {question}\n待评回复: {reply}")
}

/// 冲突检测判定的用户消息。
pub fn conflict_user(question: &str, gold_answer: &str, reply: &str) -> String {
    format!("问题: {question}\n参考答案: {gold_answer}\n待评回复: {reply}")
}

/// 语气维判分的用户消息。
///
/// 参数:
/// - `question`: 题目原文（只给语境，评分口径仍是语气风格）；
/// - `reference`: 参考回复（本题取 gold_answer）；
/// - `reply`: 待评回复。
pub fn tone_user(question: &str, reference: &str, reply: &str) -> String {
    format!("问题: {question}\n参考回复: {reference}\n待评回复: {reply}")
}

/// 重试用的用户消息：在原消息末尾追加 JSON 约束。
pub fn retry_user(user: &str) -> String {
    format!("{user}\n{RETRY_SUFFIX}")
}

// =========================================================
// 输出解析
// =========================================================

/// `rubric_points` 的输出负载。
#[derive(Deserialize)]
struct PointsPayload {
    points: Vec<PointPayload>,
}

/// `rubric_points` 的单条要点输出。
#[derive(Deserialize)]
struct PointPayload {
    index: usize,
    hit: bool,
    #[serde(default)]
    reason: String,
}

/// `tone` 的输出负载（JSON 形态）。
#[derive(Deserialize)]
struct ScorePayload {
    score: u8,
    #[serde(default)]
    reason: String,
}

/// 布尔任务输出负载（两种键共用一个结构，按任务取值）。
#[derive(Deserialize)]
struct FlagPayload {
    #[serde(default)]
    refused: Option<bool>,
    #[serde(default)]
    accepted_premise: Option<bool>,
    #[serde(default)]
    reason: String,
}

/// 解析一票模型输出。
///
/// 参数:
/// - `task`: 任务类型；
/// - `body`: 模型返回的完整文本；
/// - `points`: rubric 任务的要点数（其他任务忽略）。
///
/// 返回:
/// - 成功时返回票面值；JSON 不合法、缺少字段、要点序号缺失或重复返回 `None`（该票作废）。
pub fn parse(task: JudgeTask, body: &str, points: Option<usize>) -> Option<JudgeValue> {
    match task {
        JudgeTask::RubricPoints => parse_points(body, points?),
        JudgeTask::Unanswerable => parse_flag(body, JudgeTask::Unanswerable),
        JudgeTask::Conflict => parse_flag(body, JudgeTask::Conflict),
        JudgeTask::Tone => parse_score(body),
    }
}

/// 解析语气维输出：先按 JSON 取 `score`，JSON 不成立再按整数提取。
///
/// 说明:
/// - JSON 形态 `{"score":4,"reason":"..."}` 直接取分，理由一并保留；
/// - 纯整数 / 中文数字输出（`4`、`评分：5`、`四`）走整数提取，理由留空；
/// - JSON 成立但分值越界（0 或 6 以上）视为解析失败，不做二次提取，交重试处理。
fn parse_score(body: &str) -> Option<JudgeValue> {
    let trimmed = body.trim();
    match serde_json::from_str::<ScorePayload>(trimmed) {
        Ok(payload) => (1..=5)
            .contains(&payload.score)
            .then_some(JudgeValue::Score {
                value: payload.score,
                reason: payload.reason,
            }),
        Err(_) => {
            crate::score::persona::parse_judge_score(trimmed).map(|value| JudgeValue::Score {
                value,
                reason: String::new(),
            })
        }
    }
}

/// 解析逐要点输出，并按 index 还原顺序。
fn parse_points(body: &str, expected: usize) -> Option<JudgeValue> {
    if expected == 0 {
        return None;
    }
    let payload: PointsPayload = serde_json::from_str(body.trim()).ok()?;
    if payload.points.len() != expected {
        return None;
    }
    let mut slots: Vec<Option<PointVote>> = vec![None; expected];
    for point in payload.points {
        if point.index >= expected || slots[point.index].is_some() {
            return None;
        }
        slots[point.index] = Some(PointVote {
            hit: point.hit,
            reason: point.reason,
        });
    }
    let points = slots.into_iter().collect::<Option<Vec<_>>>()?;
    Some(JudgeValue::Points { points })
}

/// 解析布尔输出。
fn parse_flag(body: &str, task: JudgeTask) -> Option<JudgeValue> {
    let payload: FlagPayload = serde_json::from_str(body.trim()).ok()?;
    let value = match task {
        JudgeTask::Unanswerable => payload.refused?,
        JudgeTask::Conflict => payload.accepted_premise?,
        _ => return None,
    };
    Some(JudgeValue::Flag {
        value,
        reason: payload.reason,
    })
}
