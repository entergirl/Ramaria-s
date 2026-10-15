//! crates/ramaria-memory/src/prompt/builder/tests/patch.rs - 提示词补丁注入
//!
//! 设计特点:
//! - 由 父测试模块 以 mod patch; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 覆盖角色层 / 表达层后缀的追加位置、空值按未配置的逐字节等价与 JSON 历史形态兼容。
//! - 用例为确定性断言，可离线运行。

use super::*;

/// 无 [prompt] 节的基础 TOML 配置（身份 + 自由文本块）。
const BASE_TOML: &str = r#"[identity]
assistant_name = "小明"

[blocks]
A_persona = """
角色设定文本。
"""
"#;

/// 构造带 TOML 形态 config 的 persona（可含 [prompt] 补丁节）。
fn persona_with_toml_config(config: &str) -> Persona {
    Persona {
        config: Some(config.to_string()),
        description: None,
        ..make_test_persona()
    }
}

/// 按下述配置渲染（含对话示例，供表达层位置断言使用）。
fn render_with_examples(config: &str) -> String {
    assemble_prompt(
        &PromptContext {
            persona: Some(persona_with_toml_config(config)),
            examples: vec![make_test_example("在吗", "在的")],
            ..Default::default()
        },
        &PromptConfig::default(),
    )
}

// ---- 角色层后缀 ----

/// `[prompt].role_suffix` 追加在角色层末尾（回复格式之后、下一层之前）。
#[test]
fn role_suffix_appended_at_role_layer_end() {
    let config = format!("{BASE_TOML}\n[prompt]\nrole_suffix = \"记得说话带点停顿感\"\n");
    let result = render_with_examples(&config);

    let suffix_pos = result
        .find("记得说话带点停顿感")
        .expect("role_suffix 应注入");
    let rules_pos = result.find("## 回复格式").expect("回复格式段应存在");
    let next_pos = result.find("# 说话风格（表达层）").expect("表达层应存在");

    assert!(suffix_pos > rules_pos, "role_suffix 应在回复格式之后");
    assert!(suffix_pos < next_pos, "role_suffix 应在表达层之前");
}

// ---- 表达层后缀 ----

/// `[prompt].style_suffix` 追加在表达层末尾（对话示例之后、下一层之前）。
#[test]
fn style_suffix_appended_at_style_layer_end() {
    let config = format!("{BASE_TOML}\n[prompt]\nstyle_suffix = \"少用书面词\"\n");
    let result = render_with_examples(&config);

    let suffix_pos = result.find("少用书面词").expect("style_suffix 应注入");
    let examples_pos = result.find("## 对话示例").expect("对话示例段应存在");
    let memory_pos = result.find("# 记忆（脉络层）").expect("记忆层应存在");

    assert!(suffix_pos > examples_pos, "style_suffix 应在对话示例之后");
    assert!(suffix_pos < memory_pos, "style_suffix 应在记忆层之前");
    assert!(
        result.contains("# 说话风格（表达层）"),
        "表达层标题保持稳定"
    );
}

/// 两个后缀同时配置 → 各自落在所属层，角色层后缀早于表达层标题。
#[test]
fn both_suffixes_render_at_own_layers() {
    let config =
        format!("{BASE_TOML}\n[prompt]\nrole_suffix = \"角色补丁\"\nstyle_suffix = \"风格补丁\"\n");
    let result = render_with_examples(&config);

    let role_pos = result.find("角色补丁").expect("role_suffix 应注入");
    let style_suffix_pos = result.find("风格补丁").expect("style_suffix 应注入");
    let rules_pos = result.find("## 回复格式").expect("回复格式段应存在");
    let style_title_pos = result.find("# 说话风格（表达层）").expect("表达层应存在");
    let examples_pos = result.find("## 对话示例").expect("对话示例段应存在");

    assert!(role_pos > rules_pos, "role_suffix 应在回复格式之后");
    assert!(role_pos < style_title_pos, "role_suffix 应在表达层之前");
    assert!(
        style_suffix_pos > examples_pos,
        "style_suffix 应在对话示例之后"
    );
}

// ---- 空值按未配置 ----

/// 同一 persona：无 [prompt] 节 vs 键值为空串 / 纯空白 → 渲染输出逐字节相等。
#[test]
fn empty_prompt_section_is_byte_identical_to_absent() {
    let no_patch = BASE_TOML.to_string();
    let empty_patch = format!("{BASE_TOML}\n[prompt]\nrole_suffix = \"\"\nstyle_suffix = \"\"\n");
    let blank_patch =
        format!("{BASE_TOML}\n[prompt]\nrole_suffix = \"   \"\nstyle_suffix = \"  \"\n");

    let baseline = render_with_examples(&no_patch);
    assert_eq!(
        baseline,
        render_with_examples(&empty_patch),
        "空串补丁按未配置（逐字节相等）"
    );
    assert_eq!(
        baseline,
        render_with_examples(&blank_patch),
        "空白补丁按未配置（逐字节相等）"
    );
}

// ---- 历史形态兼容 ----

/// JSON 形态 config（历史写入形态）无 [prompt] 节：按无补丁处理，渲染不 panic。
#[test]
fn json_config_renders_without_patch() {
    let persona = make_test_persona();
    assert!(
        super::super::role::persona_patch(Some(&persona)).is_none(),
        "JSON 配置解析失败按无补丁"
    );
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(persona),
            ..Default::default()
        },
        &PromptConfig::default(),
    );
    assert!(result.contains("背景：一个喜欢编程的大学生"));
    assert!(result.contains("热情活泼"));
}

// ---- 单独后缀与消融开关 ----

/// 仅 `style_suffix`（无手工风格 / 无示例）→ 表达层仍以「标题 + 后缀」形态出现。
#[test]
fn style_suffix_alone_creates_style_layer() {
    let config = format!("{BASE_TOML}\n[prompt]\nstyle_suffix = \"说话别太长\"\n");
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(persona_with_toml_config(&config)),
            ..Default::default()
        },
        &PromptConfig::default(),
    );

    let title_pos = result.find("# 说话风格（表达层）").expect("表达层应出现");
    let suffix_pos = result.find("说话别太长").expect("style_suffix 应注入");
    assert!(suffix_pos > title_pos, "后缀在表达层标题之后");
    assert!(
        !result.contains("## 说话风格\n"),
        "无手工风格不产生手工子段"
    );
    assert!(!result.contains("## 对话示例"), "无示例不产生示例子段");
}

/// 补丁不受表达层 / 示例 / 社交基调等消融开关影响（只看是否配置）。
#[test]
fn suffix_survives_ablation_switches() {
    let config =
        format!("{BASE_TOML}\n[prompt]\nrole_suffix = \"角色补丁\"\nstyle_suffix = \"风格补丁\"\n");
    let prompt_config = PromptConfig {
        include_speaking_style: false,
        include_examples: false,
        include_social_tone: false,
        ..Default::default()
    };
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(persona_with_toml_config(&config)),
            examples: vec![make_test_example("在吗", "在的")],
            ..Default::default()
        },
        &prompt_config,
    );

    assert!(result.contains("角色补丁"), "role_suffix 不随消融关闭");
    assert!(result.contains("风格补丁"), "style_suffix 不随消融关闭");
    assert!(!result.contains("## 对话示例"), "示例消融仍生效");
}
