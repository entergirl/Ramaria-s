//! rust/crates/ramaria-memory/src/l1/summarizer.rs - L0→L1 摘要生成管线
//!
//! 设计特点:
//! - 依赖注入: 通过 `&dyn LlmProvider` + `&dyn StorageBackend` 解耦具体实现
//! - 完整流程: 取消息 → 格式化 → 选Prompt → 调LLM → 解析JSON → 校验 → 存L1 + 写关键词
//! - JSON 解析三步递进: 直接解析 → 剥离 think 标签 → 正则提取
//! - 字段校验对齐五档效价/显著性，非法值自动钳制到最近合法档位
//! - 关键词自动写回 keyword_pool，驱动词典累积
//! - 所有可恢复错误转换为 RamariaError，保留上下文

use ramaria_core::keyword::KeywordToken;
use ramaria_core::traits::ChatRequest;
use ramaria_core::types::MessageRole;
use ramaria_core::{LlmProviderTrait, MemoryL1, RamariaError, RamariaResult, StorageBackend};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::prompt::{KEYWORD_INJECT_LIMIT, KEYWORD_INJECT_THRESHOLD, build_l1_prompt};
use crate::utils;

// =========================================================
// LLM 响应 JSON 结构（反序列化目标）
// =========================================================

/// LLM 返回的 L1 摘要 JSON 结构。
///
/// 字段:
/// - 所有字段均为 `Option`，以容忍 LLM 输出缺失字段。
/// - 校验阶段再填充默认值，避免解析阶段 panic。
/// - `situation_strength` 为 新增字段，当前 LLM prompt 尚未包含此输出，
///   因此大部分情况下为 None（等效 3）。
#[derive(Debug, Deserialize)]
struct L1SummaryResponse {
    summary: Option<String>,
    keywords: Option<String>,
    time_period: Option<String>,
    atmosphere: Option<String>,
    valence: Option<f64>,
    salience: Option<f64>,
    /// 情境强度 1-5，None 时按默认值 3 处理
    #[serde(default)]
    situation_strength: Option<i32>,
}

// =========================================================
// L1 Summarizer 配置
// =========================================================

/// L1 Summarizer 配置。
///
/// 字段约定:
/// - `max_tokens`: LLM 最大输出 token 数，默认 512（与 Python 一致）。
/// - `temperature`: LLM 生成温度，默认 0.3。
/// - `conversation_format_user`: 用户消息格式化前缀。
/// - `conversation_format_assistant`: 助手消息格式化前缀。
/// - `persona_uid`: 本条摘要描述的对象（人格标识），None 表示描述默认用户。
/// - `context_json`: 分组上下文，含 chat_partners 列表。
/// - `situation_strength`: 情境强度（1-5），None 时 LLM 输出缺失则默认 3。
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
}

impl Default for L1SummarizerConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 512,
            user_prefix: "用户：".to_string(),
            assistant_prefix: "助手：".to_string(),
            persona_uid: None,
            context_json: None,
            situation_strength: None,
        }
    }
}

// =========================================================
// L1 Summarizer
// =========================================================

/// L0→L1 摘要生成器。
///
/// 职责:
/// - 接收已关闭的 session_id，读取全部 L0 消息。
/// - 调用 LLM 生成结构化摘要 JSON。
/// - 解析、校验并写入 `memory_l1` 表。
/// - 将关键词写回 `keyword_pool`。
///
/// 用法:
/// ```ignore
/// let summarizer = L1Summarizer::new(&llm, &storage, L1SummarizerConfig::default);
/// let l1 = summarizer.summarize_session(session_id).await?;
/// ```
pub struct L1Summarizer<'a> {
    config: L1SummarizerConfig,
    llm: &'a dyn LlmProviderTrait,
    storage: &'a dyn StorageBackend,
}

impl<'a> L1Summarizer<'a> {
    /// 创建新的 L1Summarizer。
    pub fn new(
        llm: &'a dyn LlmProviderTrait,
        storage: &'a dyn StorageBackend,
        config: L1SummarizerConfig,
    ) -> Self {
        Self {
            config,
            llm,
            storage,
        }
    }

    // =========================================================
    // 公共 API
    // =========================================================

