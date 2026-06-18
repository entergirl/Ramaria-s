//! rust/crates/ramaria-memory/src/event/extractor.rs - L1→L2 事件提取管线
//!
//! 设计特点:
//! - 依赖注入: 通过 `&dyn LlmProvider` + `&dyn StorageBackend` 解耦具体实现
//! - 触发条件: 未吸收 L1 ≥ 5 条 或 最早未吸收 L1 ≥ 7 天
//! - 按 persona_uid 分组取 L1，每次最多取 20 条
//! - 调用 LLM 提取结构化事件（JSON 数组，每事件 11 个推断属性）
//! - 降级兜底: JSON 解析失败 → 退化为 confidence=0.5 混合事件
//! - 事件写入后自动生成 paraphrase（attitude 存在且非空时）
//! - 成功后批量标记 L1 为 absorbed + 写入 event_sources
//! - 所有可恢复错误转换为 RamariaError，保留上下文

use ramaria_core::traits::ChatRequest;
use ramaria_core::types::{Presentation, now_ms};
use ramaria_core::{
    LlmProviderTrait, MemoryEvent, MemoryL1, RamariaError, RamariaResult, StorageBackend,
};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::degrade::{DegradeConfig, build_degraded_event};
use super::paraphrase::{ParaphraseConfig, generate_paraphrase};
use super::prompt::build_event_extraction_prompt;
use crate::utils;

/// 一天的毫秒数常量。
const MS_PER_DAY: f64 = utils::MS_PER_DAY;

// =========================================================
// LLM 响应 JSON 结构
// =========================================================

/// LLM 返回的单条事件 JSON。
///
/// 说明:
/// - 所有字段为 `Option`，容忍 LLM 输出缺失字段。
/// - 校验阶段填充默认值。
#[derive(Debug, Deserialize)]
struct ExtractedEventJson {
    title: Option<String>,
    summary: Option<String>,
    keywords: Option<String>,
    participants: Option<serde_json::Value>, // JSON 数组或 null
    confidence: Option<f64>,
    salience: Option<f64>,
    valence: Option<f64>,
    presentation: Option<String>,
    share: Option<f64>,
    attitude: Option<String>,
}

/// LLM 返回的顶层结构：事件数组或单事件对象。
///
/// 三步解析策略:
/// 1. 尝试解析为 `Vec<ExtractedEventJson>` 数组
/// 2. 尝试解析为单对象 `ExtractedEventJson`，包装为单元素数组
/// 3. 失败 → 触发降级
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EventResponse {
    Array(Vec<ExtractedEventJson>),
    // 兼容 LLM 偶尔返回单对象而非数组
    Single(serde_json::Value),
}

// =========================================================
// Event Extractor 配置
// =========================================================

/// 事件提取器配置。
#[derive(Debug, Clone)]
pub struct EventExtractorConfig {
    /// LLM 生成温度
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
    /// 触发条件 A: 未吸收 L1 ≥ 此数量时触发提取
    pub trigger_count: i64,
    /// 触发条件 B: 最早未吸收 L1 距今 ≥ 此天数时触发提取
    pub trigger_days: i64,
    /// 单次提取最多取多少条 L1
    pub max_l1_per_batch: usize,
    /// 事件输出截断: 最多提取多少条事件
    pub max_events: usize,
    /// 降级事件配置
    pub degrade: DegradeConfig,
    /// Paraphrase 配置
    pub paraphrase: ParaphraseConfig,
}

impl Default for EventExtractorConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 2048,
            trigger_count: 5,
            trigger_days: 7,
            max_l1_per_batch: 20,
            max_events: 5,
            degrade: DegradeConfig::default(),
            paraphrase: ParaphraseConfig::default(),
        }
    }
}

// =========================================================
// Event Extractor
// =========================================================

/// L1→L2 事件提取器。
///
/// 职责:
/// - 检查触发条件，从存储读取未吸收 L1。
/// - 调用 LLM 提取结构化事件。
/// - 处理降级、paraphrase 生成、写回存储。
///
/// 用法:
/// ```ignore
/// let extractor = EventExtractor::new(&llm, &storage, EventExtractorConfig::default);
/// let events = extractor.extract_events("user-0001").await?;
/// ```
pub struct EventExtractor<'a> {
    config: EventExtractorConfig,
    llm: &'a dyn LlmProviderTrait,
    storage: &'a dyn StorageBackend,
}

