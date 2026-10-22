//! crates/ramaria-memory/src/l1/prompt/tests.rs - L1 摘要提示词单元测试
//!
//! 设计特点:
//! - 位于 l1::prompt 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由父文件以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

/// build_l1_prompt 各关键词输入参数化验证（None/有值/空串/纯空白）。
#[test]
fn build_prompt_keywords_cases() {
    // 无关键词 → 不含"关键词候选"
    let conv = "用户：你好\n助手：你好，有什么可以帮你的？";
    let prompt = build_l1_prompt(conv, None, None);
    assert!(prompt.contains(conv));
    assert!(!prompt.contains("关键词候选"));
    assert!(prompt.contains("清晨"));
    assert!(prompt.contains("Context"));
    // 有关键词 → 注入候选
    let conv = "用户：今天天气真不错";
    let prompt = build_l1_prompt(conv, Some("天气, 心情, 户外"), None);
    assert!(prompt.contains(conv));
    assert!(prompt.contains("天气"));
    assert!(prompt.contains("心情"));
    assert!(prompt.contains("关键词候选"));
    // 空串 → 回退（不含候选）
    let prompt = build_l1_prompt("用户：测试", Some(""), None);
    assert!(!prompt.contains("关键词候选"));
    assert!(prompt.contains("测试"));
    // 纯空白 → 回退（不含候选）
    let prompt = build_l1_prompt("用户：测试", Some("   "), None);
    assert!(!prompt.contains("关键词候选"));
}

#[test]
fn keyword_inject_constants() {
    assert_eq!(KEYWORD_INJECT_THRESHOLD, 100);
    assert_eq!(KEYWORD_INJECT_LIMIT, 50);
}

#[test]
fn prompt_contains_all_required_fields() {
    let prompt = build_l1_prompt("test", None, None);
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
    let prompt = build_l1_prompt("test", None, None);
    for period in &["清晨", "上午", "下午", "傍晚", "夜间", "深夜"] {
        assert!(prompt.contains(period), "prompt should mention {period}");
    }
}

#[test]
fn prompt_mentions_valid_valence_values() {
    let prompt = build_l1_prompt("test", None, None);
    for val in &["-1.0", "-0.5", "0.0", "0.5", "1.0"] {
        assert!(prompt.contains(val), "prompt should mention valence {val}");
    }
}

