//! crates/ramaria-memory/src/event/prompt/tests.rs - 事件抽取提示词单元测试
//!
//! 设计特点:
//! - 位于 event::prompt 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由父文件以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

#[test]
fn event_prompt_contains_required_fields() {
    let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
    assert!(prompt.contains("title"));
    assert!(prompt.contains("summary"));
    assert!(prompt.contains("confidence"));
    assert!(prompt.contains("salience"));
    assert!(prompt.contains("valence"));
    assert!(prompt.contains("presentation"));
    assert!(prompt.contains("share"));
    assert!(prompt.contains("attitude"));
    assert!(prompt.contains("participants"));
    assert!(prompt.contains("keywords"));
}

#[test]
fn event_prompt_contains_motives_field() {
    let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
    assert!(prompt.contains("motives"), "prompt 应包含 motives 字段");
    assert!(prompt.contains("自我保护"));
    assert!(prompt.contains("归属"));
    assert!(prompt.contains("求知"));
}

#[test]
fn event_prompt_contains_relations_format() {
    let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
    assert!(prompt.contains("relations"), "prompt 应包含 relations 数组");
    assert!(prompt.contains("from_index"));
    assert!(prompt.contains("to_index"));
    assert!(prompt.contains("CausedBy"));
    assert!(prompt.contains("PartOf"));
    assert!(prompt.contains("RelatedTo"));
    assert!(prompt.contains("ContinuedBy"));
    assert!(prompt.contains("Contradicts"));
    assert!(prompt.contains("Timeline"));
}

#[test]
fn event_prompt_contains_role_distinction_rules() {
    let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
    assert!(
        prompt.contains("角色区分"),
        "v2.0: prompt 应包含角色区分段落"
    );
    assert!(prompt.contains("另一方"));
    assert!(prompt.contains("用户主动"));
    assert!(prompt.contains("逻辑错误"));
}

/// v1.4 M4：提取 Prompt 含因果线索段落——
/// cause 槽位说明、仅供背景参考标注、防编造约束。
#[test]
fn event_prompt_contains_evidence_clue_section() {
    let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
    assert!(prompt.contains("因果线索"), "应包含因果线索规则段落");
    assert!(prompt.contains("[线索]"), "应说明 [线索] 行格式");
    assert!(prompt.contains("cause"), "应说明 cause 槽位");
    assert!(prompt.contains("仅供背景参考"), "应标注仅供背景参考");
    assert!(
        prompt.contains("CausedBy"),
        "cause 应与 CausedBy 关系提取关联"
    );
    assert!(prompt.contains("不得"), "应禁止依据线索编造新事件");
}

/// v1.4 M4：因果线索段落同时存在于 persona 替换版本（导入场景）。
#[test]
fn persona_prompt_keeps_evidence_clue_section() {
    let prompt = build_event_extraction_prompt_for_persona("[1] 2025-01-01 测试", "张三", None);
    assert!(
        prompt.contains("因果线索"),
        "persona 版 prompt 也应含因果线索段落"
    );
    assert!(prompt.contains("cause"));
}

#[test]
fn persona_prompt_injects_other_party_hint() {
    let prompt =
        build_event_extraction_prompt_for_persona("[1] 2025-01-01 测试", "张三", Some("李四"));
    assert!(prompt.contains("张三"), "应包含 persona 名称");
    assert!(prompt.contains("李四"), "应包含对话另一方名称");
    assert!(prompt.contains("对话方：李四"));
}

#[test]
fn persona_prompt_no_other_party_no_hint() {
    let prompt = build_event_extraction_prompt_for_persona("[1] 2025-01-01 测试", "张三", None);
    assert!(prompt.contains("张三"));
    assert!(!prompt.contains("对话方"), "无另一方时不应注入角色提示");
}

#[test]
fn event_prompt_injects_l1_text() {
    let l1_text = "[1] 2025-06-01 用户去看了电影";
    let prompt = build_event_extraction_prompt(l1_text);
    assert!(prompt.contains(l1_text));
}

