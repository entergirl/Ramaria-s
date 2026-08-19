//! crates/ramaria-memory/src/l1/summarizer.rs - L0→L1 摘要生成管线
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
use ramaria_core::types::{EvidenceNote, MessageRole};
use ramaria_core::{LlmProviderTrait, MemoryL1, RamariaError, RamariaResult, StorageBackend};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::prompt::{KEYWORD_INJECT_LIMIT, KEYWORD_INJECT_THRESHOLD, build_l1_prompt};
use crate::utils;
use crate::utt::UttChunk;

// =========================================================
// LLM 响应 JSON 结构（反序列化目标）
// =========================================================

/// LLM 返回的 L1 摘要 JSON 结构。
///
/// 字段:
/// - 所有字段均为 `Option`，以容忍 LLM 输出缺失字段。
/// - 校验阶段再填充默认值，避免解析阶段 panic。
/// - `situation_strength` 为新增字段，prompt 已包含此输出，缺失时由 config 注入或默认 3。
/// - `evidence_notes` 为结构化证据线索（v1.4），LLM 可能输出缺失或空数组，
///   校验失败时降级为 `Some(vec![])` 但不阻塞 L1 生成。
///   M1~M3 过渡期 LLM 仍可能输出旧字符串数组，由自定义反序列化器统一转换为
///   对象数组（字符串落 `text` 槽位）；M4 起 prompt 升级为对象数组。
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
    /// 证据线索列表（宽容解析：对象数组 / 旧字符串数组 / 缺失）
    #[serde(default, deserialize_with = "deserialize_evidence_notes")]
    evidence_notes: Option<Vec<EvidenceNote>>,
    /// 相对上一块的话题延续关系（v1.5 B2）：延续/转折/无关。
    /// 无上一块时 prompt 不含该字段，LLM 不会输出 → None。
    #[serde(default)]
    continuation: Option<String>,
}

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
            prior_context_threshold: 20,
            prior_context_max_chars: 1500,
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
/// ```no_run
/// # use ramaria_memory::{L1Summarizer, L1SummarizerConfig};
/// // llm / storage 由上层注入（&dyn LlmProviderTrait / &dyn StorageBackend）；
/// // 需完整 mock 才能运行，故示例仅示意构造（no_run）。
/// let summarizer = L1Summarizer::new(todo!(), todo!(), L1SummarizerConfig::default());
/// # let _ = &summarizer;
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
    /// v1.5 B2（§6.3）上下文感知生成：
    /// - 配置了 `utt_splitter` 时，将 session 消息切分为话语块，逐块生成 L1；
    ///   块 N（N>0）生成时注入上一块上文（短块注入原文 / 长块注入上一 L1 摘要+线索），
    ///   输出 `continuation`（延续/转折/无关）。
    /// - 单块 session 或未配置切分器 → 与 v1.4 行为完全一致（独立摘要）。
    ///
    /// 块级容错：
    /// - 某块 LLM 调用/解析失败 → 记 warn、该块不产出 L1，后续块以上一块
    ///   原文（截断）作为上文继续生成（不阻塞整体）。
    /// - 全部块均失败 → 返回最后一个错误（与 v1.4 失败语义一致）。
    /// - 成功块统一写库；写库失败为硬错误直接返回。
    ///
    /// 参数:
    /// - `session_id`: 已关闭的 session UUID。
    ///
    /// 返回:
    /// - 成功时返回最后成功块的 `MemoryL1`（调用方从库读取的顺序一致）。
    /// - session 无消息时返回 Validation 错误。
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

        // 2. 切分为话语块（B2 上下文感知生成的块粒度）
        //    - 配置了 utt_splitter → 用 split_messages 切分（目标 persona 为 config.persona_uid）
        //    - 未配置 → 整会话一块（v1.4 行为）
        //    - 切分结果为空（如纯用户消息块被丢弃）→ 回退整会话一块，
        //      保证与 v1.4 至少产出一条摘要的语义一致。
        let chunks = match &self.config.utt_splitter {
            Some(splitter_cfg) => {
                let target = self.config.persona_uid.as_deref();
                let split = crate::utt::splitter::split_messages(&messages, target, splitter_cfg);
                if split.is_empty() {
                    vec![UttChunk::from_messages(messages.clone())]
                } else {
                    split
                }
            }
            None => vec![UttChunk::from_messages(messages)],
        };
        debug!(%session_id, block_count = chunks.len(), "L1 摘要按块生成");

        // 3. 逐块生成（内存收集，全部成功后统一写库）
        //    - generated[i] = 块 i 的 (L1, 关键词列表)（None = 该块生成失败，降级）
        //    - 块 i 的上文来自块 i-1 的生成结果（内存传递，只注入最近 1 块，不链式）
        let mut generated: Vec<Option<(MemoryL1, Vec<KeywordToken>)>> =
            Vec::with_capacity(chunks.len());
        let mut last_error: Option<RamariaError> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            // 构建上一块上文（混合形态，§6.3）：
            // - 上一块消息数 ≤ prior_context_threshold → 注入 L0 原文
            // - 长块 → 注入上一 L1 摘要 + 结构化线索（上一 L1 缺失 → 原文截断）
            let prior_context = if i == 0 {
                None
            } else {
                Some(build_prior_context(
                    &chunks[i - 1],
                    generated[i - 1].as_ref().map(|(l1, _)| l1),
                    &self.config,
                    &self.config.user_prefix,
                    &self.config.assistant_prefix,
                ))
            };

            match self
                .generate_chunk_l1(session_id, chunk, prior_context.as_deref())
                .await
            {
                Ok((l1, keywords)) => {
                    debug!(%session_id, block_index = i, "块 {} L1 生成成功", i);
                    generated.push(Some((l1, keywords)));
                }
                Err(e) => {
                    // 块级降级：不阻塞整体，后续块以原文（截断）作为上文
                    warn!(%session_id, block_index = i, error=%e, "L1 块生成失败，该块无摘要（降级继续）");
                    last_error = Some(e);
                    generated.push(None);
                }
            }
        }

        // 4. 统一写库（成功块）+ 写回关键词
        let mut saved_last: Option<MemoryL1> = None;
        for entry in generated.iter().flatten() {
            let (l1, keywords) = entry;
            self.storage.save_memory_l1(l1).await.map_err(|e| {
                warn!(%session_id, l1_id = %l1.id, error=%e, "写入 memory_l1 失败");
                RamariaError::storage(format!("写入 session {session_id} L1 摘要失败: {e}"))
            })?;
            saved_last = Some(l1.clone());
            self.write_back_keywords(session_id, l1, keywords).await;
        }

        // 5. 返回
        match saved_last {
            Some(l1) => {
                info!(
                    %session_id,
                    l1_id = %l1.id,
                    total_blocks = chunks.len(),
                    success_blocks = generated.iter().filter(|g| g.is_some()).count(),
                    "L1 摘要生成完成（按块）"
                );
                Ok(l1)
            }
            None => Err(last_error.unwrap_or_else(|| {
                RamariaError::validation(format!("session {session_id} 全部 L1 块生成失败"))
            })),
        }
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
        format_messages(
            messages,
            &self.config.user_prefix,
            &self.config.assistant_prefix,
        )
    }

    /// 为单个话语块生成 L1（LLM 调用 + 解析 + 校验，不写库）。
    ///
    /// v1.5 B2：块 N 生成时注入上一块上文（`prior_context`），输出含 continuation。
    ///
    /// 参数:
    /// - `session_id`: 来源 session。
    /// - `chunk`: 当前块（其消息为对话原文）。
    /// - `prior_context`: 上一块的上文文本（None = 无上一块，v1.4 独立摘要路径）。
    ///
    /// 返回:
    /// - 校验后的 `(MemoryL1, 关键词列表)`（尚未写入存储，由调用方统一写库）。
    async fn generate_chunk_l1(
        &self,
        session_id: Uuid,
        chunk: &UttChunk,
        prior_context: Option<&str>,
    ) -> RamariaResult<(MemoryL1, Vec<KeywordToken>)> {
        // 1. 格式化当前块对话文本
        let conversation = self.format_conversation(&chunk.messages);

        // 2. 获取关键词候选
        let keyword_candidates = self.get_keyword_candidates().await;

        // 3. 构建 prompt（含上文注入时使用上下文感知模板）
        let prompt = build_l1_prompt(&conversation, keyword_candidates.as_deref(), prior_context);

        // 4. 调用 LLM
        let request_id = Uuid::new_v4();
        let llm_request = ChatRequest {
            system_prompt: String::new(),
            memory_context: None,
            history: vec![],
            user_message: prompt,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            request_id,
            template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
        };

        let raw_response = self.llm.chat(&llm_request).await.map_err(|e| {
            warn!(%session_id, %request_id, block_msg_count = chunk.msg_count, error=%e, "L1 块 LLM 调用失败");
            RamariaError::llm(format!(
                "session {session_id} L1 摘要生成 LLM 调用失败: {e}"
            ))
        })?;

        debug!(%session_id, %request_id, "LLM 返回 {} 字符", raw_response.len());

        // 5. 解析 JSON
        let parsed = self.parse_summary_json(&raw_response)?;

        // 6. 校验并修正字段
        let (mut l1, keywords) = Self::validate_and_build(&parsed, session_id);

        // 注入 config 中的上下文字段
        l1.persona_uid = self.config.persona_uid.clone();
        l1.context_json = self.config.context_json.clone();
        // 优先使用 LLM 输出的 situation_strength，缺失时回退 config 默认值
        l1.situation_strength = parsed
            .situation_strength
            .or(self.config.situation_strength)
            .or(Some(3)); // 最终默认值：中性情境

        // 7. continuation 校验（三选一；无上一块时强制 None——即使 LLM 输出）
        l1.continuation = if prior_context.is_some() {
            validate_continuation(parsed.continuation.as_deref(), session_id)
        } else {
            None
        };

        debug!(
            %session_id,
            block_msg_count = chunk.msg_count,
            continuation = ?l1.continuation,
            has_prior = prior_context.is_some(),
            "L1 块校验完成"
        );

        // 返回（不写库），关键词随调用方统一处理
        Ok((l1, keywords))
    }

    /// 写回关键词词典 + 倒排索引（每块成功后调用，失败记 warn 不阻塞）。
    async fn write_back_keywords(
        &self,
        session_id: Uuid,
        l1: &MemoryL1,
        keywords: &[KeywordToken],
    ) {
        for kw_token in keywords {
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
    /// 全部失败返回 Validation 错误（隐私红线：不包含原始响应内容，仅记长度供诊断）。
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
        // 隐私红线：LLM 原始响应不落日志，仅记录长度供诊断
        warn!(
            response_len = raw.chars().count(),
            "L1 摘要 JSON 解析全部失败（可能因 max_tokens 输出预算不足被截断）"
        );
        Err(RamariaError::validation(format!(
            "L1 摘要 JSON 解析失败，原始响应 {} 字符（不记录原文，防隐私泄漏）\
             （若响应不完整，可能是 max_tokens 输出预算不足导致截断）",
            raw.chars().count()
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
    /// - `evidence_notes`: 新增，后处理校验（非空数组 + 每条 ≥ 5 字符），
    ///   校验失败不阻塞 L1 生成，降级为空数组并记 warn 日志
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

        // evidence_notes: 后处理校验
        // 规则：非空数组 + 每条 trim 后 ≥ 5 字符
        // 校验失败不阻塞 L1 生成，降级为空数组并记 warn 日志
        let evidence_notes = validate_evidence_notes(parsed.evidence_notes.clone(), session_id);

        // continuation: 三选一校验（v1.5 B2），非法值置 None 不阻塞
        let continuation = validate_continuation(parsed.continuation.as_deref(), session_id);

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
            evidence_notes: Some(evidence_notes), // 始终为 Some(vec![])，存储层存为 JSON 数组
            continuation,
        };

        (l1, keywords_list)
    }
}

// =========================================================
// 消息格式化（自由函数，供块级生成与上文构建复用）
// =========================================================

/// 将消息列表格式化为对话文本（供 L1 摘要 prompt 使用）。
///
/// 格式:
/// - User 消息: `{user_prefix}{content}`
/// - Assistant 消息: `{assistant_prefix}{content}`
/// - System/Tool 消息: 跳过（不参与摘要）
fn format_messages(
    messages: &[ramaria_core::types::Message],
    user_prefix: &str,
    assistant_prefix: &str,
) -> String {
    let mut lines = Vec::with_capacity(messages.len());
    for msg in messages {
        let prefix = match msg.role {
            MessageRole::User => user_prefix,
            MessageRole::Assistant => assistant_prefix,
            // System/Tool 消息不进入摘要上下文
            _ => continue,
        };
        lines.push(format!("{prefix}{}", msg.content));
    }
    lines.join("\n")
}

// =========================================================
// B2 上下文感知：上一块上文构建（v1.5，§6.3 混合形态）
// =========================================================

/// 构建上一块的上文文本（只注入最近 1 块，不链式）。
///
/// 混合形态（§6.3）:
/// - 上一块消息数 ≤ `prior_context_threshold`（默认 20）→ 注入 L0 原文。
/// - 长块（消息数 > 阈值）→ 注入上一 L1 的摘要 + 结构化线索
///   （`evidence_notes`，含 time/who/cause 槽位）。
/// - 长块但上一 L1 不可用（生成失败降级）→ 回退注入上一块原文并截断到
///   `prior_context_max_chars`（默认 1500 字符），防止超长上文挤占输出预算。
///
/// 隐私: 原文仅作为 LLM prompt 上下文（与摘要生成同链路），不落日志。
fn build_prior_context(
    prev_chunk: &crate::utt::UttChunk,
    prev_l1: Option<&MemoryL1>,
    config: &L1SummarizerConfig,
    user_prefix: &str,
    assistant_prefix: &str,
) -> String {
    let is_long = (prev_chunk.msg_count as usize) > config.prior_context_threshold;

    // 短块 → 直接注入 L0 原文（原文信息量最大，无需 L1）
    if !is_long {
        return format_messages(&prev_chunk.messages, user_prefix, assistant_prefix);
    }

    // 长块 → 优先注入上一 L1 摘要 + 结构化线索
    if let Some(prev) = prev_l1 {
        let mut ctx = format!("[上一块摘要] {}", prev.summary);
        if let Some(notes) = prev.evidence_notes.as_ref().filter(|n| !n.is_empty()) {
            let lines: Vec<String> = notes
                .iter()
                .filter_map(|n| {
                    if n.text.trim().is_empty() {
                        None
                    } else {
                        Some(format!("- {}{}", n.text.trim(), slot_suffix(n)))
                    }
                })
                .collect();
            if !lines.is_empty() {
                ctx.push_str("\n[上一块线索]");
                ctx.push_str(&format!("\n{}", lines.join("\n")));
            }
        }
        return ctx;
    }

    // 长块且上一 L1 缺失（降级）→ 注入上一块原文并截断
    let raw = format_messages(&prev_chunk.messages, user_prefix, assistant_prefix);
    truncate_chars(&raw, config.prior_context_max_chars)
}

/// 构造 evidence 线索的可选槽位后缀（`（时间：... · 人物：... · 原因：...）`）。
fn slot_suffix(note: &ramaria_core::types::EvidenceNote) -> String {
    let mut parts = Vec::new();
    if let Some(t) = note.time.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("时间：{}", t.trim()));
    }
    if let Some(w) = note.who.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("人物：{}", w.trim()));
    }
    if let Some(c) = note.cause.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("原因：{}", c.trim()));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join(" · "))
    }
}

