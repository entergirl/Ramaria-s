//! crates/ramaria-memory/src/event/paraphrase.rs - Attitude → Paraphrase 去情境化重述
//!
//! 设计特点:
//! - 轻量 LLM 调用: 仅当事件有 attitude 且 paraphrase 为空时才触发
//! - 结果持久化缓存到 `memory_events.paraphrase` 列，避免重复 LLM 调用
//! - 剥离具体实体（人名/地点/具体事件），提取通用行为模式
//! - 输出 ≤30 字第三人称描述
//! - 信息保留度校验: 对任意"上游文本 vs 下游重述"计算保留度
//!   （关键词覆盖 + 字符 bigram Dice 的轻量合成度量，零外部依赖），
//!   低于阈值判级联失效并走既有"保留原文/降级"路径；独立开关、关闭回退直接采用
//! - 失败/低保留时静默降级，不阻塞事件提取主流程；日志只记元数据不记原文全文

use ramaria_core::LlmProviderTrait;
use ramaria_core::traits::ChatRequest;
use std::collections::HashSet;
use tracing::{debug, warn};
use uuid::Uuid;

use super::prompt::build_paraphrase_prompt;
use crate::bm25::tokenize;

// =========================================================
// Paraphrase 配置
// =========================================================

/// Paraphrase 生成配置。
///
/// 字段约定:
/// - `retention_check_enabled`: 信息保留度校验独立开关。`true` 时生成后校验
///   paraphrase 与 attitude 原文的保留度，低于 `retention_min` 判级联失效；
///   `false` 时回退直接采用清理结果（与旧实现行为一致）。
/// - `retention_min`: 保留度下界（0.0..=1.0）。仅开关开启时生效。
#[derive(Debug, Clone)]
pub struct ParaphraseConfig {
    /// LLM 生成温度（低温度以保持稳定输出）
    pub temperature: f64,
    /// 最大输出 tokens
    pub max_tokens: u32,
    /// paraphrase 最大字符数（用于截断）
    pub max_chars: usize,
    /// 信息保留度校验开关。
    ///
    /// `true`（默认）: LLM 输出清理后计算与 attitude 原文的保留度，
    /// 低于 `retention_min` 判级联失效 → 返回 `None`（调用方以 attitude 原文兜底）。
    /// `false`: 直接采用清理结果（回退旧行为，供对比/消融）。
    pub retention_check_enabled: bool,
    /// 信息保留度最低阈值（0.0..=1.0）。
    ///
    /// 默认值经文本样例校准（prompt Target 示例与事件规格示例）:
    /// - 完全跑题/信息清空的重述保留度恰为 0（可检出）；
    /// - 保留情感/反应核心成分的正常去情境化改写约 0.11~0.44；
    /// - 极端"全换词同义改写"（如"开心"→"愉悦"）字面零共享、保留度趋 0，
    ///   纯文本口径下与跑题不可区分（度量固有局限，见 `compute_information_retention`）。
    ///
    /// 取 0.05：只拦截字面零共享/近零共享的重述（跑题、清空、全换词抽象），
    /// 放行有实质信息成分保留的改写。若取更高值会把大量合格去情境化改写
    /// （实体剥离后的部分保留）判为失效，使 paraphrase 形同虚设；
    /// 定稿值留待真实高情感数据评估。
    pub retention_min: f64,
}

impl Default for ParaphraseConfig {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            max_tokens: 128,
            max_chars: 30,
            retention_check_enabled: true,
            retention_min: 0.05,
        }
    }
}

// =========================================================
// Paraphrase 生成
// =========================================================

