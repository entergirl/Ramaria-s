//! rust/crates/ramaria-memory/src/l1/prompt.rs - L1 摘要 Prompt 模板管理
//!
//! 设计特点:
//! - 基础模板 + 关键词候选注入两个变体
//! - 双版本：仅情绪 + 情绪含关键词，根据 keyword_pool 状态自动选择
//! - 字段约束（六选一时间段、五档效价/显著性）嵌入 Prompt 以提升 LLM 输出合规率
//! - 与 Python v0.x 的 `L1_SUMMARY_PROMPT` 语义对齐
//! - 所有模板为 `&'static str`，零运行时分配

// =========================================================
// L1 摘要基础 Prompt（不含关键词候选）
// =========================================================

/// L1 摘要基础 Prompt —— emotion-only 版本。
///
/// 用途: keyword_pool 为空（冷启动）时使用。
///
/// 字段约束:
/// - time_period: 严格六选一（清晨/上午/下午/傍晚/夜间/深夜）
/// - valence: 五档 (-1.0/-0.5/0.0/0.5/1.0)
/// - salience: 五档 (0.0/0.25/0.5/0.75/1.0)
///
/// 格式要求:
/// - 纯 JSON 输出，不加 markdown 代码块、不加说明文字
/// - summary 用第三人称，用"烧酒"指代用户
pub const L1_SUMMARY_PROMPT_BASE: &str = r#"你是一个对话摘要助手。请根据下面的对话内容，生成一条结构化摘要。

【输出格式要求】
严格按照以下 JSON 格式输出，不要输出任何其他内容（不要加 markdown 代码块，不要加说明文字）：

{
  "summary": "第三人称客观描述本次对话的核心结论，一句话，不超过50字",
  "keywords": "3到5个名词标签，用英文逗号分隔，标签之间不能语义重复",
  "time_period": "从以下六个选项中选一个：清晨、上午、下午、傍晚、夜间、深夜",
  "atmosphere": "四字以内描述对话整体氛围，例如：专注高效、轻松愉快、情绪低落",
  "valence": 0.0,
  "salience": 0.5
}

【valence 情绪效价说明】
只能从以下五个值中选一个：
-1.0  非常消极（崩溃、绝望、强烈愤怒）
-0.5  偏消极（疲惫、担心、轻度低落）
 0.0  中性（平静日常、技术问答、无明显情绪）
 0.5  偏积极（放松、满意、轻度开心）
 1.0  非常积极（兴奋、强烈成就感、里程碑）

【salience 情感显著性说明】
只能从以下五个值中选一个：
0.0   平淡（纯闲聊或技术问答，无情感投入）
0.25  轻微（有轻微情绪但不重要）
0.5   中等（正常对话，有情感内容）
0.75  较高（情绪明显，或话题对用户有重要意义）
1.0   极高（强烈情绪，或人生重要节点/里程碑）

【其他字段说明】
- summary：只记结论，不记过程；用"烧酒"指代用户；客观陈述，不加主观评价
- keywords：只取名词，不取动词或形容词；避免同义词重复；无合适关键词时填空字符串
- time_period：根据对话发生的时间判断；若无法判断，根据内容氛围推断
- atmosphere：优先反映整体基调；四字以内，不超过四字

【对话内容】
{conversation}"#;

// =========================================================
// L1 摘要 Prompt（含关键词候选注入）
// =========================================================

/// L1 摘要 Prompt —— emotion + keyword injection 版本。
///
/// 用途: keyword_pool 有内容时使用，引导 LLM 复用已有词典。
///
/// 注入策略:
/// - 词典 <= 100 条时全量注入
/// - 词典 > 100 条时取使用频次最高的 50 条
/// - 候选列表仅作提示，LLM 可生成列表外的新关键词
pub const L1_SUMMARY_PROMPT_WITH_KEYWORDS: &str = r#"你是一个对话摘要助手。请根据下面的对话内容，生成一条结构化摘要。

【输出格式要求】
严格按照以下 JSON 格式输出，不要输出任何其他内容（不要加 markdown 代码块，不要加说明文字）：

{
  "summary": "第三人称客观描述本次对话的核心结论，一句话，不超过50字",
  "keywords": "3到5个名词标签，用英文逗号分隔，标签之间不能语义重复",
  "time_period": "从以下六个选项中选一个：清晨、上午、下午、傍晚、夜间、深夜",
  "atmosphere": "四字以内描述对话整体氛围，例如：专注高效、轻松愉快、情绪低落",
  "valence": 0.0,
  "salience": 0.5
}

【valence 情绪效价说明】
只能从以下五个值中选一个：
-1.0  非常消极（崩溃、绝望、强烈愤怒）
-0.5  偏消极（疲惫、担心、轻度低落）
 0.0  中性（平静日常、技术问答、无明显情绪）
 0.5  偏积极（放松、满意、轻度开心）
 1.0  非常积极（兴奋、强烈成就感、里程碑）

