//! crates/ramaria-memory/src/prompt/builder/tests/utt_bridge.rs - utt 原文片段与桥接
//!
//! 设计特点:
//! - 由 父测试模块 以 mod utt_bridge; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// utt 原文片段
// =========================================================

fn utt_hit(id: i64, persona: &str, text: &str, score: f64) -> crate::retriever::UttHit {
    crate::retriever::UttHit {
        doc: crate::retriever::UttDocView {
            id,
            persona_uid: persona.to_string(),
            session_id: uuid::Uuid::new_v4(),
            block_text: text.to_string(),
            msg_count: 2,
            created_at: 1000,
        },
        score,
        channel: "vector",
    }
}

#[test]
fn render_utt_context_keeps_all_within_budget() {
    let hits = vec![
        utt_hit(1, "char-0001", "今天天气真好", 0.9),
        utt_hit(2, "char-0001", "晚上吃火锅", 0.8),
    ];
    let out = render_utt_context(&hits, 500);
    assert!(out.contains("今天天气真好"));
    assert!(out.contains("晚上吃火锅"));
    assert!(
        out.contains(
            "

"
        ),
        "块间空行分隔"
    );
}

#[test]
fn render_utt_context_trims_by_budget_keeping_high_score() {
    // 预算只够一块：高分的保留，低分的整块丢弃
    // 块1 9 字符 ≤ 预算 10；块1+块2 = 9+5+2(空行) > 10 → 块2 被丢
    let hits = vec![
        utt_hit(1, "char-0001", "第一块内容很长很长", 0.9),
        utt_hit(2, "char-0001", "第二块内容", 0.8),
    ];
    let out = render_utt_context(&hits, 10);
    assert!(out.contains("第一块"), "高分块保留");
    assert!(!out.contains("第二块"), "超预算整块丢弃");
}

#[test]
fn render_utt_context_first_block_over_budget_yields_empty() {
    let hits = vec![utt_hit(1, "char-0001", "超长块内容", 0.9)];
    let out = render_utt_context(&hits, 2);
    assert!(out.is_empty(), "首块即超预算 → 不注入");
}

#[test]
fn render_utt_context_empty_hits_yields_empty() {
    assert!(render_utt_context(&[], 100).is_empty());
}

#[test]
fn render_utt_context_skips_blank_blocks() {
    let hits = vec![
        utt_hit(1, "char-0001", "   ", 0.9),
        utt_hit(2, "char-0001", "有效内容", 0.8),
    ];
    let out = render_utt_context(&hits, 100);
    assert!(out.contains("有效内容"));
    assert!(
        !out.contains(
            "

"
        ),
        "空白块被跳过不产生空段"
    );
}

#[test]
fn assemble_prompt_includes_utt_section_only_when_present() {
    // 无原文片段 → prompt 不含【原文片段】段落（白名单外/未命中）
    let ctx = PromptContext::default();
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(!result.contains("原文片段"), "无原文时不产生段落");

    // 有原文片段 → 段落出现
    let ctx2 = PromptContext {
        utt_context: Some("这是角色原话内容".to_string()),
        ..Default::default()
    };
    let result2 = assemble_prompt(&ctx2, &PromptConfig::default());
    assert!(result2.contains("## 原文片段"), "原文片段段落应出现");
    assert!(result2.contains("这是角色原话内容"));
}

/// 桥接内容存在时产生【桥接（上一会话尾部）】段落；
/// 缺失/空白时不产生段落（白名单外不注入）。
#[test]
fn assemble_prompt_includes_bridge_section_only_when_present() {
    // 无桥接内容 → 不产生段落
    let ctx = PromptContext::default();
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        !result.contains("桥接（上一会话尾部）"),
        "无桥接时不产生段落"
    );

    // 空白内容 → 不产生段落（防御）
    let ctx_blank = PromptContext {
        bridge_context: Some("   ".to_string()),
        ..Default::default()
    };
    let result_blank = assemble_prompt(&ctx_blank, &PromptConfig::default());
    assert!(
        !result_blank.contains("## 桥接"),
        "空白桥接内容不应产生段落"
    );

    // 有桥接内容 → 段落出现，含衔接说明与原文
    let ctx2 = PromptContext {
        bridge_context: Some("[2026-08-01 20:00] 角色: 上次聊到这里".to_string()),
        ..Default::default()
    };
    let result2 = assemble_prompt(&ctx2, &PromptConfig::default());
    assert!(
        result2.contains("## 桥接（上一会话尾部）"),
        "桥接段落应出现"
    );
    assert!(result2.contains("延续该话题继续对话"), "应含衔接用途说明");
    assert!(result2.contains("上次聊到这里"), "应含桥接原文内容");
}

/// 桥接与原文片段并存时两个段落都渲染（互不覆盖）。
#[test]
fn assemble_prompt_bridge_and_utt_coexist() {
    let ctx = PromptContext {
        utt_context: Some("原文片段内容".to_string()),
        bridge_context: Some("桥接内容".to_string()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(result.contains("## 原文片段"), "原文片段段落应保留");
    assert!(result.contains("## 桥接（上一会话尾部）"), "桥接段落应出现");
    assert!(result.contains("原文片段内容") && result.contains("桥接内容"));
}