/// 为事件的态度生成去情境化重述。
///
/// 用法:
/// ```ignore
/// // async + 需要 &dyn LlmProviderTrait（真实 LLM 或 mock），示例仅示意调用形态。
/// let paraphrase = generate_paraphrase(llm, "被批评后很沮丧", "工作汇报后被领导批评", &config).await;
/// ```
///
/// 参数:
/// - `llm`: LLM provider 引用。
/// - `attitude`: 态度的自然语言原文（同时是信息保留度校验的上游文本）。
/// - `context`: 事件上下文（summary + keywords），供 LLM 理解但不会直接引用；
///   不参与保留度比对（拼入会稀释 attitude 核心成分占比、放大改写误判）。
/// - `config`: paraphrase 生成配置。
///
/// 返回:
/// - 成功时返回去情境化重述文本（≤30 字）。
/// - LLM 调用失败、输出为空、或开启保留校验且保留度低于阈值时返回 `None`；
///   调用方应以 attitude 原文作为 fallback（paraphrase 列留空，L3 态度聚类用原文）。
///
/// 说明:
/// - 信息保留度校验是"宁缺毋滥"方向：只有与 attitude 保留足够信息量的
///   重述才值得落库缓存；判失效时走既有 attitude 原文路径，不劣于直接采信低质量重述。
/// - 当前实现为单跳（attitude→paraphrase）；`compute_information_retention`
///   对任意"上游 vs 下游重述"对生效，未来引入多跳时同一函数直接适用。
pub async fn generate_paraphrase(
    llm: &dyn LlmProviderTrait,
    attitude: &str,
    context: &str,
    config: &ParaphraseConfig,
) -> Option<String> {
    // 构建 prompt
    let prompt = build_paraphrase_prompt(attitude, context);

    let request_id = Uuid::new_v4();
    let llm_request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: vec![],
        user_message: prompt,
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        request_id,
        template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
        enable_thinking: None,
    };

    // 调用 LLM
    let raw = match llm.chat(&llm_request).await {
        Ok(text) => text,
        Err(e) => {
            warn!(%request_id, error=%e, "paraphrase LLM 调用失败，使用 attitude 原文作为 fallback");
            return None;
        }
    };

    // 清理输出（去引号/截断/去空白）
    let cleaned = clean_paraphrase(&raw, config.max_chars);

    if cleaned.is_empty() {
        warn!(%request_id, "paraphrase 输出为空，使用 attitude 原文作为 fallback");
        return None;
    }

    // 信息保留度校验（独立开关，默认开启）
    if config.retention_check_enabled {
        // 上游取 attitude 原文：度量的是"attitude→paraphrase 这一跳"的信息保留。
        let retention = compute_information_retention(attitude, &cleaned);
        if retention < config.retention_min {
            warn!(
                %request_id,
                source_len = attitude.chars().count(),
                restated_len = cleaned.chars().count(),
                retention,
                retention_min = config.retention_min,
                "paraphrase 信息保留度低于阈值（级联失效），使用 attitude 原文作为 fallback"
            );
            return None;
        }
        debug!(
            %request_id,
            source_len = attitude.chars().count(),
            restated_len = cleaned.chars().count(),
            retention,
            retention_min = config.retention_min,
            "paraphrase 信息保留度校验通过"
        );
    } else {
        debug!(%request_id, "paraphrase 信息保留度校验已关闭，直接采用清理结果");
    }

    // 隐私约束：不记录 attitude/paraphrase 原文，仅记录字符长度等元数据
    debug!(
        %request_id,
        source_len = attitude.chars().count(),
        restated_len = cleaned.chars().count(),
        "paraphrase 生成成功"
    );

    Some(cleaned)
}

// =========================================================
// 纯函数：信息保留度（级联失效度量）
// =========================================================

