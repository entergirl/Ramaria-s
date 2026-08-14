//! rust/crates/ramaria-memory/src/event/paraphrase.rs - Attitude → Paraphrase 去情境化重述
//!
//! 设计特点:
//! - 轻量 LLM 调用: 仅当事件有 attitude 且 paraphrase 为空时才触发
//! - 结果持久化缓存到 `memory_events.paraphrase` 列，避免重复 LLM 调用
//! - 剥离具体实体（人名/地点/具体事件），提取通用行为模式
//! - 输出 ≤30 字第三人称描述
//! - 失败时静默降级（attitude 原文作为 fallback），不阻断事件提取主流程

use ramaria_core::LlmProviderTrait;
use ramaria_core::traits::ChatRequest;
use tracing::{debug, warn};
use uuid::Uuid;

use super::prompt::build_paraphrase_prompt;

// =========================================================
// Paraphrase 配置
// =========================================================

/// Paraphrase 生成配置。
#[derive(Debug, Clone)]
pub struct ParaphraseConfig {
    /// LLM 生成温度（低温度以保持稳定输出）
    pub temperature: f64,
    /// 最大输出 tokens
    pub max_tokens: u32,
    /// paraphrase 最大字符数（用于截断）
    pub max_chars: usize,
}

impl Default for ParaphraseConfig {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            max_tokens: 128,
            max_chars: 30,
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
/// let paraphrase = generate_paraphrase(llm, "被批评后很沮丧", "工作汇报后被领导批评", &config).await;
/// ```
///
/// 参数:
/// - `llm`: LLM provider 引用。
/// - `attitude`: 态度的自然语言原文。
/// - `context`: 事件上下文（summary + keywords），供 LLM 理解但不会直接引用。
/// - `config`: paraphrase 生成配置。
///
/// 返回:
/// - 成功时返回去情境化重述文本（≤30 字）。
/// - LLM 调用失败时返回 `None`（调用方应以 attitude 原文作为 fallback）。
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
    };

    // 调用 LLM
    let raw = match llm.chat(&llm_request).await {
        Ok(text) => text,
        Err(e) => {
            warn!(%request_id, error=%e, "paraphrase LLM 调用失败，使用 attitude 原文作为 fallback");
            return None;
        }
    };

    // 清理输出
    let cleaned = clean_paraphrase(&raw, config.max_chars);

    if cleaned.is_empty() {
        warn!(%request_id, "paraphrase 输出为空，使用 attitude 原文作为 fallback");
        return None;
    }

    debug!(
        %request_id,
        original = %attitude,
        paraphrase = %cleaned,
        "paraphrase 生成成功"
    );

    Some(cleaned)
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

    // 截断到最大字符数
    let truncated: String = text.chars().take(max_chars).collect();

    truncated.trim().to_string()
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// clean_paraphrase 各输入参数化验证：去引号 / 截断 / 空输入 / 保真 / 中文弯引号。
    #[test]
    fn clean_paraphrase_cases() {
        // 去英文引号（双引号/单引号）
        assert_eq!(
            clean_paraphrase(r#""面对批评时容易沮丧""#, 30),
            "面对批评时容易沮丧"
        );
        assert_eq!(
            clean_paraphrase("'面对权威时倾向于退缩'", 30),
            "面对权威时倾向于退缩"
        );
        // 去中文弯引号
        assert_eq!(
            clean_paraphrase("\u{201c}面对批评容易沮丧\u{201d}", 30),
            "面对批评容易沮丧"
        );
        // 超长截断
        let long = "这是一个非常长的去情境化描述文本超过了三十个字的限制需要截断处理";
        let result = clean_paraphrase(long, 30);
        assert!(result.chars().count() <= 30);
        // 空输入
        assert_eq!(clean_paraphrase("", 30), "");
        assert_eq!(clean_paraphrase("   ", 30), "");
        // 30 字以内保留全文
        let input = "面对亲密关系中的不安全感时倾向于过度担忧";
        assert!(input.chars().count() <= 30);
        assert_eq!(clean_paraphrase(input, 30), input);
    }

    #[test]
    fn config_defaults() {
        let config = ParaphraseConfig::default();
        assert_eq!(config.temperature, 0.2);
        assert_eq!(config.max_tokens, 128);
        assert_eq!(config.max_chars, 30);
    }
}