【salience 情感显著性说明】
只能从以下五个值中选一个：
0.0   平淡（纯闲聊或技术问答，无情感投入）
0.25  轻微（有轻微情绪但不重要）
0.5   中等（正常对话，有情感内容）
0.75  较高（情绪明显，或话题对用户有重要意义）
1.0   极高（强烈情绪，或人生重要节点/里程碑）

【其他字段说明】
- summary：只记结论，不记过程；用"烧酒"指代用户；客观陈述，不加主观评价
- keywords：只取名词，不取动词或形容词；避免同义词重复；优先从下方候选列表中选择
- time_period：根据对话发生的时间判断；若无法判断，根据内容氛围推断
- atmosphere：优先反映整体基调；四字以内，不超过四字

【关键词候选列表】（仅供提示，可选用列表外的关键词）
{keyword_candidates}

【对话内容】
{conversation}"#;

// =========================================================
// Prompt 构建函数
// =========================================================

/// 关键词注入阈值。
///
/// 词典条数 ≤ 此值时全量注入；> 此值时取频次最高的 50 条。
pub const KEYWORD_INJECT_THRESHOLD: usize = 100;

/// 关键词注入上限。
pub const KEYWORD_INJECT_LIMIT: usize = 50;

/// 为 L1 摘要构建 Prompt。
///
/// 用法:
/// - 根据 keyword_pool 是否有内容选择基础版或关键词注入版。
/// - `format_conversation` 应已预先格式化为 "用户：xxx\n助手：xxx" 格式。
///
/// 参数:
/// - `conversation_text`: 格式化后的完整对话文本。
/// - `keyword_candidates`: 从 keyword_pool 读取的关键词候选列表（逗号分隔字符串）。
///    传 `None` 或空字符串时使用基础版 prompt。
///
/// 返回:
/// - 完整 prompt 字符串，可直接作为 LLM user message 发送。
pub fn build_l1_prompt(conversation_text: &str, keyword_candidates: Option<&str>) -> String {
    match keyword_candidates {
        Some(kw) if !kw.trim().is_empty() => L1_SUMMARY_PROMPT_WITH_KEYWORDS
            .replace("{conversation}", conversation_text)
            .replace("{keyword_candidates}", kw.trim()),
        _ => L1_SUMMARY_PROMPT_BASE.replace("{conversation}", conversation_text),
    }
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_without_keywords() {
        let conv = "用户：你好\n助手：你好，有什么可以帮你的？";
        let prompt = build_l1_prompt(conv, None);
        assert!(prompt.contains(conv));
        assert!(!prompt.contains("关键词候选"));
        assert!(prompt.contains("清晨"));
    }

    #[test]
    fn build_prompt_with_keywords() {
        let conv = "用户：今天天气真不错";
        let keywords = "天气, 心情, 户外";
        let prompt = build_l1_prompt(conv, Some(keywords));
        assert!(prompt.contains(conv));
        assert!(prompt.contains("天气"));
        assert!(prompt.contains("心情"));
        assert!(prompt.contains("关键词候选"));
    }

    #[test]
    fn build_prompt_with_empty_keywords_falls_back() {
        let conv = "用户：测试";
        let prompt = build_l1_prompt(conv, Some(""));
        assert!(!prompt.contains("关键词候选"));
        assert!(prompt.contains("测试"));
    }

    #[test]
    fn build_prompt_with_whitespace_keywords_falls_back() {
        let conv = "用户：测试";
        let prompt = build_l1_prompt(conv, Some("   "));
        assert!(!prompt.contains("关键词候选"));
    }

    #[test]
    fn keyword_inject_constants() {
        assert_eq!(KEYWORD_INJECT_THRESHOLD, 100);
        assert_eq!(KEYWORD_INJECT_LIMIT, 50);
    }

    #[test]
    fn prompt_contains_all_required_fields() {
        let prompt = build_l1_prompt("test", None);
        assert!(prompt.contains("summary"));
        assert!(prompt.contains("keywords"));
        assert!(prompt.contains("time_period"));
        assert!(prompt.contains("atmosphere"));
        assert!(prompt.contains("valence"));
        assert!(prompt.contains("salience"));
    }

    #[test]
    fn prompt_mentions_valid_time_periods() {
        let prompt = build_l1_prompt("test", None);
        for period in &["清晨", "上午", "下午", "傍晚", "夜间", "深夜"] {
            assert!(prompt.contains(period), "prompt should mention {period}");
        }
    }

    #[test]
    fn prompt_mentions_valid_valence_values() {
        let prompt = build_l1_prompt("test", None);
        for val in &["-1.0", "-0.5", "0.0", "0.5", "1.0"] {
            assert!(prompt.contains(val), "prompt should mention valence {val}");
        }
    }
}