#[test]
fn event_prompt_contains_valence_options() {
    let prompt = build_event_extraction_prompt("test");
    for val in &["-1.0", "-0.5", "0.0", "0.5", "1.0"] {
        assert!(prompt.contains(val), "prompt should mention {val}");
    }
}

#[test]
fn event_prompt_contains_presentation_options() {
    let prompt = build_event_extraction_prompt("test");
    assert!(prompt.contains("objective"));
    assert!(prompt.contains("subjective"));
    assert!(prompt.contains("mixed"));
}

#[test]
fn paraphrase_prompt_injects_both_fields() {
    let attitude = "被批评后很沮丧";
    let context = "用户在工作汇报后被领导批评了PPT做得不好";
    let prompt = build_paraphrase_prompt(attitude, context);
    assert!(prompt.contains(attitude));
    assert!(prompt.contains(context));
}

#[test]
fn paraphrase_prompt_has_instructions() {
    let prompt = build_paraphrase_prompt("test", "context");
    assert!(prompt.contains("去情境化"));
    assert!(prompt.contains("第三人称"));
    assert!(prompt.contains("不超过 30 字"));
}

// =========================================================
// build_event_extraction_prompt_with_context 测试
// =========================================================

use crate::event::context_retriever::ContextDocument;

#[test]
fn prompt_with_context_injects_context_section() {
    let context_docs = vec![ContextDocument {
        summary: "历史事件1: 完成Rust项目".to_string(),
        layer: "l2".to_string(),
        source_channel: "exact".to_string(),
        score: 1.0,
    }];
    let prompt =
        build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &context_docs);
    assert!(prompt.contains("仅供参考"));
    assert!(prompt.contains("不得据此编造"));
    assert!(prompt.contains("[L2] 历史事件1"));
}

#[test]
fn prompt_with_context_empty_docs_no_injection() {
    let prompt = build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &[]);
    // 空上下文 → 不注入补充背景段落（v1.4 M4 起基础模板自带
    // "因果线索（仅供背景参考）"段落，因此断言补充背景专属标题）
    assert!(
        !prompt.contains("### 补充背景"),
        "空上下文不应注入补充背景段落"
    );
    assert!(
        !prompt.contains("[L1]") && !prompt.contains("[L2]"),
        "不应有上下文条目"
    );
    assert!(prompt.contains("测试摘要"));
}

#[test]
fn prompt_with_context_multiple_docs() {
    let context_docs = vec![
        ContextDocument {
            summary: "历史L1: Rust编程".to_string(),
            layer: "l1".to_string(),
            source_channel: "exact".to_string(),
            score: 1.0,
        },
        ContextDocument {
            summary: "历史L2: 完成Rust项目".to_string(),
            layer: "l2".to_string(),
            source_channel: "substring".to_string(),
            score: 0.5,
        },
    ];
    let prompt =
        build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &context_docs);
    assert!(prompt.contains("[L1] 历史L1: Rust编程"));
    assert!(prompt.contains("[L2] 历史L2: 完成Rust项目"));
}

#[test]
fn prompt_with_context_contains_merge_instruction() {
    let context_docs = vec![ContextDocument {
        summary: "历史事件".to_string(),
        layer: "l2".to_string(),
        source_channel: "exact".to_string(),
        score: 1.0,
    }];
    let prompt =
        build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &context_docs);
    // 验证去重约束已注入
    assert!(prompt.contains("合并"));
    assert!(prompt.contains("而非新建事件"));
    assert!(prompt.contains("直接复制为新事件"));
}

/// 示例与约束一致性（T-V29-6-004 审查修订）：示例中的 `salience` 取五档值、
/// `share` 避开被约束点名的 0.3 / 0.7 锚点——示例矛盾会直接污染输出分布。
#[test]
fn event_prompt_example_matches_constraints() {
    let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
    assert!(
        prompt.contains(r#""salience":0.75"#),
        "示例 salience 应取五档值 0.75"
    );
    assert!(
        !prompt.contains(r#""salience":0.7,"#),
        "不应残留非五档示例值"
    );
    assert!(
        prompt.contains(r#""share":0.45"#),
        "示例 share 应取连续值并避开 0.3 / 0.7 锚点"
    );
}
