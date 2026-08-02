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
/// v2.0 重构 (CRAFT 框架):
/// - Context: 明确任务背景（对话摘要供记忆系统检索聚合）。
/// - Role: 客观第三方视角，离散值约束。
/// - Action: 8 字段任务描述。
/// - Format: 严格 JSON 输出格式。
/// - Target: 质量目标（客观、精炼、可独立理解）。
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
/// - summary 用第三人称
pub const L1_SUMMARY_PROMPT_BASE: &str = r#"# Context（背景）
你是一个对话摘要助手。将一段多人对话提炼为一条结构化摘要，供记忆系统后续检索和聚合。

# Role（角色定位）
你以客观第三方视角观察对话，不添加主观评价。你用精确的离散值描述情绪和显著性，而非模糊的自然语言。

# Action（执行任务）
根据下方对话内容，生成一条结构化摘要，包含 8 个字段：

1. **summary** — 第三人称一句话客观描述本次对话的核心结论，不超过 50 字。只记结论不记过程，用对话中的实际人物名称指代。
2. **keywords** — 3–5 个中文名词标签，逗号分隔。只取名词，不取动词或形容词，避免同义词重复。无合适关键词时填空字符串。
3. **time_period** — 六选一：清晨 / 上午 / 下午 / 傍晚 / 夜间 / 深夜（无法判断时根据内容氛围推断）
4. **atmosphere** — 四字以内描述对话整体氛围，如"专注高效""轻松愉快""情绪低落"
5. **valence** — 情绪效价，五选一：
   - `-1.0` 非常消极（崩溃、绝望、强烈愤怒）
   - `-0.5` 偏消极（疲惫、担心、轻度低落）
   - `0.0` 中性（平静日常、技术问答、无明显情绪）
   - `0.5` 偏积极（放松、满意、轻度开心）
   - `1.0` 非常积极（兴奋、强烈成就感、里程碑）
6. **salience** — 情感显著性，五选一：
   - `0.0` 平淡（纯闲聊或技术问答，无情感投入）
   - `0.25` 轻微（有轻微情绪但不重要）
   - `0.5` 中等（正常对话，有情感内容）
   - `0.75` 较高（情绪明显，或话题对分析对象有重要意义）
   - `1.0` 极高（强烈情绪，或人生重要节点/里程碑）
7. **situation_strength** — 情境强度，1–5 整数：
   - `1` 很弱情境（纯粹闲聊、寒暄、无实质内容）
   - `2` 较弱情境（日常琐事、随性聊天）
   - `3` 中性情境（普通对话、一般交流）
   - `4` 较强情境（重要对话、关键决策、正式场合）
   - `5` 强情境（危机处理、重大人生事件、强烈冲突）
8. **evidence_notes** — 1–3 条支撑 summary 结论的具体事实引用，每条不少于 5 个中文字符。记录"表达了什么观点/经历了什么事件"，引用原话或转述具体事实。

# Format（输出格式）
你的整个回复必须是一个裸 JSON 对象，以 { 开头、以 } 结尾，不要添加任何其他内容：

{"summary":"...","keywords":"...","time_period":"...","atmosphere":"...","valence":0.0,"salience":0.5,"situation_strength":3,"evidence_notes":["...","..."]}

# Target（质量目标）
- summary 客观、精炼、可独立理解（脱离对话原文仍有信息量）
- valence/salience 严格使用五档离散值，不做微调（如 0.3、0.7 等无效值）
- evidence_notes 从对话中提取，不编造

---

# 对话内容
{conversation}"#;

// =========================================================
// L1 摘要 Prompt（含关键词候选注入）
// =========================================================

/// L1 摘要 Prompt —— emotion + keyword injection 版本。
///
/// v2.0 重构 (CRAFT 框架): 与基础版共享相同的 CRAFT 结构，
/// 仅在 Action > keywords 字段末尾注入关键词候选列表，
/// 并在 Target 中增加"优先使用候选列表中的词"的引导。
///
/// 用途: keyword_pool 有内容时使用，引导 LLM 复用已有词典。
///
/// 注入策略:
/// - 词典 <= 100 条时全量注入
/// - 词典 > 100 条时取使用频次最高的 50 条
/// - 候选列表仅作提示，LLM 可生成列表外的新关键词
pub const L1_SUMMARY_PROMPT_WITH_KEYWORDS: &str = r#"# Context（背景）
你是一个对话摘要助手。将一段多人对话提炼为一条结构化摘要，供记忆系统后续检索和聚合。

# Role（角色定位）
你以客观第三方视角观察对话，不添加主观评价。你用精确的离散值描述情绪和显著性，而非模糊的自然语言。

# Action（执行任务）
根据下方对话内容，生成一条结构化摘要，包含 8 个字段：