/// 按字符数截断字符串（保留 UTF-8 字符边界），超长时追加省略标记。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…（上文过长已截断）")
}

/// 校验 LLM 输出的 continuation 枚举值。
///
/// 规则（§6.3）:
/// - 合法值三选一：`延续` / `转折` / `无关`，返回校验后的值。
/// - 缺失（None）→ None（正常：LLM 未输出或 prompt 无该字段）。
/// - 非法值 → 置 None 并记 warn（不阻塞生成）。
fn validate_continuation(raw: Option<&str>, session_id: Uuid) -> Option<String> {
    match raw.map(str::trim) {
        Some("延续") | Some("转折") | Some("无关") => raw.map(str::trim).map(str::to_string),
        Some(other) if !other.is_empty() => {
            warn!(%session_id, value = %other, "非法的 continuation 值，置为 None");
            None
        }
        _ => None,
    }
}

// =========================================================
// evidence_notes 宽容反序列化 + 校验
// =========================================================/// 宽容反序列化 evidence_notes（v1.4 结构化升级的过渡兼容）。
///
/// 兼容三种输入:
/// 1. 对象数组 `[{"text": "...", "time": ..., "who": ..., "cause": ...}]` — 直接解析
/// 2. 旧字符串数组 `["...", "..."]` — 字符串落 `text` 槽位，其余置空
/// 3. 缺失 / null / 非数组 — 返回 None
///
/// 说明:
/// - 存储层（memory_l1 表）格式约定：只读写新格式，无旧格式解析分支（见 docs/dev-1.5/v1.5-decisions.md）；
///   此处的宽容解析仅针对 LLM 输出（M4 之前 prompt 仍为旧格式）。
fn deserialize_evidence_notes<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<EvidenceNote>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        _ => return Ok(None),
    };

    let mut notes = Vec::with_capacity(items.len());
    for item in items {
        match item {
            // 旧格式：字符串 → text 槽位
            serde_json::Value::String(s) => notes.push(EvidenceNote::new(s)),
            // 新格式：对象 → 结构化解析（缺失字段回退 None）
            serde_json::Value::Object(_) => match serde_json::from_value::<EvidenceNote>(item) {
                Ok(note) => notes.push(note),
                Err(e) => {
                    tracing::warn!(error = %e, "evidence_notes 条目解析失败，跳过该条");
                }
            },
            other => {
                tracing::warn!(
                    kind = %other,
                    "evidence_notes 条目类型非法（应为字符串或对象），跳过该条"
                );
            }
        }
    }
    Ok(Some(notes))
}

