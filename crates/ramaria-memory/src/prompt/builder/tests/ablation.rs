//! crates/ramaria-memory/src/prompt/builder/tests/ablation.rs - 注入闸门消融
//!
//! 设计特点:
//! - 由 父测试模块 以 mod ablation; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 注入闸门渲染测试（探针消融：B0 无记忆 / F4 −脉络 / F3 −表达）
// =========================================================

/// 全开（默认）时四层段落全部渲染——回归红线：默认行为不回归。
#[test]
fn ablation_all_on_renders_all_layers() {
    let result = assemble_prompt(&make_full_ctx(), &PromptConfig::default());
    assert!(result.contains("# 记忆（脉络层）"));
    assert!(result.contains("## 近期对话脉络"));
    assert!(result.contains("## 相关历史记忆"));
    assert!(result.contains("## 原文片段"));
    assert!(result.contains("## 桥接"));
    assert!(result.contains("# 说话风格（表达层）"));
    assert!(result.contains("## 对话示例"));
    assert!(result.contains("## 性格特征"));
    assert!(result.contains("## 已知事实"));
}

/// B0 无记忆注入：关闭全部记忆子段 + 表达子段后，
/// prompt 不含记忆块 / 行为块占位（行为/知识由数据层门控，渲染侧无段落）。
#[test]
fn ablation_b0_omits_memory_and_style_blocks() {
    let config = PromptConfig {
        include_speaking_style: false,
        include_examples: false,
        include_narrative: false,
        include_memory_rag: false,
        include_utt: false,
        include_bridge: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    // 记忆块（含各子段与占位）整体不产生
    assert!(
        !result.contains("# 记忆（脉络层）"),
        "B0 不应含记忆块: {result}"
    );
    assert!(!result.contains("## 近期对话脉络"));
    assert!(!result.contains("## 相关历史记忆"));
    assert!(!result.contains("## 原文片段"));
    assert!(!result.contains("## 桥接"));
    assert!(!result.contains("无历史对话"), "B0 不应出现脉络占位");
    // 表达层（说话风格 + 自动风格规则 + 对话示例）不产生
    assert!(!result.contains("# 说话风格（表达层）"));
    assert!(!result.contains("## 自动风格规则"));
    assert!(!result.contains("## 说话风格"));
    assert!(!result.contains("## 对话示例"));
    // 纯角色保留（persona 身份是 B0 的"纯角色"组成部分）
    assert!(result.contains("# 角色（行为层）"));
    assert!(result.contains("小明"));
}

/// F4 −脉络层：近期对话脉络与桥接子段不渲染，原文片段仍在。
#[test]
fn ablation_f4_omits_narrative_and_bridge_keeps_utt() {
    let config = PromptConfig {
        include_narrative: false,
        include_bridge: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    assert!(!result.contains("## 近期对话脉络"), "F4 应无脉络: {result}");
    assert!(!result.contains("## 桥接"), "F4 应无桥接");
    assert!(result.contains("## 原文片段"), "F4 保留原文样例");
    assert!(!result.contains("无历史对话"), "关闭脉络时不产生占位");
}

/// F3 −表达层：说话风格/自动风格规则/对话示例不渲染；记忆块仍保留。
#[test]
fn ablation_f3_omits_expression_keeps_memory() {
    let config = PromptConfig {
        include_speaking_style: false,
        include_examples: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    assert!(
        !result.contains("# 说话风格（表达层）"),
        "F3 应无表达层: {result}"
    );
    assert!(!result.contains("## 自动风格规则"));
    assert!(!result.contains("## 说话风格"));
    assert!(!result.contains("## 对话示例"));
    // 手工 speaking_style 随表达层一并关闭（build_personality 也受闸门控制）
    assert!(!result.contains("热情活泼"));
    assert!(result.contains("# 记忆（脉络层）"), "F3 保留记忆块");
    assert!(result.contains("## 近期对话脉络"));
}

/// 记忆子段全部关闭时整块不产生——行为/知识等由数据层负责，此处验证记忆块边界。
#[test]
fn ablation_memory_subsections_off_omits_whole_block() {
    let config = PromptConfig {
        include_narrative: false,
        include_memory_rag: false,
        include_utt: false,
        include_bridge: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    assert!(
        !result.contains("# 记忆（脉络层）"),
        "全部记忆子段关闭时整块省略: {result}"
    );
}