#[test]
fn keyword_prompt_also_contains_evidence_notes() {
    let prompt = build_l1_prompt("test", Some("天气, 心情"), None);
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

/// 两条模板均须输出结构化对象数组
/// `[{text, time?, who?, cause?}]`，含槽位说明与 JSON 示例。
#[test]
fn both_templates_use_structured_object_array() {
    for prompt in [
        build_l1_prompt("test", None, None),
        build_l1_prompt("test", Some("天气, 心情"), None),
    ] {
        // 槽位说明齐全
        assert!(
            prompt.contains("`text`（必填）"),
            "应说明 text 必填: {prompt}"
        );
        assert!(prompt.contains("`time`（可选）"), "应说明 time 可选槽位");
        assert!(prompt.contains("`who`（可选）"), "应说明 who 可选槽位");
        assert!(prompt.contains("`cause`（可选）"), "应说明 cause 可选槽位");
        // JSON 示例为对象数组（含 { 起始的 text 槽位），不再是字符串数组
        assert!(
            prompt.contains(r#""evidence_notes":[{"text""#),
            "示例应为对象数组: {prompt}"
        );
        // 旧字符串数组示例必须移除，避免误导 LLM 输出旧格式
        assert!(
            !prompt.contains(r#""evidence_notes":["..."]"#),
            "不应残留旧字符串数组示例"
        );
        // 可选槽位缺失时的降级语义说明
        assert!(prompt.contains("无法判断时省略"), "应说明槽位可省略");
        assert!(prompt.contains("宁缺毋滥"), "应包含 cause 宁缺毋滥约束");
    }
}

/// v1.4 M4：两条模板均明确 cause 槽位仅记短句原因、不编造。
#[test]
fn both_templates_mention_cause_guidance() {
    for prompt in [
        build_l1_prompt("test", None, None),
        build_l1_prompt("test", Some("天气, 心情"), None),
    ] {
        assert!(prompt.contains("原因/触发条件"), "应说明 cause 语义");
        assert!(prompt.contains("不要编造"), "应禁止编造 cause");
    }
}

// =========================================================
// 上下文感知模板测试
// =========================================================

/// 注入上文 → 使用上下文感知模板（含 prior_context 段落与 continuation 字段）。
#[test]
fn prior_context_uses_context_aware_template() {
    let conv = "用户：今天天气真好\n助手：是啊";
    let prompt = build_l1_prompt(conv, None, Some("上一块：讨论了周末计划"));
    // 上文段落注入
    assert!(prompt.contains("上一块：讨论了周末计划"));
    assert!(prompt.contains("上一块上文"));
    // continuation 输出字段说明与示例
    assert!(prompt.contains("continuation"));
    assert!(prompt.contains("延续"));
    assert!(prompt.contains("转折"));
    assert!(prompt.contains("无关"));
    assert!(prompt.contains("仅用于判断话题延续性"));
}

/// 上文 + 关键词候选 → 使用「关键词 + 上文」模板，两段落均注入。
#[test]
fn prior_context_with_keywords_uses_combined_template() {
    let prompt = build_l1_prompt(
        "用户：测试对话",
        Some("天气, 心情"),
        Some("上一块：聊了工作"),
    );
    assert!(prompt.contains("上一块：聊了工作"));
    assert!(prompt.contains("关键词候选"));
    assert!(prompt.contains("continuation"));
    assert!(prompt.contains("天气"));
    assert!(prompt.contains("心情"));
}

/// 空白上文 → 回退非上下文模板（与 v1.4 等价），不出现 continuation 字段。
#[test]
fn blank_prior_context_falls_back_to_plain_template() {
    for prior in [Some("   "), Some("")] {
        let prompt = build_l1_prompt("用户：测试", None, prior);
        assert!(!prompt.contains("上一块上文"), "空白上文不注入段落");
        assert!(
            !prompt.contains("continuation"),
            "无上文时不应输出 continuation 字段（保持 v1.4 模板）"
        );
    }
}

/// 无上文 + 无关键词 → 输出与 v1.4 基础模板完全一致（回归红线）。
#[test]
fn no_prior_no_keywords_identical_to_v1_4_base() {
    let conv = "用户：你好\n助手：你好";
    let now = build_l1_prompt(conv, None, None);
    let v1_4 = L1_SUMMARY_PROMPT_BASE.replace("{conversation}", conv);
    assert_eq!(now, v1_4, "无上文时应与 v1.4 基础模板逐字节一致");
}

/// 无上文 + 关键词 → 输出与 v1.4 关键词模板完全一致（回归红线）。
#[test]
fn no_prior_with_keywords_identical_to_v1_4_keyword_template() {
    let conv = "用户：测试";
    let now = build_l1_prompt(conv, Some("天气"), None);
    let v1_4 = L1_SUMMARY_PROMPT_WITH_KEYWORDS
        .replace("{conversation}", conv)
        .replace("{keyword_candidates}", "天气");
    assert_eq!(now, v1_4, "无上文时应与 v1.4 关键词模板逐字节一致");
}

/// 上下文感知模板同样保留 evidence_notes 结构化对象数组约束（与 v1.4 对齐）。
#[test]
fn prior_context_template_keeps_structured_evidence_notes() {
    let prompt = build_l1_prompt("test", None, Some("上文"));
    assert!(
        prompt.contains(r#""evidence_notes":[{"text""#),
        "示例应为对象数组: {prompt}"
    );
    assert!(
        !prompt.contains(r#""evidence_notes":["..."]"#),
        "不应残留旧字符串数组示例"
    );
    assert!(prompt.contains("`cause`（可选）"), "应保留 cause 槽位说明");
}