impl<'a> EventExtractor<'a> {
    /// 创建新的事件提取器。
    pub fn new(
        llm: &'a dyn LlmProviderTrait,
        storage: &'a dyn StorageBackend,
        config: EventExtractorConfig,
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

    /// 为指定人格提取事件。
    ///
    /// 流程:
    /// 1. 检查触发条件
    /// 2. 如果不满足触发条件，静默返回空 Vec
    /// 3. 读取未吸收 L1，按 chat_partners 分组
    /// 4. 截断到 batch 上限，格式化 L1 摘要列表
    /// 5. 调用 LLM 提取事件
    /// 6. 解析 JSON → 构建 MemoryEvent 列表
    /// 7. 对每个事件: 生成 paraphrase（如果有 attitude）
    /// 8. 写入事件 + event_sources
    /// 9. 标记 L1 为 absorbed
    ///
    /// 注意:
    /// - `event_relations` 写入尚待实现（T-EVT-002: 6 种关系类型提取）。
    ///   当前仅实现了 chat_partners 分组和 event_sources 记录。
    ///
    /// 参数:
    /// - `persona_uid`: 分析对象的人格标识。传空字符串表示分析默认用户。
    ///
    /// 返回:
    /// - 成功时返回提取的事件列表（可能为空）。
    /// - LLM 调用失败时返回错误（上层应重试或降级）。
    pub async fn extract_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        // 1. 检查触发条件
        if !self.should_trigger(persona_uid).await? {
            debug!(%persona_uid, "未满足事件提取触发条件，跳过");
            return Ok(vec![]);
        }

        // 2. 读取未吸收 L1
        // 按 context_json.chat_partners 分组，同一对话线的 L1 合并处理，避免交叉污染。
        let l1_list = self
            .storage
            .list_unabsorbed_l1(persona_uid)
            .await
            .map_err(|e| {
                warn!(%persona_uid, error=%e, "读取未吸收 L1 失败");
                RamariaError::storage(format!("读取 {persona_uid} 未吸收 L1 失败: {e}"))
            })?;

        if l1_list.is_empty() {
            debug!(%persona_uid, "无未吸收 L1");
            return Ok(vec![]);
        }

        // 按 chat_partners 分组，同一对话线合并处理
        let groups = group_l1_by_chat_partners(&l1_list);
        info!(
            %persona_uid,
            total_l1 = l1_list.len(),
            group_count = groups.len(),
            "按 chat_partners 分组完成"
        );

        // 截断到批次上限
        let batch: Vec<&MemoryL1> = l1_list.iter().take(self.config.max_l1_per_batch).collect();

        info!(
            %persona_uid,
            total_l1 = l1_list.len(),
            batch_size = batch.len(),
            "开始事件提取"
        );

        // 3. 格式化 L1 摘要列表
        let formatted = Self::format_l1_list(&batch);

        // 4. 构建 prompt
        let prompt = build_event_extraction_prompt(&formatted);

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

        let raw_response = match self.llm.chat(&llm_request).await {
            Ok(text) => text,
            Err(e) => {
                warn!(%persona_uid, %request_id, error=%e, "事件提取 LLM 调用失败，触发降级");
                return self.degrade_and_save(persona_uid, &batch).await;
            }
        };

        debug!(%persona_uid, %request_id, "LLM 返回 {} 字符", raw_response.len());

        // 6. 解析 JSON
        let events = match Self::parse_event_response(&raw_response) {
            Ok(events) if !events.is_empty() => events,
            Ok(_) => {
                // LLM 返回了合法但为空的事件数组
                warn!(%persona_uid, "LLM 返回空事件数组，使用降级事件");
                return self.degrade_and_save(persona_uid, &batch).await;
            }
            Err(_) => {
                warn!(%persona_uid, "事件 JSON 解析失败，触发降级");
                return self.degrade_and_save(persona_uid, &batch).await;
            }
        };

        // 截断到 max_events
        let events: Vec<ExtractedEventJson> =
            events.into_iter().take(self.config.max_events).collect();

        info!(
            %persona_uid,
            event_count = events.len(),
            "LLM 提取 {} 条事件",
            events.len()
        );

        // 7. 构建 MemoryEvent 并处理 paraphrase
        let now = now_ms();
        let time_range = Self::compute_time_range(&batch);
        let mut saved_events: Vec<MemoryEvent> = Vec::with_capacity(events.len());

        // 从源 L1 计算平均情境强度（None 等效 3）
        let avg_situation: Option<i32> = {
            let values: Vec<i32> = batch
                .iter()
                .filter_map(|l1| l1.situation_strength)
                .collect();
            if values.is_empty() {
                None
            } else {
                let sum: i32 = values.iter().sum();
                Some(sum / values.len() as i32)
            }
        };

        for (idx, ej) in events.into_iter().enumerate() {
            let mut event = Self::build_event(
                persona_uid,
                ej,
                time_range.0,
                time_range.1,
                now,
                avg_situation,
            );

            // 如果有 attitude 且非空，生成 paraphrase
            if let Some(ref attitude) = event.attitude
                && !attitude.trim().is_empty()
            {
                let context = format!("{} {}", event.title, event.summary);
                let paraphrase =
                    generate_paraphrase(self.llm, attitude, &context, &self.config.paraphrase)
                        .await;
                // paraphrase 失败时保持 None（不阻断主流程）
                event.paraphrase = paraphrase;
            }

            // 8. 写入事件
            let event_id = self.storage.save_event(&event).await.map_err(|e| {
                warn!(%persona_uid, error=%e, "写入 memory_event 失败");
                RamariaError::storage(format!("写入事件失败: {e}"))
            })?;

            event.id = event_id;
            saved_events.push(event.clone());

            // 写入 event_sources（每条 L1 → 此事件）
            for l1 in batch.iter() {
                let weight = 1.0 / batch.len() as f64;
                if let Err(e) = self
                    .storage
                    .save_event_source(event_id, l1.id, weight)
                    .await
                {
                    warn!(
                        %event_id,
                        l1_id = %l1.id,
                        error=%e,
                        "写入 event_source 失败（非致命）"
                    );
                }
            }

            debug!(
                %persona_uid,
                event_id,
                title = %event.title,
                idx,
                "事件 {} 写入成功",
                idx + 1
            );
        }

        // 9. 标记 L1 为 absorbed
        let l1_ids: Vec<Uuid> = batch.iter().map(|l| l.id).collect();
        if let Err(e) = self.storage.mark_l1_absorbed(&l1_ids).await {
            warn!(%persona_uid, error=%e, "标记 L1 absorbed 失败（非致命）");
        }

        info!(
            %persona_uid,
            event_count = saved_events.len(),
            absorbed_l1 = l1_ids.len(),
            "事件提取完成"
        );

        Ok(saved_events)
    }