1. **summary** — 第三人称一句话客观描述本次对话的核心结论，不超过 50 字。只记结论不记过程，用对话中的实际人物名称指代。
2. **keywords** — 3–5 个中文名词标签，逗号分隔。只取名词，不取动词或形容词，避免同义词重复。优先从下方候选列表中选择。
3. **time_period** — 六选一：清晨 / 上午 / 下午 / 傍晚 / 夜间 / 深夜（无法判断时根据内容氛围推断）
4. **atmosphere** — 四字以内描述对话整体氛围，如"专注高效""轻松愉快""情绪低落"
5. **valence** — 情绪效价，五选一：
   - `-1.0` 非常消极（崩溃、绝望、强烈愤怒）
   - `-0.5` 偏消极（疲惫、担心、轻度低落）
   - `0.0` 中性（平静日常、技术问答、无明显情绪）
   - `0.5` 偏积极（放松、满意、轻度开心）
   - `1.0` 非常积极（兴奋、强烈成就感、里程碑）
6. **salience** — 情感显著性，五选一：
   - `0.0` 平淡（纯闲聊或技术问答，无情感投入）
   - `0.25` 轻微（有轻微情绪但不重要）
   - `0.5` 中等（正常对话，有情感内容）
   - `0.75` 较高（情绪明显，或话题对分析对象有重要意义）
   - `1.0` 极高（强烈情绪，或人生重要节点/里程碑）
7. **situation_strength** — 情境强度，1–5 整数：
   - `1` 很弱情境（纯粹闲聊、寒暄、无实质内容）
   - `2` 较弱情境（日常琐事、随性聊天）
   - `3` 中性情境（普通对话、一般交流）
   - `4` 较强情境（重要对话、关键决策、正式场合）
   - `5` 强情境（危机处理、重大人生事件、强烈冲突）
8. **evidence_notes** — 1–3 条支撑 summary 结论的具体事实引用，每条不少于 5 个中文字符。记录"表达了什么观点/经历了什么事件"，引用原话或转述具体事实。

# Format（输出格式）
你的整个回复必须是一个裸 JSON 对象，以 { 开头、以 } 结尾，不要添加任何其他内容：

{"summary":"...","keywords":"...","time_period":"...","atmosphere":"...","valence":0.0,"salience":0.5,"situation_strength":3,"evidence_notes":["...","..."]}

# Target（质量目标）
- summary 客观、精炼、可独立理解（脱离对话原文仍有信息量）
- keywords 优先使用候选列表中的词，便于跨会话聚合
- valence/salience 严格使用五档离散值，不做微调（如 0.3、0.7 等无效值）
- evidence_notes 从对话中提取，不编造

---

# 关键词候选列表（仅供提示，可选用列表外的关键词）
{keyword_candidates}

# 对话内容
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
///   传 `None` 或空字符串时使用基础版 prompt。
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

    /// build_l1_prompt 各关键词输入参数化验证（None/有值/空串/纯空白）。
    #[test]
    fn build_prompt_keywords_cases() {
        // 无关键词 → 不含"关键词候选"
        let conv = "用户：你好\n助手：你好，有什么可以帮你的？";
        let prompt = build_l1_prompt(conv, None);
        assert!(prompt.contains(conv));
        assert!(!prompt.contains("关键词候选"));
        assert!(prompt.contains("清晨"));
        assert!(prompt.contains("Context"));
        // 有关键词 → 注入候选
        let conv = "用户：今天天气真不错";
        let prompt = build_l1_prompt(conv, Some("天气, 心情, 户外"));
        assert!(prompt.contains(conv));
        assert!(prompt.contains("天气"));
        assert!(prompt.contains("心情"));
        assert!(prompt.contains("关键词候选"));
        // 空串 → 回退（不含候选）
        let prompt = build_l1_prompt("用户：测试", Some(""));
        assert!(!prompt.contains("关键词候选"));
        assert!(prompt.contains("测试"));
        // 纯空白 → 回退（不含候选）
        let prompt = build_l1_prompt("用户：测试", Some("   "));
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
        assert!(
            prompt.contains("situation_strength"),
            "prompt 应包含 situation_strength 字段"
        );
        assert!(
            prompt.contains("evidence_notes"),
            "prompt 应包含 evidence_notes 字段"
        );
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

    #[test]
    fn keyword_prompt_also_contains_evidence_notes() {
        let prompt = build_l1_prompt("test", Some("天气, 心情"));
        assert!(
            prompt.contains("situation_strength"),
            "关键词注入版 prompt 也应包含 situation_strength"
        );
        assert!(
            prompt.contains("evidence_notes"),
            "关键词注入版 prompt 也应包含 evidence_notes"
        );
        assert!(
            prompt.contains("关键词候选"),
            "关键词注入版 prompt 应包含关键词候选段落"
        );
    }
}