/// 计算"上游文本 → 下游重述"的信息保留度，归一化到 [0.0, 1.0]。
///
/// 职责:
/// - 度量重述相对上游文本保留了多少信息成分，用于检出逐级重述的信息丢失
///   （去情境化级联失效）。函数对任意"原文 vs 重述"文本对生效，
///   单跳（attitude→paraphrase）与未来多跳（paraphrase 再作为上游）可直接复用。
/// - 纯文本度量，不依赖 LLM/embedding/外部词典，embedding 不可用时天然
///   退化为本口径（零额外 I/O、无降级失败路径）。
///
/// 度量构成（权重依据见下）:
/// - ① 关键词覆盖（权重 0.6）: 对 `source` 用 `bm25::tokenize` 分词
///   （复用 keyword normalizer 的 Bigram 口径：CJK bigram + 英文小写词，
///   标点/空白丢弃），去重后统计命中 `restated` 的占比。
///   关键词覆盖直接度量"上游关键信息成分是否到达下游"，是级联失效的主指标，
///   故权重最高。
/// - ② 字符级 bigram Dice（权重 0.4）: 字符两两序列集合的 Dice 相似度，
///   捕捉同义改写/词序调整下仍保留的字面重叠；作为关键词覆盖的补充，
///   权重较低以避免对去情境化替换（实体→抽象）的过度惩罚。
///
/// 取舍说明:
/// - 未预留可插拔 embedding 语义一致性参数：重述文本 ≤30 字、cluster 域小，
///   纯文本口径已足够；引入 embedding 会带来 trait 依赖与 mock 复杂度，
///   且对 ≤30 字短文本的区分度增益需真实数据实证（留后续数据阶段评估）。
/// - 纯文本口径的固有局限: 极端"全换词同义改写"（如"开心"→"愉悦"）字面
///   共享趋零，保留度会低估。此时判失效仅触发"保留原文"降级（不比直接
///   采信劣化重述差），方向安全。
///
/// 返回:
/// - 归一化保留度（0.0..=1.0），不 panic。
/// - `source` 或 `restated` 为空/纯空白时返回 0.0；是否判级联失效由调用方
///   依据业务阈值决定，本函数不抛错。
pub fn compute_information_retention(source: &str, restated: &str) -> f64 {
    if source.trim().is_empty() || restated.trim().is_empty() {
        return 0.0;
    }

    // ① 关键词覆盖：source 分词（去重）→ 命中 restated 的占比
    let source_tokens: HashSet<String> = tokenize(source).into_iter().collect();
    let coverage = if source_tokens.is_empty() {
        0.0
    } else {
        let restated_tokens: HashSet<String> = tokenize(restated).into_iter().collect();
        let hit = source_tokens.intersection(&restated_tokens).count();
        hit as f64 / source_tokens.len() as f64
    };

    // ② 字符级 bigram Dice（字面一致性）
    let dice = char_bigram_dice(source, restated);

    (0.6 * coverage + 0.4 * dice).clamp(0.0, 1.0)
}

/// 计算两段文本字符级 bigram 集合的 Dice 相似度。
///
/// 说明:
/// - bigram 按原始字符序列两两成对（含标点），对空/单字符文本不产生 bigram。
/// - 双方 bigram 集合均为空（≤1 字符的极短文本）时退化为整串相等判定，
///   保证返回有限值、不出现除零/NaN。
fn char_bigram_dice(source: &str, restated: &str) -> f64 {
    let a = char_bigrams(source);
    let b = char_bigrams(restated);

    if a.is_empty() && b.is_empty() {
        return if source == restated { 1.0 } else { 0.0 };
    }
    let intersection = a.intersection(&b).count();
    2.0 * intersection as f64 / (a.len() + b.len()) as f64
}

/// 提取文本的字符级 bigram 集合（顺序无关，用于 Dice 相似度）。
fn char_bigrams(text: &str) -> HashSet<(char, char)> {
    let chars: Vec<char> = text.chars().collect();
    chars.windows(2).map(|w| (w[0], w[1])).collect()
}

// =========================================================
// 纯函数：paraphrase 清理
// =========================================================

/// 清理 LLM 输出的 paraphrase 文本。
///
/// 操作:
/// 1. 剥离可能的引号包裹
/// 2. 截断到 `max_chars` 个字符
/// 3. 去除首尾空白
fn clean_paraphrase(raw: &str, max_chars: usize) -> String {
    let text = raw.trim();

    // 剥离 LLM 可能添加的引号（ASCII 双引号、中文弯引号、ASCII 单引号）
    let text = text.trim_matches(|c: char| {
        c == '"' || c == '\u{201c}' || c == '\u{201d}' // ASCII "  + 中文弯引号 " / "
            || c == '\''
    });

    // 截断到最大字符数（字符边界，不破坏 UTF-8）
    let truncated = ramaria_core::text::truncate_chars_bare(text, max_chars);

    truncated.trim().to_string()
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests;