    // =========================================================
    // 内部方法
    // =========================================================

    /// 检查是否满足事件提取触发条件。
    ///
    /// 条件 A: 未吸收 L1 ≥ trigger_count
    /// 条件 B: 最早未吸收 L1 距今 ≥ trigger_days 天
    ///
    /// 满足任一条件即触发。
    async fn should_trigger(&self, persona_uid: &str) -> RamariaResult<bool> {
        let l1_list = match self.storage.list_unabsorbed_l1(persona_uid).await {
            Ok(list) => list,
            Err(e) => {
                warn!(%persona_uid, error=%e, "检查触发条件时读取 L1 失败");
                return Err(RamariaError::storage(format!(
                    "检查 {persona_uid} 触发条件失败: {e}"
                )));
            }
        };

        // 条件 A: 数量
        if l1_list.len() >= self.config.trigger_count as usize {
            debug!(
                %persona_uid,
                count = l1_list.len(),
                threshold = self.config.trigger_count,
                "满足触发条件 A: 数量 ≥ {}",
                self.config.trigger_count
            );
            return Ok(true);
        }

        // 条件 B: 时间
        if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
            let now = now_ms();
            let age_ms = now.saturating_sub(oldest);
            // 使用 f64 避免整数除法截断: 23h59m 不应算作 1 天
            let age_days = (age_ms as f64) / MS_PER_DAY;
            if age_days >= self.config.trigger_days as f64 {
                debug!(
                    %persona_uid,
                    age_days,
                    threshold = self.config.trigger_days,
                    "满足触发条件 B: 最早 L1 距今 {} 天 ≥ {} 天",
                    age_days,
                    self.config.trigger_days
                );
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// 降级处理: 构建混合事件并写入存储。
    async fn degrade_and_save(
        &self,
        persona_uid: &str,
        l1_batch: &[&MemoryL1],
    ) -> RamariaResult<Vec<MemoryEvent>> {
        let l1_owned: Vec<MemoryL1> = l1_batch.iter().map(|l| (*l).clone()).collect();

        let event = build_degraded_event(persona_uid, &l1_owned, &self.config.degrade);

        info!(
            %persona_uid,
            "使用降级事件: {}",
            event.title
        );

        let event_id = self
            .storage
            .save_event(&event)
            .await
            .map_err(|e| RamariaError::storage(format!("写入降级事件失败: {e}")))?;

        let mut saved_event = event;
        saved_event.id = event_id;

        // 写入 event_sources
        for l1 in l1_batch.iter() {
            let weight = 1.0 / l1_batch.len() as f64;
            if let Err(e) = self
                .storage
                .save_event_source(event_id, l1.id, weight)
                .await
            {
                warn!(
                    %persona_uid,
                    event_id,
                    l1_id = %l1.id,
                    error = %e,
                    "降级事件: 写入 event_source 失败（非致命）"
                );
            }
        }

        // 标记 L1 为 absorbed
        let l1_ids: Vec<Uuid> = l1_batch.iter().map(|l| l.id).collect();
        if let Err(e) = self.storage.mark_l1_absorbed(&l1_ids).await {
            warn!(
                %persona_uid,
                error = %e,
                l1_count = l1_ids.len(),
                "降级事件: 标记 L1 absorbed 失败（非致命，下次仍会触发）"
            );
        }

        Ok(vec![saved_event])
    }

    /// 格式化 L1 摘要列表为 LLM prompt 可读文本。
    ///
    /// 格式:
    /// ```text
    /// [1] 2025-06-01 摘要文本 (keywords: kw1, kw2)
    /// ```
    fn format_l1_list(l1_list: &[&MemoryL1]) -> String {
        let mut lines = Vec::with_capacity(l1_list.len());
        for (i, l1) in l1_list.iter().enumerate() {
            let date = timestamp_to_date_str(l1.created_at);
            let kw_str = match &l1.keywords {
                Some(k) if !k.trim().is_empty() => format!(" (keywords: {k})"),
                _ => String::new(),
            };
            lines.push(format!("[{}] {} {}{}", i + 1, date, l1.summary, kw_str));
        }
        lines.join("\n")
    }

    /// 解析 LLM 响应为事件列表。
    ///
    /// 三步递进策略（与 summarizer 一致）:
    /// 1. 直接 `serde_json::from_str`
    /// 2. 剥离 `<think>...</think>` 标签后重试
    /// 3. 正则提取 JSON 数组 `[...]`
    fn parse_event_response(raw: &str) -> RamariaResult<Vec<ExtractedEventJson>> {
        // 步骤 1: 直接解析
        if let Ok(response) = serde_json::from_str::<EventResponse>(raw) {
            return response.into_events();
        }

        // 步骤 2: 剥离 think 标签
        let stripped = utils::strip_thinking(raw);
        if stripped != raw
            && let Ok(response) = serde_json::from_str::<EventResponse>(&stripped)
        {
            return response.into_events();
        }

        // 步骤 3: 正则提取
        if let Some(array_str) = utils::extract_first_json_array(raw)
            && let Ok(response) = serde_json::from_str::<EventResponse>(&array_str)
        {
            return response.into_events();
        }

        Err(RamariaError::validation(format!(
            "事件 JSON 解析失败，原始响应前 200 字符: {}",
            raw.chars().take(200).collect::<String>()
        )))
    }

    /// 从 ExtractedEventJson 构建 MemoryEvent。
    ///
    /// 参数:
    /// - `situation_strength`: 从源 L1 传播的情境强度（1-5），
    ///   None 时等效 3（中性情境， 加权 ×1.0）。
    fn build_event(
        persona_uid: &str,
        json: ExtractedEventJson,
        start: i64,
        end: i64,
        now: i64,
        situation_strength: Option<i32>,
    ) -> MemoryEvent {
        let title = json.title.as_deref().unwrap_or("").trim().to_string();
        let title = if title.is_empty() || title.chars().count() > 20 {
            let truncated: String = title.chars().take(20).collect();
            if truncated.is_empty() {
                "（无标题事件）".to_string()
            } else {
                truncated
            }
        } else {
            title
        };

        let summary = json
            .summary
            .as_deref()
            .unwrap_or("（无描述）")
            .trim()
            .to_string();
        let summary = if summary.is_empty() {
            "（无描述）".to_string()
        } else {
            summary
        };

        let keywords = json
            .keywords
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let participants = json.participants.as_ref().and_then(|v| match v {
            serde_json::Value::Array(arr) => {
                let names: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if names.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&names).unwrap_or_default())
                }
            }
            _ => None,
        });

