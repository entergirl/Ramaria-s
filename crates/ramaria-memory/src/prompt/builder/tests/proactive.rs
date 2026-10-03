//! crates/ramaria-memory/src/prompt/builder/tests/proactive.rs - 主动开口段渲染
//!
//! 设计特点:
//! - 由 父测试模块 以 mod proactive; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 覆盖主动段条件输出（None 零输出）、六时段文案差异、可选行渲染与部件数回归。
//! - 用例为确定性断言，可离线运行。

use super::*;
use ramaria_core::time_period::TimePeriod;

/// 构造带主动上下文的装配输入。
fn proactive_ctx(proactive: ProactivePromptContext) -> PromptContext {
    PromptContext {
        persona: Some(make_test_persona()),
        proactive_context: Some(proactive),
        ..Default::default()
    }
}

#[test]
fn proactive_block_renders_directive_and_anchor() {
    let ctx = proactive_ctx(ProactivePromptContext {
        time_period: TimePeriod::LateNight,
        anchor: Some("上周提过想去看展".into()),
        angle: Some("关心".into()),
        tone: Some("轻松".into()),
    });
    let result = assemble_prompt(&ctx, &PromptConfig::default());

    assert!(result.contains("# 主动开口"), "主动指令段标题缺失");
    assert!(result.contains("上周提过想去看展"), "锚点未注入");
    assert!(result.contains("## 此刻情境"), "此刻情境标题缺失");
    assert!(result.contains("对方可能已经休息"), "深夜时段文案未注入");
}

#[test]
fn proactive_block_absent_by_default() {
    let default_ctx = PromptContext {
        persona: Some(make_test_persona()),
        ..Default::default()
    };
    let default_out = assemble_prompt(&default_ctx, &PromptConfig::default());
    assert!(!default_out.contains("# 主动开口"));

    // 显式 None 与缺省构造输出逐字节一致
    let explicit_none = PromptContext {
        persona: Some(make_test_persona()),
        proactive_context: None,
        ..Default::default()
    };
    let none_out = assemble_prompt(&explicit_none, &PromptConfig::default());
    assert_eq!(default_out, none_out, "显式 None 不得改变既有输出");
    assert!(!none_out.contains("## 此刻情境"));
}

#[test]
fn proactive_time_period_scenes_differ() {
    let periods = [
        TimePeriod::EarlyMorning,
        TimePeriod::Morning,
        TimePeriod::Afternoon,
        TimePeriod::Evening,
        TimePeriod::Night,
        TimePeriod::LateNight,
    ];
    let outputs: Vec<String> = periods
        .iter()
        .map(|&period| {
            let ctx = proactive_ctx(ProactivePromptContext {
                time_period: period,
                anchor: None,
                angle: None,
                tone: None,
            });
            assemble_prompt(&ctx, &PromptConfig::default())
        })
        .collect();

    // 六份输出两两不同（时段文案各异）
    for (i, left) in outputs.iter().enumerate() {
        for right in outputs.iter().skip(i + 1) {
            assert_ne!(left, right, "不同时段的装配输出必须不同");
        }
    }

    // 各时段包含对应文案
    assert!(outputs[0].contains("刚醒来"), "清晨文案: {}", outputs[0]);
    assert!(outputs[1].contains("现在是上午"), "上午文案缺失");
    assert!(outputs[2].contains("现在是下午"), "下午文案缺失");
    assert!(outputs[3].contains("轻松地聊两句"), "傍晚文案缺失");
    assert!(outputs[4].contains("对方可能在放松"), "夜间文案缺失");
    assert!(outputs[5].contains("对方可能已经休息"), "深夜文案缺失");
}

#[test]
fn proactive_optional_lines_follow_inputs() {
    // 只有 anchor：角度 / 语气行不渲染
    let anchor_only = proactive_ctx(ProactivePromptContext {
        time_period: TimePeriod::Night,
        anchor: Some("想问问晚饭吃了没".into()),
        angle: None,
        tone: None,
    });
    let out = assemble_prompt(&anchor_only, &PromptConfig::default());
    assert!(out.contains("本次想到的事：想问问晚饭吃了没"));
    assert!(!out.contains("开口角度："));
    assert!(!out.contains("说话语气："));

    // 空白串与 None 同义：对应行同样不渲染
    let blank_fields = proactive_ctx(ProactivePromptContext {
        time_period: TimePeriod::Night,
        anchor: Some("   ".into()),
        angle: Some("\t\n".into()),
        tone: Some("  ".into()),
    });
    let out_blank = assemble_prompt(&blank_fields, &PromptConfig::default());
    assert!(!out_blank.contains("本次想到的事："));
    assert!(!out_blank.contains("开口角度："));
    assert!(!out_blank.contains("说话语气："));
    // 主动段本身仍产生（指令 + 此刻情境）
    assert!(out_blank.contains("# 主动开口"));
    assert!(out_blank.contains("## 此刻情境"));
}

#[test]
fn render_prompt_parts_still_has_seven_parts() {
    let ctx = proactive_ctx(ProactivePromptContext {
        time_period: TimePeriod::Morning,
        anchor: Some("昨天说想看电影".into()),
        angle: None,
        tone: None,
    });
    let parts = render_prompt_parts(&ctx, &PromptConfig::default());
    assert_eq!(parts.len(), 7, "主动段并入角色层，部件数保持 7");
    assert!(
        parts[1].content.contains("# 主动开口"),
        "主动段应位于角色层部件（索引 1）"
    );
}