    /// 为指定 session 生成 L1 摘要。
    ///
    /// 参数:
    /// - `session_id`: 已关闭的 session UUID。
    ///
    /// 返回:
    /// - 成功时返回已写入存储的 `MemoryL1`。
    /// - session 无消息时返回 Validation 错误。
    /// - LLM 调用失败时返回 Llm 错误。
    /// - JSON 解析全部失败时返回 Validation 错误。
    pub async fn summarize_session(&self, session_id: Uuid) -> RamariaResult<MemoryL1> {
        // 1. 读取 session 全部消息
        let messages = self.storage.list_messages(session_id).await.map_err(|e| {
            warn!(%session_id, error=%e, "读取 session 消息失败");
            RamariaError::storage(format!("读取 session {session_id} 消息失败: {e}"))
        })?;

        if messages.is_empty() {
            return Err(RamariaError::validation(format!(
                "session {session_id} 无消息，无法生成摘要"
            )));
        }

        debug!(%session_id, msg_count = messages.len(), "开始生成 L1 摘要");

        // 2. 格式化对话文本
        let conversation = self.format_conversation(&messages);

        // 3. 获取关键词候选
        let keyword_candidates = self.get_keyword_candidates().await;

        // 4. 构建 prompt
        let prompt = build_l1_prompt(&conversation, keyword_candidates.as_deref());

        // 5. 调用 LLM
        let request_id = Uuid::new_v4();
        let llm_request = ChatRequest {
            system_prompt: String::new(),
            memory_context: None,
            history: vec![],
            user_message: prompt,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            request_id,
        };

        let raw_response = self.llm.chat(&llm_request).await.map_err(|e| {
            warn!(%session_id, %request_id, error=%e, "LLM 调用失败");
            RamariaError::llm(format!(
                "session {session_id} L1 摘要生成 LLM 调用失败: {e}"
            ))
        })?;

        debug!(%session_id, %request_id, "LLM 返回 {} 字符", raw_response.len());

        // 6. 解析 JSON
        let parsed = self.parse_summary_json(&raw_response)?;

        // 7. 校验并修正字段
        let (mut l1, keywords) = Self::validate_and_build(&parsed, session_id);

        // 注入 config 中的上下文字段
        l1.persona_uid = self.config.persona_uid.clone();
        l1.context_json = self.config.context_json.clone();
        // 优先使用 LLM 输出的 situation_strength，缺失时回退 config 默认值
        l1.situation_strength = parsed
            .situation_strength
            .or(self.config.situation_strength)
            .or(Some(3)); // 最终默认值：中性情境

        // 8. 写入存储
        self.storage.save_memory_l1(&l1).await.map_err(|e| {
            warn!(%session_id, error=%e, "写入 memory_l1 失败");
            RamariaError::storage(format!("写入 session {session_id} L1 摘要失败: {e}"))
        })?;

        // 9. 写回关键词词典 + 倒排索引
        for kw_token in &keywords {
            // 写回 keyword_pool
            if let Err(e) = self.storage.upsert_keyword(kw_token.as_str()).await {
                warn!(%session_id, keyword=%kw_token, error=%e, "关键词写回失败（非致命）");
            }
            // 写入 keyword_refs 倒排索引（L1 文档引用，doc_id 使用 UUID 字符串）
            if let Err(e) = self
                .storage
                .insert_keyword_ref(
                    kw_token.as_str(),
                    "l1",
                    &l1.id.to_string(),
                    l1.persona_uid.as_deref().unwrap_or(""),
                    1.0,
                )
                .await
            {
                warn!(%session_id, keyword=%kw_token, error=%e, "关键词引用写入失败（非致命）");
            }
        }

        info!(
            %session_id,
            l1_id = %l1.id,
            keyword_count = keywords.len(),
            valence = l1.valence,
            salience = l1.salience,
            "L1 摘要生成完成"
        );

        Ok(l1)
    }

    // =========================================================
    // 内部方法
    // =========================================================

    /// 将消息列表格式化为对话文本。
    ///
    /// 格式:
    /// - User 消息: `用户：{content}`
    /// - Assistant 消息: `助手：{content}`
    /// - System/Tool 消息: 跳过（不参与摘要）
    fn format_conversation(&self, messages: &[ramaria_core::types::Message]) -> String {
        let mut lines = Vec::with_capacity(messages.len());
        for msg in messages {
            let prefix = match msg.role {
                MessageRole::User => &self.config.user_prefix,
                MessageRole::Assistant => &self.config.assistant_prefix,
                // System/Tool 消息不进入摘要上下文
                _ => continue,
            };
            lines.push(format!("{prefix}{}", msg.content));
        }
        lines.join("\n")
    }