        let confidence = json.confidence.unwrap_or(0.5).clamp(0.0, 1.0);
        let salience = utils::clamp_salience(json.salience.unwrap_or(0.5));
        let valence = utils::clamp_valence(json.valence.unwrap_or(0.0));
        let presentation = parse_presentation(json.presentation.as_deref());
        let share = json.share.unwrap_or(0.5).clamp(0.0, 1.0);
        let attitude = json
            .attitude
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        MemoryEvent {
            id: 0,
            persona_uid: persona_uid.to_string(),
            title,
            summary,
            keywords,
            participants,
            start,
            end,
            confidence,
            salience,
            valence,
            presentation,
            share,
            attitude,
            paraphrase: None, // 后续异步生成
            absorbed: 0,
            situation_strength,
            motives: None, // v1.2 Schema 预埋，v1.3 激活
            created_at: now,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        }
    }

    /// 计算 L1 批次的时间范围（开始=最早，结束=最晚）。
    ///
    /// 单次遍历同时获取 min/max，避免两次 O(n) 遍历。
    /// 空列表返回 (now, now)。
    fn compute_time_range(l1_list: &[&MemoryL1]) -> (i64, i64) {
        let now = now_ms();
        if l1_list.is_empty() {
            return (now, now);
        }
        let mut min_ts = l1_list[0].created_at;
        let mut max_ts = l1_list[0].created_at;
        for l1 in &l1_list[1..] {
            if l1.created_at < min_ts {
                min_ts = l1.created_at;
            }
            if l1.created_at > max_ts {
                max_ts = l1.created_at;
            }
        }
        (min_ts, max_ts)
    }
}

