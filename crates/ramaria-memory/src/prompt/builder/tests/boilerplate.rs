//! crates/ramaria-memory/src/prompt/builder/tests/boilerplate.rs - 样板体量对照
//!
//! 设计特点:
//! - 由 父测试模块 以 mod boilerplate; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 样板体量对照（标签压缩/提示优化）
// =========================================================

/// 标签压缩改造前"固定骨架"渲染体量（字符数 / 估算 token）。
///
/// 口径: 在压缩改造前用同一骨架上下文（见 [`skeleton_context`]）实测——
/// `assemble_prompt` 输出恰好由固定样板组成（能力边界 + 默认知识边界 + 默认角色 +
/// 回复规范默认规则/记忆引用规则 + 记忆层引导 + 无历史对话占位 + 无相关记忆占位 +
/// 当前时间），无任何注入数据内容，因此可作为"样板体积"的稳定代理。
/// 实测记录: chars=849, tokens=393。
const LEGACY_BOILERPLATE_CHARS: usize = 849;
const LEGACY_BOILERPLATE_TOKENS: usize = 393;

/// 构造"固定骨架"上下文（无注入内容、时间固定，保证体量稳定可断言）。
fn skeleton_context() -> PromptContext {
    PromptContext {
        current_time_str: Some("2026-06-10 10:00".into()),
        ..Default::default()
    }
}

/// 骨架样板体量下降：压缩后总体积必须小于压缩前基线（防样板回退膨胀）。
///
/// 口径: 社交对话基调与说话锚点是有意新增的固定内容块（非既有引导句膨胀），
/// 对照旧基线时以 `include_social_tone=false` 扣除，使"既有样板未回退膨胀"
/// 的约束仍然成立；两块体量另作精确增量断言（防止两块之外再有增长）。
#[test]
fn boilerplate_skeleton_shrunk_below_legacy() {
    let base_config = PromptConfig {
        include_social_tone: false,
        ..Default::default()
    };
    let base = measure_prompt_volume(&assemble_prompt(&skeleton_context(), &base_config));
    assert!(
        base.chars < LEGACY_BOILERPLATE_CHARS,
        "骨架字符数应低于压缩前基线 {LEGACY_BOILERPLATE_CHARS}，实际 {}",
        base.chars
    );
    assert!(
        base.tokens < LEGACY_BOILERPLATE_TOKENS,
        "骨架估算 token 应低于压缩前基线 {LEGACY_BOILERPLATE_TOKENS}，实际 {}",
        base.tokens
    );

    // 默认（聊天档）骨架 = 陈述档骨架 + 基调块 + 说话锚点，增量恰为两块自身体量
    let with_tone = measure_prompt_volume(&assemble_prompt(
        &skeleton_context(),
        &PromptConfig::default(),
    ));
    let chat_delta_chars = SOCIAL_CHAT_TONE_RULES.chars().count() + RESPONSE_ANCHOR.chars().count();
    assert_eq!(
        with_tone.chars,
        base.chars + chat_delta_chars,
        "基调块与说话锚点应按原文字符数精确叠加"
    );
    // token 估算在拼接边界非严格线性（±1 舍入），增量与两块自身体量对齐即可
    let chat_delta_tokens = crate::token_budget::estimate_tokens(SOCIAL_CHAT_TONE_RULES)
        + crate::token_budget::estimate_tokens(RESPONSE_ANCHOR);
    let token_delta = with_tone.tokens - base.tokens;
    assert!(
        token_delta.abs_diff(chat_delta_tokens) <= 2,
        "基调+锚点增量 token 应约等于两块自身体量 {chat_delta_tokens}，实际 {token_delta}"
    );
}

/// 各样板引导句/占位文本长度上限（引导句压缩的逐项锁定）。
///
/// 上限给的是压缩后实际文本的宽裕余量（+10 左右），防后续"加长引导"悄悄回退。
#[test]
fn boilerplate_leads_stay_within_upper_bounds() {
    let cases: &[(&str, &str, usize)] = &[
        ("CAPACITY_INTRO", CAPACITY_INTRO, 130),
        ("KNOWLEDGE_BOUNDARY_DEFAULT", KNOWLEDGE_BOUNDARY_DEFAULT, 60),
        ("ROLE_DEFAULT_TEXT", ROLE_DEFAULT_TEXT, 110),
        ("MEMORY_SECTION_INTRO", MEMORY_SECTION_INTRO, 90),
        ("NARRATIVE_PLACEHOLDER", NARRATIVE_PLACEHOLDER, 20),
        ("RAG_PLACEHOLDER", RAG_PLACEHOLDER, 30),
        ("UTT_LEAD", UTT_LEAD, 60),
        ("BRIDGE_LEAD", BRIDGE_LEAD, 60),
        ("STATEMENT_LEAD", STATEMENT_LEAD, 40),
        ("CORE_RULES_DEFAULT", CORE_RULES_DEFAULT, 90),
        ("STYLE_USAGE_LEAD", STYLE_USAGE_LEAD, 40),
        ("RESPONSE_ANCHOR", RESPONSE_ANCHOR, 60),
        ("MEMORY_CITATION_RULES", MEMORY_CITATION_RULES, 230),
        ("SOCIAL_CHAT_TONE_RULES", SOCIAL_CHAT_TONE_RULES, 288),
    ];
    for (name, text, limit) in cases {
        let chars = text.chars().count();
        assert!(
            chars <= *limit,
            "样板常量 {name} 超引导句上限 {limit}: 实际 {chars} 字符"
        );
    }
}

/// 压缩后保留关键指令（语义等价锁定——压缩只删引导措辞，不删行为约束）。
#[test]
fn compressed_boilerplate_keeps_essential_instructions() {
    // 边界约束关键词逐条保留（防止后续压缩误删"禁止照搬/不编造"等安全语义）
    assert!(
        UTT_LEAD.contains("禁止照搬内容"),
        "utt 引导须保留防照搬边界"
    );
    assert!(
        BRIDGE_LEAD.contains("不重复原文"),
        "桥接引导须保留防重复边界"
    );
    assert!(
        BRIDGE_LEAD.contains("不编造未提及的内容"),
        "桥接引导须保留防编造边界"
    );
    assert!(CAPACITY_INTRO.contains("不编造"), "能力边界须保留诚实约束");
    assert!(
        CAPACITY_INTRO.contains("不生成对他人有害"),
        "能力边界须保留安全约束"
    );
    assert!(
        MEMORY_CITATION_RULES.contains("你还记得"),
        "记忆引用规则须保留主动询问边界"
    );
    assert!(
        MEMORY_CITATION_RULES.contains("相关才引用")
            && MEMORY_CITATION_RULES.contains("打招呼或新话题不提及"),
        "记忆引用规则须保留引用时机边界"
    );
    // 记忆层引导保留"引用时机"约束
    assert!(MEMORY_SECTION_INTRO.contains("仅在话题相关时自然提及"));
}
