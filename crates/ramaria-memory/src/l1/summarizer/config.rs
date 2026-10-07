//! crates/ramaria-memory/src/l1/summarizer/config.rs - L1 Summarizer 配置
//!
//! 设计特点:
//! - 纯数据结构 + Default，无 I/O、无逻辑分支。
//! - `utt_splitter` 为 None 时整会话单块（v1.4 行为）；Some 时按话语块逐块生成。
//! - `prior_context_*` 控制上一块上文注入形态（原文 / 上一 L1 摘要 + 线索 / 截断原文）。
//! - `fanout_others` 开启时按块/段内他人发言者复制 L1 行（多画像分发，默认关闭）。

// =========================================================
// L1 Summarizer 配置
// =========================================================

/// L1 Summarizer 配置。
///
/// 字段约定:
/// - `max_tokens`: LLM 最大输出 token 数，默认 1024。v1.4 起 L1 输出包含
///   `evidence_notes` 结构化对象数组（1-3 条 × text/time/who/cause 槽位），
///   完整 JSON 明显长于旧版字符串数组输出；512（Python 旧值）过紧会导致
///   LLM 输出被截断、JSON 解析失败，故默认提升至 1024。
/// - `temperature`: LLM 生成温度，默认 0.3。
/// - `conversation_format_user`: 用户消息格式化前缀。
/// - `conversation_format_assistant`: 助手消息格式化前缀。
/// - `persona_uid`: 本条摘要描述的对象（人格标识），None 表示描述默认用户。
/// - `context_json`: 分组上下文，含 chat_partners 列表。
/// - `situation_strength`: 情境强度（1-5），None 时 LLM 输出缺失则默认 3。
/// - `utt_splitter`（v1.5 B2）: utt 切分配置。`Some` → 将 session 消息切分为
///   话语块并逐块生成 L1（块 N 注入上一块上文，上下文感知生成，§6.3）；
///   `None` → 整会话一块，与 v1.4 行为完全一致（独立摘要，无上文注入）。
///   短会话（单块）自然回退 v1.4 行为。
/// - `prior_context_threshold`（v1.5 B2）: 上一块消息数 ≤ 此阈值 → 注入 L0 原文；
///   超过 → 注入上一块 L1 摘要 + 结构化线索。默认 20（§6.3 示例值）。
/// - `prior_context_max_chars`（v1.5 B2）: 长块无上一 L1 时回退注入原文的截断上限。
/// - `fanout_others`: 多画像分发开关（群聊场景）。开启时按块/段内他人发言者复制 L1 行：
///   参与者为空（无他人参与）→ 原行保留；参与者非空 → 每参与者一行（各自 persona_uid），
///   原行不落库。默认关闭（私聊路径行为不变）。
#[derive(Debug, Clone)]
pub struct L1SummarizerConfig {
    /// LLM 生成温度 0.0..2.0
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
    /// 用户消息格式化前缀
    pub user_prefix: String,
    /// 助手消息格式化前缀
    pub assistant_prefix: String,
    /// 人格关联——本条摘要描述的对象
    pub persona_uid: Option<String>,
    /// 分组上下文——JSON 格式 `{"chat_partners": ["user-0001", "char-0003"]}`
    pub context_json: Option<String>,
    /// 情境强度默认值（1-5），None 时使用 3
    pub situation_strength: Option<i32>,
    /// utt 切分配置（v1.5 B2 上下文感知生成），None = v1.4 整会话单块
    pub utt_splitter: Option<crate::utt::UttSplitterConfig>,
    /// 多画像分发：按块/段内他人发言者复制 L1 行（每行各自 persona_uid；默认关闭）
    pub fanout_others: bool,
    /// 上一块消息数阈值（≤ 注入原文，> 注入上一 L1 摘要+线索），默认 20
    pub prior_context_threshold: usize,
    /// 长块无上一 L1 时原文截断上限（字符），默认 1500
    pub prior_context_max_chars: usize,
}

impl Default for L1SummarizerConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 1024,
            user_prefix: "用户：".to_string(),
            assistant_prefix: "助手：".to_string(),
            persona_uid: None,
            context_json: None,
            situation_strength: None,
            utt_splitter: Some(crate::utt::UttSplitterConfig::default()),
            fanout_others: false,
            prior_context_threshold: 20,
            prior_context_max_chars: 1500,
        }
    }
}
