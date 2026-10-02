//! crates/ramaria-memory/src/l1/summarizer/generate.rs - 单块 L1 生成与字段校验
//!
//! 设计特点:
//! - generate_chunk_l1: 单块生成（对话格式化 / LLM 调用 / JSON 解析 / 字段校验）。
//! - write_back_keywords: 关键词写回 keyword_pool + keyword_refs 倒排索引（失败非致命）。
//! - parse_summary_json: 三步递进 JSON 解析（直接 → 剥离 think 标签 → 正则提取）。
//! - validate_and_build: 字段校验（五档钳制 / 六选一 / evidence_notes 后处理）。
//! - 隐私红线：LLM 原始响应不落日志，仅记长度；所有可恢复错误转 RamariaError。

use ramaria_core::keyword::KeywordToken;
use ramaria_core::traits::ChatRequest;
use ramaria_core::{MemoryL1, RamariaError, RamariaResult};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::l1::prompt::{KEYWORD_INJECT_LIMIT, KEYWORD_INJECT_THRESHOLD, build_l1_prompt};
use crate::utils;
use crate::utt::UttChunk;

use super::helpers::{
    format_messages, parse_keywords, validate_continuation, validate_evidence_notes,
};
use super::pipeline::L1Summarizer;
use super::types::L1SummaryResponse;

impl<'a> L1Summarizer<'a> {
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
    pub(super) async fn generate_chunk_l1(
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
    pub(super) async fn write_back_keywords(
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
    pub(super) fn validate_and_build(
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
                let truncated = ramaria_core::text::truncate_chars_bare(&s, 4);
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