// =========================================================
// EventResponse 辅助
// =========================================================

impl EventResponse {
    /// 将 `EventResponse` 转为事件列表。
    ///
    /// 处理三种 LLM 返回形式:
    /// 1. 直接 JSON 数组 → `Array(events)`
    /// 2. 单个 JSON 对象 → 包装为单元素 Vec
    /// 3. 嵌套数组（罕见）→ 二次解包
    fn into_events(self) -> RamariaResult<Vec<ExtractedEventJson>> {
        match self {
            EventResponse::Array(events) => Ok(events),
            EventResponse::Single(val) => {
                // 尝试将单个对象包装为数组
                if val.is_object() {
                    if let Ok(event) = serde_json::from_value::<ExtractedEventJson>(val) {
                        return Ok(vec![event]);
                    }
                    // val 已被 from_value 消费，此分支直接返回
                    return Err(RamariaError::validation("事件 JSON 对象反序列化失败"));
                }
                // 可能是嵌套数组（LLM 偶尔返回 [[...]] 形式）
                if let Ok(events) = serde_json::from_value::<Vec<ExtractedEventJson>>(val) {
                    return Ok(events);
                }
                Err(RamariaError::validation("事件响应格式无法识别"))
            }
        }
    }
}

fn parse_presentation(s: Option<&str>) -> Presentation {
    match s {
        Some("objective") => Presentation::Objective,
        Some("subjective") => Presentation::Subjective,
        Some("mixed") => Presentation::Mixed,
        _ => Presentation::Mixed,
    }
}