/// 校验 evidence_notes 字段（v1.4 结构化格式）。
///
/// 校验规则:
/// 1. 输入为 None 或空数组 → 降级为空数组，记 warn
/// 2. 每条 evidence 的 `text` trim 后 < 5 字符 → 丢弃该条，记 debug
/// 3. 可选槽位 `time`/`who`/`cause` 规范化：trim 后为空字符串视为缺失 → 置 None
///    （v3.1 §6.3：cause 缺失时槽位置空，不阻塞生成）
/// 4. 丢弃后数组为空 → 降级为空数组，记 warn
///
/// 返回:
/// - `Vec<EvidenceNote>`：经过滤的有效 evidence 列表（可能为空）
fn validate_evidence_notes(raw: Option<Vec<EvidenceNote>>, session_id: Uuid) -> Vec<EvidenceNote> {
    let raw_list = match raw {
        Some(list) if !list.is_empty() => list,
        _ => {
            warn!(%session_id, "LLM 未产出 evidence_notes 或为空数组，降级为空");
            return vec![];
        }
    };

    // 过滤过短条目（校验 text 槽位）+ 规范化可选槽位（空白视为缺失）
    let valid: Vec<EvidenceNote> = raw_list
        .into_iter()
        .map(|mut note| {
            // text 必填槽位：trim 后参与长度校验
            note.text = note.text.trim().to_string();
            // 可选槽位：trim 后为空字符串 → 置 None（保持"缺省即无"的语义，
            // 避免下游把空字符串误当作有效槽位值）
            note.time = normalize_optional_slot(note.time);
            note.who = normalize_optional_slot(note.who);
            note.cause = normalize_optional_slot(note.cause);
            note
        })
        .filter(|note| {
            let ok = note.text.chars().count() >= 5;
            if !ok {
                // 隐私红线：evidence_notes 承载原文级信息，日志只记长度不记内容
                debug!(%session_id, len = note.text.chars().count(), "evidence 过短（<5 字符），丢弃");
            }
            ok
        })
        .collect();

    if valid.is_empty() {
        warn!(%session_id, "所有 evidence_notes 条目均不满足最小长度要求，降级为空");
    }

    valid
}