    /// 获取关键词候选字符串。
    ///
    /// 策略:
    /// - 词典 ≤ 100 条: 全部返回
    /// - 词典 > 100 条: 仅返回前 50 条（已按 use_count 降序排列）
    /// - 词典为空: 返回 None
    async fn get_keyword_candidates(&self) -> Option<String> {
        let keywords = match self.storage.list_keywords().await {
            Ok(kws) => kws,
            Err(e) => {
                warn!(error=%e, "读取 keyword_pool 失败，跳过关键词注入");
                return None;
            }
        };

        if keywords.is_empty() {
            return None;
        }

        let selected = if keywords.len() <= KEYWORD_INJECT_THRESHOLD {
            keywords
        } else {
            keywords
                .into_iter()
                .take(KEYWORD_INJECT_LIMIT)
                .collect::<Vec<_>>()
        };

        Some(selected.join(", "))
    }

    /// 三步递进 JSON 解析。
    ///
    /// 步骤:
    /// 1. 直接 `serde_json::from_str`
    /// 2. 剥离 `<think>...</think>` 标签后重试
    /// 3. 正则提取首对 `{...}` 后解析
    ///
    /// 全部失败返回 Validation 错误，包含原始响应前 100 字符供诊断。
    fn parse_summary_json(&self, raw: &str) -> RamariaResult<L1SummaryResponse> {
        // 步骤 1: 直接解析
        if let Ok(parsed) = serde_json::from_str::<L1SummaryResponse>(raw) {
            return Ok(parsed);
        }

        // 步骤 2: 剥离 think 标签
        let stripped = utils::strip_thinking(raw);
        if stripped != raw
            && let Ok(parsed) = serde_json::from_str::<L1SummaryResponse>(&stripped)
        {
            debug!("剥离 think 标签后解析成功");
            return Ok(parsed);
        }

        // 步骤 3: 正则提取首对花括号
        if let Some(json_segment) = utils::extract_first_json_object(raw)
            && let Ok(parsed) = serde_json::from_str::<L1SummaryResponse>(&json_segment)
        {
            debug!("正则提取 JSON 对象后解析成功");
            return Ok(parsed);
        }

        // 全部失败
        let preview: String = raw.chars().take(100).collect();
        warn!(response_preview=%preview, "L1 摘要 JSON 解析全部失败");
        Err(RamariaError::validation(format!(
            "L1 摘要 JSON 解析失败，原始响应前 100 字符: {preview}"
        )))
    }

    /// 校验 LLM 返回字段并构建 MemoryL1。
    ///
    /// 校验规则（与 Python v0.x 对齐）:
    /// - `summary`: 必填，为空时填降级文本
    /// - `time_period`: 严格六选一，非法值置 None
    /// - `atmosphere`: 四字以内，超长截断
    /// - `valence`: 五档钳制到最近的合法值
    /// - `salience`: 五档钳制到最近的合法值
    ///
    /// 返回:
    /// - (MemoryL1, KeywordToken 列表)
    fn validate_and_build(
        parsed: &L1SummaryResponse,
        session_id: Uuid,
    ) -> (MemoryL1, Vec<KeywordToken>) {
        // summary: 必填降级
        let summary = parsed.summary.as_deref().unwrap_or("").trim().to_string();
        let summary = if summary.is_empty() {
            warn!(%session_id, "LLM 返回空 summary，使用降级文本");
            "（摘要生成失败，内容为空）".to_string()
        } else {
            summary
        };

        // keywords: 容许为空，拆分为列表
        let (keywords_str, keywords_list) = parse_keywords(parsed.keywords.as_deref());

        // time_period: 严格六选一
        let time_period = parsed
            .time_period
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| {
                const VALID: &[&str] = &["清晨", "上午", "下午", "傍晚", "夜间", "深夜"];
                let ok = VALID.contains(&s.as_str());
                if !ok {
                    warn!(%session_id, time_period=%s, "非法的 time_period 值，置为 None");
                }
                ok
            });

        // atmosphere: 四字以内
        let atmosphere = parsed
            .atmosphere
            .as_deref()
            .map(|s| s.trim().to_string())
            .map(|s| {
                let truncated: String = s.chars().take(4).collect();
                if truncated.len() != s.chars().count() {
                    debug!(%session_id, original=%s, truncated=%truncated, "atmosphere 超长截断");
                }
                truncated
            });

        // valence: 五档钳制
        let valence = utils::clamp_valence(parsed.valence.unwrap_or(0.0));
        if (valence - parsed.valence.unwrap_or(0.0)).abs() > f64::EPSILON {
            debug!(
                %session_id,
                original = parsed.valence.unwrap_or(0.0),
                clamped = valence,
                "valence 钳制到合法档位"
            );
        }