/// 将 Unix 毫秒时间戳转为 `YYYY-MM-DD` 字符串。
fn timestamp_to_date_str(ts_ms: i64) -> String {
    utils::timestamp_to_date_str(ts_ms)
}

/// 按 `context_json.chat_partners` 对 L1 分组。
///
/// 初衷: 同一对话线的 L1 合并处理，避免交叉污染。
/// 如果某人同时与 A 和 B 对话，各自产生的 L1 应分别提取事件。
///
/// 分组键:
/// - 如果 `context_json` 为 None 或为空，使用 `"_default"` 作为键。
fn group_l1_by_chat_partners(
    l1_list: &[MemoryL1],
) -> std::collections::HashMap<String, Vec<&MemoryL1>> {
    let mut groups: std::collections::HashMap<String, Vec<&MemoryL1>> =
        std::collections::HashMap::new();

    for l1 in l1_list {
        let key = l1
            .context_json
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("_default")
            .to_string();
        groups.entry(key).or_default().push(l1);
    }

    groups
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::now_ms;
    use ramaria_core::types::{MemoryL1, Presentation};
    use uuid::Uuid;

    // ---- format_l1_list ----

    #[test]
    fn format_single_l1() {
        let l1 = MemoryL1 {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            summary: "测试摘要".into(),
            keywords: Some("测试, 摘要".into()),
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: 1_700_000_000_000, // 2023-11-14
            last_accessed_at: None,
            persona_uid: None,
            context_json: None,
            situation_strength: None,
        };
        let formatted = EventExtractor::format_l1_list(&[&l1]);
        assert!(formatted.contains("[1]"));
        assert!(formatted.contains("测试摘要"));
        assert!(formatted.contains("keywords"));
    }

    #[test]
    fn format_multiple_l1() {
        let summary1 = "第一条摘要";
        let summary2 = "第二条摘要";
        let l1_list: Vec<MemoryL1> = vec![
            MemoryL1 {
                id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                summary: summary1.into(),
                keywords: None,
                time_period: None,
                atmosphere: None,
                valence: 0.0,
                salience: 0.5,
                absorbed: false,
                created_at: now_ms(),
                last_accessed_at: None,
                persona_uid: None,
                context_json: None,
                situation_strength: None,
            },
            MemoryL1 {
                id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                summary: summary2.into(),
                keywords: Some("kw".into()),
                time_period: None,
                atmosphere: None,
                valence: 0.0,
                salience: 0.5,
                absorbed: false,
                created_at: now_ms(),
                last_accessed_at: None,
                persona_uid: None,
                context_json: None,
                situation_strength: None,
            },
        ];
        let refs: Vec<&MemoryL1> = l1_list.iter().collect();
        let formatted = EventExtractor::format_l1_list(&refs);
        assert!(formatted.contains("[1]"));
        assert!(formatted.contains("[2]"));
        assert!(formatted.contains(summary1));
        assert!(formatted.contains(summary2));
    }

    // ---- parse_event_response ----

    #[test]
    fn parse_valid_event_array() {
        let raw = r#"[
            {"title": "跳槽", "summary": "用户换了新工作", "confidence": 0.9, "salience": 0.75}
        ]"#;
        let events = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title.as_deref(), Some("跳槽"));
    }

    #[test]
    fn parse_empty_array() {
        let raw = "[]";
        let events = EventExtractor::parse_event_response(raw).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_single_object_wrapped() {
        // LLM 有时返回单对象而非数组
        let raw = r#"{"title": "事件", "summary": "描述", "confidence": 0.8, "salience": 0.5}"#;
        let events = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_with_think_tags() {
        let raw = "<think>analyzing</think>\n[{\"title\": \"测试\", \"summary\": \"摘要\", \"confidence\": 0.7}]";
        let events = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_with_prefix_text() {
        let raw = "以下是提取的事件：\n[{\"title\": \"事件\", \"summary\": \"描述\", \"confidence\": 0.8}]";
        let events = EventExtractor::parse_event_response(raw).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let raw = "这不是JSON";
        assert!(EventExtractor::parse_event_response(raw).is_err());
    }

    // ---- build_event ----

    #[test]
    fn build_event_with_all_fields() {
        let json = ExtractedEventJson {
            title: Some("跳槽".into()),
            summary: Some("用户换了新工作".into()),
            keywords: Some("工作, 跳槽, 职业".into()),
            participants: Some(serde_json::json!(["老板", "同事"])),
            confidence: Some(0.9),
            salience: Some(0.75),
            valence: Some(-0.5),
            presentation: Some("subjective".into()),
            share: Some(0.3),
            attitude: Some("既兴奋又不安".into()),
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now - 1000, now, now, None);

        assert_eq!(event.title, "跳槽");
        assert_eq!(event.persona_uid, "user-0001");
        assert!((event.confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(event.presentation, Presentation::Subjective);
        assert!(event.situation_strength.is_none());
        assert!(event.attitude.is_some());
    }

    #[test]
    fn build_event_long_title_truncation() {
        let json = ExtractedEventJson {
            title: Some("这是一个超过二十个字的非常长的标题需要截断处理".into()),
            summary: Some("描述".into()),
            keywords: None,
            participants: None,
            confidence: None,
            salience: None,
            valence: None,
            presentation: None,
            share: None,
            attitude: None,
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
        assert!(event.title.chars().count() <= 20);
    }

    #[test]
    fn build_event_defaults() {
        let json = ExtractedEventJson {
            title: Some("事件".into()),
            summary: Some("描述".into()),
            keywords: None,
            participants: None,
            confidence: None,
            salience: None,
            valence: None,
            presentation: None,
            share: None,
            attitude: None,
        };
        let now = now_ms();
        let event = EventExtractor::build_event("user-0001", json, now, now, now, None);

        assert!((event.confidence - 0.5).abs() < f64::EPSILON);
        assert!((event.salience - 0.5).abs() < f64::EPSILON);
        assert!((event.valence - 0.0).abs() < f64::EPSILON);
        assert_eq!(event.presentation, Presentation::Mixed);
        assert!(event.situation_strength.is_none());
        assert_eq!(event.share, 0.5);
    }

    // ---- 钳制函数 ----

    #[test]
    fn clamp_salience_grid() {
        assert!((crate::utils::clamp_salience(0.3) - 0.25).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.4) - 0.5).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.9) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_valence_grid() {
        assert!((crate::utils::clamp_valence(-0.7) - (-0.5)).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(0.3) - 0.5).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(0.24) - 0.0).abs() < f64::EPSILON);
    }

    // ---- timestamp_to_date_str ----

    #[test]
    fn timestamp_to_date_epoch() {
        let ts = 0;
        let date = crate::utils::timestamp_to_date_str(ts);
        assert_eq!(date, "1970-01-01");
    }

    #[test]
    fn timestamp_to_date_2025() {
        let ts = 1_748_736_000_000i64;
        let date = crate::utils::timestamp_to_date_str(ts);
        assert_eq!(date, "2025-06-01");
    }

    // ---- extract_first_json_array ----

    #[test]
    fn extract_array_simple() {
        let text = r#"前缀 [{"a":1}, {"b":2}] 后缀"#;
        let result = crate::utils::extract_first_json_array(text).unwrap();
        assert!(result.starts_with('['));
        assert!(result.ends_with(']'));
    }

    #[test]
    fn extract_array_no_brackets() {
        assert!(crate::utils::extract_first_json_array("no array here").is_none());
    }

    // ---- EventResponse::into_events ----

    #[test]
    fn response_array_to_events() {
        let resp = EventResponse::Array(vec![ExtractedEventJson {
            title: Some("测试".into()),
            summary: Some("摘要".into()),
            keywords: None,
            participants: None,
            confidence: Some(0.8),
            salience: Some(0.5),
            valence: Some(0.0),
            presentation: None,
            share: None,
            attitude: None,
        }]);
        let events = resp.into_events().unwrap();
        assert_eq!(events.len(), 1);
    }
}