/// 规范化可选槽位值：trim 后为空字符串（或仅空白）视为缺失 → None。
///
/// 说明:
/// - LLM 可能输出 `"time": ""` 或 `"cause": "  "` 这类空值，
///   与缺失槽位（JSON 省略该键）语义等价，统一归一为 None。
/// - 非空值保留 trim 后的内容，避免首尾空白污染下游消费。
fn normalize_optional_slot(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// =========================================================
// 纯函数辅助
// =========================================================

/// 解析关键词字符串为 `(存储用的逗号分隔字符串, 标准化关键词列表)`。
///
/// 如果输入为空或仅含空白字符，返回 `(None, vec![])`。
/// 返回 `Vec<KeywordToken>` 替代裸 `String`。
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
            // 存储时使用逗号分隔字符串
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
    use crate::l1::mock::{MockStorage, make_msg};
    use ramaria_core::types::{EvidenceNote, Message, MessageRole};

    // ---- strip_thinking（与 utils.rs 同名测试完全重复，已删除） ----

    /// v1.4 截断修复：默认 max_tokens 应足以容纳含 evidence_notes 的完整 JSON。
    ///
    /// 说明:
    /// - 512（Python 旧值）对 v1.4 结构化对象数组输出过紧，LLM 输出易被截断
    ///   导致 JSON 解析失败；默认值提升至 1024 作为所有未显式传值路径的兜底。
    #[test]
    fn default_config_max_tokens_sufficient() {
        let cfg = L1SummarizerConfig::default();
        assert_eq!(cfg.max_tokens, 1024, "L1 默认 max_tokens 应为 1024");
        assert!(
            (cfg.temperature - 0.3).abs() < f64::EPSILON,
            "temperature 默认 0.3"
        );
    }

    // ---- extract_first_json_object ----

    #[test]
    fn extract_with_markdown_block() {
        let input = "```json\n{\"summary\": \"测试\"}\n```";
        let result = crate::utils::extract_first_json_object(input).unwrap();
        assert!(result.contains("\"summary\""));
    }

    // ---- clamp_valence（与 utils.rs 同名测试完全重复，已删除） ----

    #[test]
    fn clamp_valence_boundary() {
        let result = crate::utils::clamp_valence(0.25);
        assert!(result == 0.0 || result == 0.5);
    }

    // ---- clamp_salience（与 utils.rs 同名测试完全重复，已删除） ----

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
            evidence_notes: None,
            continuation: None,
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
            evidence_notes: None,
            continuation: None,
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
            evidence_notes: None,
            continuation: None,
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
            evidence_notes: None,
            continuation: None,
        };
        let sid = ramaria_core::types::new_id();
        let (_l1, keywords) = L1Summarizer::validate_and_build(&parsed, sid);
        assert_eq!(keywords.len(), 3);
        assert!(keywords.contains(&KeywordToken::new("工作").unwrap()));
        assert!(keywords.contains(&KeywordToken::new("学习").unwrap()));
        assert!(keywords.contains(&KeywordToken::new("编程").unwrap()));
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
    fn validate_and_build_does_not_inject_situation_strength() {
        // validate_and_build 只负责字段校验，situation_strength 的注入
        // 由调用方 generate_chunk_l1 完成（LLM 输出 > config > 默认 3）。
        // 此处验证注入不在此层发生：无论 LLM 是否输出该字段，
        // validate_and_build 产出的 L1 均为 None。
        for llm_value in [Some(5), None] {
            let parsed = L1SummaryResponse {
                summary: Some("测试摘要".into()),
                keywords: None,
                time_period: Some("上午".into()),
                atmosphere: Some("轻松".into()),
                valence: Some(0.5),
                salience: Some(0.5),
                situation_strength: llm_value,
                evidence_notes: None,
                continuation: None,
            };
            let sid = ramaria_core::types::new_id();
            let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
            assert_eq!(
                l1.situation_strength, None,
                "validate_and_build 不应注入 situation_strength（LLM 输入 {llm_value:?}）"
            );
        }
    }

    /// 真实注入路径（generate_chunk_l1 步骤 7）：
    /// LLM 输出 > config 回退 > 默认 3。
    #[tokio::test]
    async fn summarize_session_injects_situation_strength_priority() {
        use crate::l1::mock::MockLlmProvider;

        // 场景 A：LLM 输出 situation_strength=5 → 优先采用
        let sid_a = Uuid::new_v4();
        let storage_a = MockStorage::new();
        storage_a.add_messages(
            sid_a,
            vec![
                make_msg(sid_a, MessageRole::User, "最近压力好大"),
                make_msg(sid_a, MessageRole::Assistant, "辛苦了，早点休息"),
            ],
        );
        let llm_a = MockLlmProvider::new("test-model");
        llm_a.set_response(
            serde_json::json!({
                "summary": "测试摘要",
                "keywords": "压力",
                "time_period": "上午",
                "atmosphere": "平静",
                "valence": -0.4,
                "salience": 0.5,
                "situation_strength": 5,
                "evidence_notes": []
            })
            .to_string(),
        );
        let summarizer_a = L1Summarizer::new(
            &llm_a,
            &storage_a,
            L1SummarizerConfig {
                utt_splitter: None,
                ..Default::default()
            },
        );
        summarizer_a
            .summarize_session(sid_a)
            .await
            .expect("场景 A 应成功");
        assert_eq!(
            storage_a.saved_l1_entries()[0].situation_strength,
            Some(5),
            "LLM 输出优先于 config 与默认值"
        );

        // 场景 B：LLM 缺失 + config=Some(2) → 回退 config
        let sid_b = Uuid::new_v4();
        let storage_b = MockStorage::new();
        storage_b.add_messages(
            sid_b,
            vec![
                make_msg(sid_b, MessageRole::User, "最近压力好大"),
                make_msg(sid_b, MessageRole::Assistant, "辛苦了，早点休息"),
            ],
        );
        let llm_b = MockLlmProvider::new("test-model");
        llm_b.set_response(llm_json("测试摘要", None)); // 无 situation_strength
        let summarizer_b = L1Summarizer::new(
            &llm_b,
            &storage_b,
            L1SummarizerConfig {
                situation_strength: Some(2),
                utt_splitter: None,
                ..Default::default()
            },
        );
        summarizer_b
            .summarize_session(sid_b)
            .await
            .expect("场景 B 应成功");
        assert_eq!(
            storage_b.saved_l1_entries()[0].situation_strength,
            Some(2),
            "LLM 缺失时应回退 config 值"
        );

        // 场景 C：LLM 缺失 + config=None → 默认 3
        let sid_c = Uuid::new_v4();
        let storage_c = MockStorage::new();
        storage_c.add_messages(
            sid_c,
            vec![
                make_msg(sid_c, MessageRole::User, "最近压力好大"),
                make_msg(sid_c, MessageRole::Assistant, "辛苦了，早点休息"),
            ],
        );
        let llm_c = MockLlmProvider::new("test-model");
        llm_c.set_response(llm_json("测试摘要", None));
        let summarizer_c = L1Summarizer::new(
            &llm_c,
            &storage_c,
            L1SummarizerConfig {
                utt_splitter: None,
                ..Default::default()
            },
        );
        summarizer_c
            .summarize_session(sid_c)
            .await
            .expect("场景 C 应成功");
        assert_eq!(
            storage_c.saved_l1_entries()[0].situation_strength,
            Some(3),
            "LLM 与 config 均缺失时回退默认 3"
        );
    }

    // =========================================================
    // evidence_notes 校验测试
    // =========================================================

    #[test]
    fn evidence_notes_valid_list_is_preserved() {
        // 正常产出证据片段 → 保留全部有效条目
        let notes = vec![
            EvidenceNote::new("用户表示最近一个月每天加班到10点以后"),
            EvidenceNote::new("用户说'感觉身体被掏空了'"),
            EvidenceNote::new("用户提到'周末也经常被叫去开会'"),
        ];
        let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
        assert_eq!(result.len(), 3);
        assert!(result[0].text.contains("加班"));
    }

    #[test]
    fn evidence_notes_null_downgrades_to_empty() {
        // LLM 未输出 evidence_notes → 降级为空数组
        let result = validate_evidence_notes(None, Uuid::new_v4());
        assert!(result.is_empty(), "evidence_notes 为 None 时应降级为空数组");
    }

    #[test]
    fn evidence_notes_empty_array_downgrades_to_empty() {
        // LLM 输出空数组 → 降级为空数组
        let result = validate_evidence_notes(Some(vec![]), Uuid::new_v4());
        assert!(result.is_empty(), "evidence_notes 为空数组时应降级为空数组");
    }

    #[test]
    fn evidence_notes_short_items_are_filtered() {
        // 过短条目（< 5 字符）应被丢弃
        let notes = vec![
            EvidenceNote::new("太长的一条完整证据描述文本"),
            EvidenceNote::new("短"), // < 5 字符，应丢弃
            EvidenceNote::new("OK"), // < 5 字符，应丢弃
            EvidenceNote::new("足够长的证据描述文本内容"),
        ];
        let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
        assert_eq!(result.len(), 2);
        assert!(result[0].text.contains("太长"));
        assert!(result[1].text.contains("足够"));
    }

    #[test]
    fn evidence_notes_all_short_downgrades_to_empty() {
        // 全部条目过短 → 降级为空数组
        let notes = vec![
            EvidenceNote::new("短"),
            EvidenceNote::new("A"),
            EvidenceNote::new("B"),
        ];
        let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
        assert!(result.is_empty(), "全部 evidence 过短时应降级为空数组");
    }

    #[test]
    fn evidence_notes_parse_from_valid_json() {
        // JSON 解析：包含 evidence_notes 数组（旧字符串数组 → 宽容转换为对象）
        let raw = r#"{
            "summary": "测试",
            "valence": 0.0,
            "salience": 0.5,
            "evidence_notes": ["证据一：用户提到项目延期", "证据二：用户表示压力很大"]
        }"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        let notes = parsed.evidence_notes.unwrap();
        assert_eq!(notes.len(), 2);
        assert!(notes[0].text.contains("项目延期"));
    }

    #[test]
    fn evidence_notes_parse_structured_object_array() {
        // JSON 解析：对象数组（v1.4 新格式）直接解析为结构化 EvidenceNote
        let raw = r#"{
            "summary": "测试",
            "valence": 0.0,
            "salience": 0.5,
            "evidence_notes": [
                {"text": "用户提到项目延期", "time": "上周三", "who": "用户", "cause": "需求变更"}
            ]
        }"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        let notes = parsed.evidence_notes.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "用户提到项目延期");
        assert_eq!(notes[0].time.as_deref(), Some("上周三"));
        assert_eq!(notes[0].who.as_deref(), Some("用户"));
        assert_eq!(notes[0].cause.as_deref(), Some("需求变更"));
    }

    #[test]
    fn evidence_notes_parse_mixed_items() {
        // JSON 解析：混合旧字符串与对象条目 → 全部转换为 EvidenceNote
        let raw = r#"{
            "summary": "测试",
            "evidence_notes": ["旧格式字符串", {"text": "新格式对象"}]
        }"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        let notes = parsed.evidence_notes.unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "旧格式字符串");
        assert!(notes[0].time.is_none());
        assert_eq!(notes[1].text, "新格式对象");
    }

    #[test]
    fn evidence_notes_parse_null_array_defaults_none() {
        // JSON 中 evidence_notes 为 null → 返回 None（降级路径）
        let raw = r#"{"summary": "测试", "evidence_notes": null}"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        assert!(parsed.evidence_notes.is_none());
    }

    #[test]
    fn evidence_notes_parse_missing_field_defaults_none() {
        // JSON 缺失 evidence_notes 字段 → serde(default) 应返回 None
        let raw = r#"{"summary": "测试", "valence": 0.0, "salience": 0.5}"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        assert!(parsed.evidence_notes.is_none());
    }

    #[test]
    fn validate_and_build_evidence_notes_present() {
        // validate_and_build 整合测试：正常 evidence_notes 应保留
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("上午".into()),
            atmosphere: Some("专注".into()),
            valence: Some(0.0),
            salience: Some(0.5),
            situation_strength: None,
            evidence_notes: Some(vec![EvidenceNote::new("用户提到项目截止日期临近")]),
            continuation: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        let notes = l1.evidence_notes.expect("evidence_notes 不应为 None");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].text.contains("项目截止日期"));
    }

    #[test]
    fn validate_and_build_evidence_notes_missing_downgrades() {
        // validate_and_build 整合测试：缺失 evidence_notes 降级为空数组
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("上午".into()),
            atmosphere: Some("轻松".into()),
            valence: Some(0.5),
            salience: Some(0.5),
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        let notes = l1
            .evidence_notes
            .expect("evidence_notes 不应为 None，应为 Some(vec![])");
        assert!(notes.is_empty(), "缺失 evidence_notes 时应降级为空数组");
    }

    // ---- 结构化槽位校验测试 ----

    /// 完整对象（text + time/who/cause 全部槽位）经校验后槽位完整保留。
    #[test]
    fn evidence_notes_full_object_slots_preserved() {
        let notes = vec![EvidenceNote {
            text: "用户提到项目延期到月底".into(),
            time: Some("上周三".into()),
            who: Some("用户".into()),
            cause: Some("需求变更频繁".into()),
        }];
        let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "用户提到项目延期到月底");
        assert_eq!(result[0].time.as_deref(), Some("上周三"));
        assert_eq!(result[0].who.as_deref(), Some("用户"));
        assert_eq!(result[0].cause.as_deref(), Some("需求变更频繁"));
    }

    /// 可选槽位为空字符串或纯空白 → 归一为 None（缺省即无，不阻塞生成）。
    #[test]
    fn evidence_notes_blank_optional_slots_normalized_to_none() {
        let notes = vec![EvidenceNote {
            text: "用户表示最近压力很大".into(),
            time: Some("".into()),   // 空字符串
            who: Some("   ".into()), // 纯空白
            cause: Some("".into()),  // 空字符串
        }];
        let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
        assert_eq!(result.len(), 1, "text 有效时条目应保留");
        assert!(result[0].time.is_none(), "空 time 应归一为 None");
        assert!(result[0].who.is_none(), "空白 who 应归一为 None");
        assert!(result[0].cause.is_none(), "空 cause 应归一为 None");
    }

    /// 可选槽位带首尾空白 → trim 后保留有效内容。
    #[test]
    fn evidence_notes_optional_slots_are_trimmed() {
        let notes = vec![EvidenceNote {
            text: "用户提到通勤时间变长".into(),
            time: Some(" 上周五 ".into()),
            who: Some(" 同事 ".into()),
            cause: Some(" 搬家 ".into()),
        }];
        let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
        assert_eq!(result[0].time.as_deref(), Some("上周五"));
        assert_eq!(result[0].who.as_deref(), Some("同事"));
        assert_eq!(result[0].cause.as_deref(), Some("搬家"));
    }

    /// 反序列化：对象条目缺少 text（如 text 为数字等非法类型）→ 跳过该条并记 warn，
    /// 其余合法条目保留（解析失败不阻塞整体）。
    #[test]
    fn evidence_notes_parse_invalid_object_item_skipped() {
        let raw = r#"{
            "summary": "测试",
            "evidence_notes": [
                {"text": 123, "cause": "非法类型"},
                {"text": "用户提到项目顺利上线"}
            ]
        }"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        let notes = parsed.evidence_notes.expect("应产出部分有效条目");
        assert_eq!(notes.len(), 1, "非法条目应被跳过，合法条目保留");
        assert_eq!(notes[0].text, "用户提到项目顺利上线");
    }

    /// 反序列化：非字符串非对象的非法条目（数字/布尔）→ 跳过该条。
    #[test]
    fn evidence_notes_parse_non_object_items_skipped() {
        let raw = r#"{
            "summary": "测试",
            "evidence_notes": [42, true, "用户提到天气转凉"]
        }"#;
        let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
        let notes = parsed.evidence_notes.expect("应产出部分有效条目");
        assert_eq!(notes.len(), 1, "数字/布尔条目应被跳过");
        assert_eq!(notes[0].text, "用户提到天气转凉");
    }

    // =========================================================
    // summarize_session 集成测试
    // =========================================================

    /// 测试 summarize_session 完整流程：消息→格式化→mock LLM→解析→校验→存储。
    #[tokio::test]
    async fn summarize_session_integration_basic() {
        use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
        use ramaria_core::types::MessageRole;
        use uuid::Uuid;

        let session_id = Uuid::new_v4();

        // 准备 mock 存储：3 条对话消息
        let storage = MockStorage::new();
        storage.add_messages(
            session_id,
            vec![
                make_msg(session_id, MessageRole::User, "今天天气真不错"),
                make_msg(session_id, MessageRole::Assistant, "是啊，适合出去走走"),
                make_msg(session_id, MessageRole::User, "不过最近工作有点累"),
            ],
        );
        storage.set_keywords(vec!["天气".into(), "工作".into(), "疲惫".into()]);

        // 准备 mock LLM：返回有效 JSON（使用 serde_json 构造确保格式正确）
        let llm = MockLlmProvider::new("test-model");
        let response_json = serde_json::json!({
            "summary": "用户和助手聊了天气和最近的工作状态",
            "keywords": "天气,工作压力,日常闲聊",
            "time_period": "上午",
            "atmosphere": "轻松闲聊",
            "valence": 0.3,
            "salience": 0.5,
            "evidence_notes": ["用户说天气不错", "用户提到最近工作有点累"]
        });
        llm.set_response(response_json.to_string());

        let config = L1SummarizerConfig {
            persona_uid: Some("test-persona".into()),
            context_json: None,
            situation_strength: None,
            temperature: 0.3,
            max_tokens: 2048,
            user_prefix: "用户：".into(),
            assistant_prefix: "助手：".into(),
            utt_splitter: None,
            prior_context_threshold: 20,
            prior_context_max_chars: 1500,
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);

        let result = summarizer.summarize_session(session_id).await;
        assert!(
            result.is_ok(),
            "summarize_session 应成功: {:?}",
            result.err()
        );

        let l1 = result.unwrap();
        assert_eq!(l1.persona_uid, Some("test-persona".into()));
        assert!(l1.summary.contains("天气"), "摘要应包含天气相关内容");
        assert!(
            !l1.evidence_notes.as_ref().unwrap().is_empty(),
            "evidence_notes 不应为空"
        );

        // 验证存储写入
        let saved = storage.saved_l1_entries();
        assert_eq!(saved.len(), 1, "应保存 1 条 L1 记录");
        assert!(storage.keyword_count() >= 1, "应写入至少 1 个关键词");
    }

    /// 测试空消息 session 返回错误。
    #[tokio::test]
    async fn summarize_session_empty_messages_errors() {
        use crate::l1::mock::{MockLlmProvider, MockStorage};
        use uuid::Uuid;

        let session_id = Uuid::new_v4();
        let storage = MockStorage::new();
        let llm = MockLlmProvider::new("test-model");

        let config = L1SummarizerConfig {
            persona_uid: None,
            context_json: None,
            situation_strength: None,
            temperature: 0.3,
            max_tokens: 2048,
            user_prefix: "用户：".into(),
            assistant_prefix: "助手：".into(),
            utt_splitter: None,
            prior_context_threshold: 20,
            prior_context_max_chars: 1500,
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(session_id).await;
        assert!(result.is_err(), "空消息 session 应返回错误");
    }

    /// 测试 LLM 返回 JSON 中 evidence_notes 缺失时降级。
    #[tokio::test]
    async fn summarize_session_missing_evidence_notes_degrades() {
        use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
        use ramaria_core::types::MessageRole;
        use uuid::Uuid;

        let session_id = Uuid::new_v4();
        let storage = MockStorage::new();
        storage.add_messages(
            session_id,
            vec![make_msg(session_id, MessageRole::User, "测试消息")],
        );

        let llm = MockLlmProvider::new("test-model");
        // 不包含 evidence_notes 字段
        let response_json = serde_json::json!({
            "summary": "一条测试消息",
            "keywords": "测试",
            "time_period": "未知",
            "atmosphere": "中性",
            "valence": 0.0,
            "salience": 0.3
        });
        llm.set_response(response_json.to_string());

        let config = L1SummarizerConfig {
            persona_uid: None,
            context_json: None,
            situation_strength: None,
            temperature: 0.3,
            max_tokens: 2048,
            user_prefix: "用户：".into(),
            assistant_prefix: "助手：".into(),
            utt_splitter: None,
            prior_context_threshold: 20,
            prior_context_max_chars: 1500,
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(session_id).await;
        assert!(
            result.is_ok(),
            "缺少 evidence_notes 不应阻塞流程: {:?}",
            result.err()
        );

        let l1 = result.unwrap();
        // evidence_notes 缺失时降级为空数组
        let notes = l1.evidence_notes.expect("evidence_notes 应为 Some");
        assert!(notes.is_empty(), "缺失 evidence_notes 时应降级为空数组");
    }

    // =========================================================
    // B2 上下文感知生成测试
    // =========================================================

    use crate::utt::{UttChunk, UttSplitterConfig};

    /// 构造带 persona_uid 的 assistant 消息（目标发言）。
    fn target_msg(session_id: Uuid, created_at: i64, content: &str) -> Message {
        let mut m = make_msg(session_id, MessageRole::Assistant, content);
        m.created_at = created_at;
        m.persona_uid = Some("char-0001".to_string());
        m
    }

    /// 构造用户消息（非目标侧）。
    fn user_msg(session_id: Uuid, created_at: i64, content: &str) -> Message {
        let mut m = make_msg(session_id, MessageRole::User, content);
        m.created_at = created_at;
        m
    }

    /// 构造一个消息块。
    fn make_chunk(msgs: Vec<Message>) -> UttChunk {
        UttChunk::from_messages(msgs)
    }

    /// 构造带 continuation 的 MemoryL1（供 build_prior_context 测试）。
    fn make_l1(summary: &str, notes: Vec<EvidenceNote>) -> MemoryL1 {
        MemoryL1 {
            id: ramaria_core::types::new_id(),
            session_id: ramaria_core::types::new_id(),
            summary: summary.to_string(),
            keywords: None,
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: 0,
            last_accessed_at: None,
            persona_uid: Some("char-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: Some(notes),
            continuation: Some("延续".to_string()),
        }
    }

    // ---- build_prior_context：两种上文形态 ----

    /// 短块（消息数 ≤ 阈值）→ 注入 L0 原文（混合形态之一）。
    #[test]
    fn prior_context_short_block_injects_raw_text() {
        let sid = Uuid::new_v4();
        let chunk = make_chunk(vec![
            user_msg(sid, 1000, "今天工作好累"),
            target_msg(sid, 2000, "辛苦了，早点休息"),
        ]);
        let cfg = L1SummarizerConfig::default();
        let ctx = build_prior_context(
            &chunk,
            Some(&make_l1("摘要", vec![])),
            &cfg,
            "用户：",
            "助手：",
        );
        // 短块即使有 L1 也注入原文
        assert!(ctx.contains("今天工作好累"), "应注入短块原文");
        assert!(ctx.contains("辛苦了，早点休息"), "应注入短块原文");
        assert!(!ctx.contains("[上一块摘要]"), "短块不应注入摘要形态");
    }

    /// 长块 + 上一 L1 → 注入摘要 + 结构化线索（含可选槽位）。
    #[test]
    fn prior_context_long_block_injects_summary_and_notes() {
        let sid = Uuid::new_v4();
        // 21 条消息（> 阈值 20）→ 长块
        let msgs: Vec<Message> = (0..21)
            .map(|i| {
                if i % 2 == 0 {
                    target_msg(sid, 1000 + i * 1000, &format!("target 消息 {i}"))
                } else {
                    user_msg(sid, 1000 + i * 1000, &format!("user 消息 {i}"))
                }
            })
            .collect();
        let chunk = make_chunk(msgs);
        let prev_l1 = make_l1(
            "用户抱怨项目延期",
            vec![EvidenceNote {
                text: "用户提到项目延期到月底".into(),
                time: Some("上周三".into()),
                who: Some("用户".into()),
                cause: Some("需求变更频繁".into()),
            }],
        );
        let cfg = L1SummarizerConfig::default();
        let ctx = build_prior_context(&chunk, Some(&prev_l1), &cfg, "用户：", "助手：");
        assert!(ctx.contains("[上一块摘要] 用户抱怨项目延期"), "应注入摘要");
        assert!(ctx.contains("用户提到项目延期到月底"), "应注入线索 text");
        assert!(ctx.contains("时间：上周三"), "线索可选槽位应保留");
        assert!(ctx.contains("人物：用户"), "线索 who 槽位应保留");
        assert!(ctx.contains("原因：需求变更频繁"), "线索 cause 槽位应保留");
    }

    /// 长块 + 上一 L1（无 evidence_notes）→ 仅注入摘要，不报错。
    #[test]
    fn prior_context_long_block_l1_without_notes_injects_summary_only() {
        let sid = Uuid::new_v4();
        let msgs: Vec<Message> = (0..21)
            .map(|i| user_msg(sid, 1000 + i * 1000, "消息"))
            .collect();
        let chunk = make_chunk(msgs);
        let prev_l1 = make_l1("用户聊了天气", vec![]);
        let cfg = L1SummarizerConfig::default();
        let ctx = build_prior_context(&chunk, Some(&prev_l1), &cfg, "用户：", "助手：");
        assert!(ctx.contains("[上一块摘要] 用户聊了天气"));
        assert!(!ctx.contains("[上一块线索]"), "无线索时不应输出线索段落");
    }

    /// 长块无上一 L1（降级）→ 注入上一块原文并截断到上限。
    #[test]
    fn prior_context_long_block_without_l1_truncates_raw() {
        let sid = Uuid::new_v4();
        let msgs: Vec<Message> = (0..21)
            .map(|i| {
                user_msg(
                    sid,
                    1000 + i * 1000,
                    "这是一条足够长的用户消息内容用于截断测试",
                )
            })
            .collect();
        let chunk = make_chunk(msgs);
        let cfg = L1SummarizerConfig {
            prior_context_max_chars: 100,
            ..Default::default()
        };
        let ctx = build_prior_context(&chunk, None, &cfg, "用户：", "助手：");
        assert!(ctx.contains("…（上文过长已截断）"), "应含截断标记");
        assert!(
            ctx.chars().count() <= 100 + "…（上文过长已截断）".chars().count() + 1,
            "截断后长度受控: {}",
            ctx.chars().count()
        );
    }

    /// 消息数恰好等于阈值 → 短块形态（原文）；超过阈值 → 长块形态（L1）。
    #[test]
    fn prior_context_threshold_boundary() {
        let sid = Uuid::new_v4();
        let cfg = L1SummarizerConfig::default();
        // 恰 20 条（= 阈值）→ 原文
        let msgs: Vec<Message> = (0..20)
            .map(|i| user_msg(sid, 1000 + i * 1000, "内容"))
            .collect();
        let chunk = make_chunk(msgs.clone());
        let ctx = build_prior_context(
            &chunk,
            Some(&make_l1("摘要", vec![])),
            &cfg,
            "用户：",
            "助手：",
        );
        assert!(!ctx.contains("[上一块摘要]"), "= 阈值仍为短块原文形态");
        // 21 条（> 阈值）→ L1 摘要形态
        let mut msgs2: Vec<Message> = msgs;
        msgs2.push(user_msg(sid, 1000 + 20 * 1000, "内容"));
        let chunk2 = make_chunk(msgs2);
        let ctx2 = build_prior_context(
            &chunk2,
            Some(&make_l1("摘要", vec![])),
            &cfg,
            "用户：",
            "助手：",
        );
        assert!(ctx2.contains("[上一块摘要]"), "超过阈值应注入 L1 摘要");
    }

    // ---- validate_continuation ----

    /// 三个合法枚举值均保留（trim 后）。
    #[test]
    fn continuation_valid_values_kept() {
        for (raw, expected) in [("延续", "延续"), (" 转折 ", "转折"), ("无关", "无关")]
        {
            let v = validate_continuation(Some(raw), Uuid::new_v4());
            assert_eq!(v.as_deref(), Some(expected), "值 {raw} 应保留");
        }
    }

    /// 非法值 → 置 None 不阻塞。
    #[test]
    fn continuation_invalid_value_dropped() {
        let v = validate_continuation(Some("延续中"), Uuid::new_v4());
        assert!(v.is_none(), "非法 continuation 应置 None");
        let v2 = validate_continuation(Some("cont"), Uuid::new_v4());
        assert!(v2.is_none());
    }

    /// 缺失/空白 → None（正常路径）。
    #[test]
    fn continuation_missing_or_blank_dropped() {
        assert!(validate_continuation(None, Uuid::new_v4()).is_none());
        assert!(validate_continuation(Some("   "), Uuid::new_v4()).is_none());
        assert!(validate_continuation(Some(""), Uuid::new_v4()).is_none());
    }

    // ---- summarize_session 集成（多块上下文感知） ----

    /// 构造一个 2 块的 session：块1 与块2 间隔 > θ_gap（30 分钟）。
    fn two_block_session(sid: Uuid) -> Vec<Message> {
        // 块1：2 条（短块），时间 0 ~ 1000
        let mut msgs = vec![
            user_msg(sid, 0, "块1：用户开场"),
            target_msg(sid, 1000, "块1：助手回应"),
        ];
        // 间隙 > 30 分钟（θ_gap=30）→ 块2
        let t2 = 31 * 60_000;
        msgs.push(user_msg(sid, t2, "块2：用户继续提问"));
        msgs.push(target_msg(sid, t2 + 1000, "块2：助手回复"));
        msgs
    }

    /// 构造带 continuation 的 mock LLM 响应 JSON。
    fn llm_json(summary: &str, continuation: Option<&str>) -> String {
        let mut obj = serde_json::json!({
            "summary": summary,
            "keywords": "测试,关键词",
            "time_period": "上午",
            "atmosphere": "平静",
            "valence": 0.0,
            "salience": 0.5,
            "evidence_notes": []
        });
        if let Some(c) = continuation {
            obj["continuation"] = serde_json::json!(c);
        }
        obj.to_string()
    }

    /// 多块 session → 每块生成一条 L1；第二块带 continuation（有上文）。
    #[tokio::test]
    async fn multi_block_generates_one_l1_per_block_with_continuation() {
        use crate::l1::mock::MockLlmProvider;

        let sid = Uuid::new_v4();
        let storage = MockStorage::new();
        storage.add_messages(sid, two_block_session(sid));

        let llm = MockLlmProvider::new("test-model");
        // 块1 无上文 → 无 continuation；块2 有上文 → continuation="延续"
        llm.set_responses(vec![
            llm_json("块1 摘要", None),
            llm_json("块2 摘要（延续上一话题）", Some("延续")),
        ]);

        let config = L1SummarizerConfig {
            persona_uid: Some("char-0001".into()),
            utt_splitter: Some(UttSplitterConfig {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
            }),
            ..Default::default()
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(sid).await;
        assert!(result.is_ok(), "多块生成应成功: {:?}", result.err());

        let saved = storage.saved_l1_entries();
        assert_eq!(saved.len(), 2, "每块应生成一条 L1");
        assert_eq!(saved[0].summary, "块1 摘要");
        assert!(
            saved[0].continuation.is_none(),
            "首块无上文 → continuation=None"
        );
        assert_eq!(saved[1].summary, "块2 摘要（延续上一话题）");
        assert_eq!(
            saved[1].continuation.as_deref(),
            Some("延续"),
            "第二块应带 continuation"
        );

        // 返回值为最后一块的 L1
        let l1 = result.unwrap();
        assert_eq!(l1.summary, "块2 摘要（延续上一话题）");
    }

    /// 第二块生成时 prompt 注入上一块原文（短块形态）；只注入最近 1 块。
    #[tokio::test]
    async fn second_block_prompt_includes_prior_block_raw() {
        use crate::l1::mock::MockLlmProvider;

        let sid = Uuid::new_v4();
        let storage = MockStorage::new();
        storage.add_messages(sid, two_block_session(sid));

        let llm = MockLlmProvider::new("test-model");
        llm.set_responses(vec![
            llm_json("块1 摘要", None),
            llm_json("块2 摘要", Some("无关")),
        ]);

        let config = L1SummarizerConfig {
            persona_uid: Some("char-0001".into()),
            utt_splitter: Some(UttSplitterConfig {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
            }),
            ..Default::default()
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        summarizer.summarize_session(sid).await.expect("应成功");

        // 最后一次请求 = 块2：prompt 应含块1 原文与 continuation 字段说明
        let last = llm.last_request().expect("应有请求记录");
        assert!(
            last.user_message.contains("块1：用户开场"),
            "应注入块1 原文"
        );
        assert!(
            last.user_message.contains("块1：助手回应"),
            "应注入块1 原文"
        );
        assert!(
            last.user_message.contains("continuation"),
            "带上文模板应含 continuation"
        );
        // 只注入最近 1 块：块2 原文是当前块内容，应出现在块2 的 prompt 对话部分
        //（上文注入的是块1，不含第三块链式内容）
        assert!(
            last.user_message.contains("块2：用户继续提问"),
            "块2 原文是当前块内容，应出现在块2 prompt 中"
        );
    }

    /// 单块 session（无上一块）→ 与 v1.4 行为一致：一条 L1、continuation=None、
    /// prompt 为 v1.4 模板（不含 continuation 字段）。
    #[tokio::test]
    async fn single_block_session_matches_v1_4_behavior() {
        use crate::l1::mock::MockLlmProvider;

        let sid = Uuid::new_v4();
        let storage = MockStorage::new();
        storage.add_messages(
            sid,
            vec![
                user_msg(sid, 0, "单块消息"),
                target_msg(sid, 1000, "单块回复"),
            ],
        );

        let llm = MockLlmProvider::new("test-model");
        // LLM 意外输出 continuation → 无上文时强制置 None（保持 v1.4 语义）
        llm.set_response(llm_json("单块摘要", Some("延续")));

        let config = L1SummarizerConfig {
            persona_uid: Some("char-0001".into()),
            utt_splitter: Some(UttSplitterConfig::default()),
            ..Default::default()
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(sid).await;
        assert!(result.is_ok(), "单块生成应成功: {:?}", result.err());

        let saved = storage.saved_l1_entries();
        assert_eq!(saved.len(), 1, "单块只生成一条 L1");
        assert!(
            saved[0].continuation.is_none(),
            "无上一块时 continuation 强制 None"
        );

        // prompt 应使用 v1.4 模板（无 continuation 字段说明）
        let last = llm.last_request().expect("应有请求记录");
        assert!(
            !last.user_message.contains("continuation"),
            "单块无上文时应使用 v1.4 模板"
        );
    }

    /// 块级失败降级：块1 生成失败 → 块2 仍生成（以上一块原文为上文），不整体失败。
    #[tokio::test]
    async fn block_failure_degrades_and_later_blocks_continue() {
        use crate::l1::mock::MockLlmProvider;

        let sid = Uuid::new_v4();
        let storage = MockStorage::new();
        storage.add_messages(sid, two_block_session(sid));

        let llm = MockLlmProvider::new("test-model");
        // 块1 返回非法 JSON（模拟 LLM 故障）；块2 正常
        llm.set_responses(vec![
            "这不是 JSON".to_string(),
            llm_json("块2 摘要", Some("转折")),
        ]);

        let config = L1SummarizerConfig {
            persona_uid: Some("char-0001".into()),
            utt_splitter: Some(UttSplitterConfig {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
            }),
            ..Default::default()
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(sid).await;
        assert!(result.is_ok(), "块失败应降级继续: {:?}", result.err());

        let saved = storage.saved_l1_entries();
        assert_eq!(saved.len(), 1, "失败块不写库，成功块照常写库");
        assert_eq!(saved[0].summary, "块2 摘要");
        // 块2 的上文来自块1 原文（短块形态，无需 L1）
        let last = llm.last_request().expect("应有请求记录");
        assert!(
            last.user_message.contains("块1：用户开场"),
            "降级后以块1 原文为上文"
        );
    }

    /// 全部块失败 → 返回错误（与 v1.4 失败语义一致），无部分写入。
    #[tokio::test]
    async fn all_blocks_fail_returns_error_no_partial_write() {
        use crate::l1::mock::MockLlmProvider;

        let sid = Uuid::new_v4();
        let storage = MockStorage::new();
        storage.add_messages(sid, two_block_session(sid));

        let llm = MockLlmProvider::new("test-model");
        llm.set_responses(vec!["坏1".to_string(), "坏2".to_string()]);

        let config = L1SummarizerConfig {
            persona_uid: Some("char-0001".into()),
            utt_splitter: Some(UttSplitterConfig::default()),
            ..Default::default()
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(sid).await;
        assert!(result.is_err(), "全部块失败应返回错误");
        assert!(
            storage.saved_l1_entries().is_empty(),
            "全部失败不应有任何写入"
        );
    }

    /// 未配置切分器（utt_splitter=None）→ 整会话一块，与 v1.4 完全一致。
    #[tokio::test]
    async fn no_splitter_config_falls_back_to_v1_4_single_block() {
        use crate::l1::mock::MockLlmProvider;

        let sid = Uuid::new_v4();
        let storage = MockStorage::new();
        // 消息间隔虽大（> θ_gap），但未配置切分器 → 不切块
        storage.add_messages(
            sid,
            vec![
                user_msg(sid, 0, "早上的消息"),
                target_msg(sid, 1000, "早上的回复"),
                user_msg(sid, 2 * 3600 * 1000, "深夜的消息"),
                target_msg(sid, 2 * 3600 * 1000 + 1000, "深夜的回复"),
            ],
        );

        let llm = MockLlmProvider::new("test-model");
        llm.set_response(llm_json("整会话摘要", None));

        let config = L1SummarizerConfig {
            persona_uid: Some("char-0001".into()),
            utt_splitter: None,
            ..Default::default()
        };

        let summarizer = L1Summarizer::new(&llm, &storage, config);
        let result = summarizer.summarize_session(sid).await;
        assert!(result.is_ok(), "未配置切分器应成功: {:?}", result.err());
        assert_eq!(
            storage.saved_l1_entries().len(),
            1,
            "未配置切分器 → 整会话一条 L1"
        );
        // 最后一次（也是唯一一次）请求不含上文
        let last = llm.last_request().expect("应有请求记录");
        assert!(
            !last.user_message.contains("continuation"),
            "v1.4 模板无 continuation"
        );
        assert!(
            last.user_message.contains("早上的消息") && last.user_message.contains("深夜的消息"),
            "整会话消息应全部进入 prompt"
        );
    }
}