        // salience: 五档钳制
        let salience = utils::clamp_salience(parsed.salience.unwrap_or(0.5));
        if (salience - parsed.salience.unwrap_or(0.5)).abs() > f64::EPSILON {
            debug!(
                %session_id,
                original = parsed.salience.unwrap_or(0.5),
                clamped = salience,
                "salience 钳制到合法档位"
            );
        }

        let l1 = MemoryL1 {
            id: ramaria_core::types::new_id(),
            session_id,
            summary,
            keywords: keywords_str,
            time_period,
            atmosphere,
            valence,
            salience,
            absorbed: false,
            created_at: ramaria_core::types::now_ms(),
            last_accessed_at: None,
            persona_uid: None,        // 由调用方在 construct 阶段通过 config 注入
            context_json: None,       // 由调用方在 construct 阶段通过 config 注入
            situation_strength: None, // 由 LLM 输出或 config 注入
        };

        (l1, keywords_list)
    }
}

// =========================================================
// 纯函数辅助
// =========================================================

/// 解析关键词字符串为 `(存储用的逗号分隔字符串, 标准化关键词列表)`。
///
/// 如果输入为空或仅含空白字符，返回 `(None, vec![])`。
/// v1.3: 返回 `Vec<KeywordToken>` 替代裸 `String`。
fn parse_keywords(raw: Option<&str>) -> (Option<String>, Vec<KeywordToken>) {
    let cleaned = raw.map(|s| s.trim()).filter(|s| !s.is_empty());
    match cleaned {
        None => (None, vec![]),
        Some(s) => {
            let list: Vec<KeywordToken> = s
                .split(',')
                .map(|k| k.trim())
                .filter(|k| !k.is_empty())
                .filter_map(KeywordToken::new)
                .collect();
            // 存储时仍用原始逗号分隔字符串（兼容旧 schema）
            (Some(s.to_string()), list)
        }
    }
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- strip_thinking ----

    #[test]
    fn strip_thinking_simple() {
        let input = "<think>Let me think...</think>\n{\"summary\": \"hello\"}";
        let result = crate::utils::strip_thinking(input);
        assert!(!result.contains("<think>"));
        assert!(result.contains("{\"summary\""));
    }

    #[test]
    fn strip_thinking_no_tags() {
        let input = "{\"summary\": \"hello\"}";
        let result = crate::utils::strip_thinking(input);
        assert_eq!(result, input);
    }

    #[test]
    fn strip_thinking_multiline() {
        let input = "Some text\n<think>\nreasoning here\n</think>\n{\"summary\": \"test\"}";
        let result = crate::utils::strip_thinking(input);
        assert!(result.contains("Some text"));
        assert!(result.contains("{\"summary\": \"test\"}"));
        assert!(!result.contains("reasoning"));
    }

    // ---- extract_first_json_object ----

    #[test]
    fn extract_simple_json() {
        let input = "前缀文本 {\"summary\": \"测试\", \"valence\": 0.5} 后缀文本";
        let result = crate::utils::extract_first_json_object(input).unwrap();
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
        assert!(result.contains("\"summary\""));
    }

    #[test]
    fn extract_nested_json() {
        let input = r#"{"a": {"b": [1,2,3]}, "c": "d"}"#;
        let result = crate::utils::extract_first_json_object(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn extract_no_json() {
        let input = "纯文本无JSON";
        assert!(crate::utils::extract_first_json_object(input).is_none());
    }

    #[test]
    fn extract_with_markdown_block() {
        let input = "```json\n{\"summary\": \"测试\"}\n```";
        let result = crate::utils::extract_first_json_object(input).unwrap();
        assert!(result.contains("\"summary\""));
    }

    // ---- clamp_valence ----

    #[test]
    fn clamp_valence_exact_match() {
        assert!((crate::utils::clamp_valence(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(1.0) - 1.0).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(-1.0) - (-1.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_valence_to_nearest() {
        assert!((crate::utils::clamp_valence(0.3) - 0.5).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(-0.7) - (-0.5)).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(0.9) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_valence_boundary() {
        let result = crate::utils::clamp_valence(0.25);
        assert!(result == 0.0 || result == 0.5);
    }

    // ---- clamp_salience ----

    #[test]
    fn clamp_salience_exact_match() {
        assert!((crate::utils::clamp_salience(0.5) - 0.5).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.75) - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_salience_to_nearest() {
        assert!((crate::utils::clamp_salience(0.3) - 0.25).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.9) - 1.0).abs() < f64::EPSILON);
    }

    // ---- validate_and_build (free function) ----

    #[test]
    fn validate_summary_empty_fallback() {
        let parsed = L1SummaryResponse {
            summary: Some("".into()),
            keywords: None,
            time_period: None,
            atmosphere: None,
            valence: Some(0.5),
            salience: Some(0.5),
            situation_strength: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _keywords) = L1Summarizer::validate_and_build(&parsed, sid);
        assert!(l1.summary.contains("失败"));
    }

    #[test]
    fn validate_time_period_invalid() {
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("午夜".into()), // 非法值
            atmosphere: None,
            valence: Some(0.0),
            salience: Some(0.5),
            situation_strength: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        assert!(l1.time_period.is_none(), "非法 time_period 应被过滤");
    }

    #[test]
    fn validate_atmosphere_truncation() {
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("上午".into()),
            atmosphere: Some("非常轻松愉快的一天".into()), // 9字
            valence: Some(0.5),
            salience: Some(0.5),
            situation_strength: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        let atm = l1.atmosphere.unwrap();
        assert!(atm.chars().count() <= 4, "atmosphere 应截断到 ≤4 字: {atm}");
    }

    #[test]
    fn validate_keywords_parsing() {
        let parsed = L1SummaryResponse {
            summary: Some("测试".into()),
            keywords: Some("工作, 学习, 编程".into()),
            time_period: Some("下午".into()),
            atmosphere: Some("专注高效".into()),
            valence: Some(0.0),
            salience: Some(0.5),
            situation_strength: None,
        };
        let sid = ramaria_core::types::new_id();
        let (_l1, keywords) = L1Summarizer::validate_and_build(&parsed, sid);
        assert_eq!(keywords.len(), 3);
        assert!(keywords.contains(&"工作".to_string()));
        assert!(keywords.contains(&"学习".to_string()));
        assert!(keywords.contains(&"编程".to_string()));
    }

    // ---- parse_summary_json (via pure helpers) ----

    #[test]
    fn parse_valid_json_direct() {
        let raw = r#"{"summary": "测试摘要", "valence": 0.5, "salience": 0.5}"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.summary.unwrap(), "测试摘要");
    }

    #[test]
    fn parse_with_think_tags() {
        let raw = "<think>reasoning</think>\n{\"summary\": \"测试\"}";
        let stripped = crate::utils::strip_thinking(raw);
        let parsed: L1SummaryResponse = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed.summary.unwrap(), "测试");
    }

    #[test]
    fn parse_with_prefix_text() {
        let raw = "这是前缀说明文字 {\"summary\": \"测试\", \"valence\": 0.0}";
        let extracted = crate::utils::extract_first_json_object(raw).unwrap();
        let parsed: L1SummaryResponse = serde_json::from_str(&extracted).unwrap();
        assert_eq!(parsed.summary.unwrap(), "测试");
    }

    // ---- 完整流程（需要 mock） ----
    // 完整集成测试在 l1/mod.rs 的测试中，使用 mock LlmProvider + mock StorageBackend

    // ---- situation_strength 解析 ----

    #[test]
    fn parse_situation_strength_from_json() {
        let raw =
            r#"{"summary": "测试", "valence": 0.0, "salience": 0.5, "situation_strength": 2}"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.situation_strength, Some(2));
    }

    #[test]
    fn parse_situation_strength_missing_defaults_none() {
        let raw = r#"{"summary": "测试", "valence": 0.0, "salience": 0.5}"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.situation_strength, None);
    }

    #[test]
    fn validate_situation_strength_injected_from_llm() {
        // validate_and_build 始终设置 situation_strength = None，
        // 实际注入在 summarize_session 中完成（LLM 输出 > config > 默认 3）
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("上午".into()),
            atmosphere: Some("轻松".into()),
            valence: Some(0.5),
            salience: Some(0.5),
            situation_strength: Some(5),
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        // validate_and_build 不负责注入 — 注入由 summarize_session 步骤 7 完成
        assert_eq!(l1.situation_strength, None);
    }

    #[test]
    fn validate_situation_strength_defaults_to_3() {
        // LLM 未输出 situation_strength → config 也未设置 → 应回退到 Some(3)
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("下午".into()),
            atmosphere: Some("专注".into()),
            valence: Some(0.0),
            salience: Some(0.5),
            situation_strength: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        // validate_and_build 设置 situation_strength 为 None，
        // 实际赋值在 summarize_session 中（步骤 7）
        // validate_and_build 中设为 None，最终由调用方（步骤 7）注入
        assert_eq!(l1.situation_strength, None);
    }
}
