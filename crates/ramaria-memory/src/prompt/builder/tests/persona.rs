//! crates/ramaria-memory/src/prompt/builder/tests/persona.rs - 人格与角色层渲染
//!
//! 设计特点:
//! - 由 父测试模块 以 mod persona; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// ---- Role 块测试 ----

#[test]
fn role_with_persona() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("小明"), "角色名注入");
    assert!(
        result.contains("你是「小明」，有自己的脾气和说话习惯。"),
        "kind 行按身份事实渲染"
    );
    assert!(
        result.contains("场景：你在社交软件上和对方即时聊天"),
        "场景行注入"
    );
    assert!(result.contains("编程"), "背景描述注入");
    assert!(result.contains("emoji"), "手工风格注入");
    // 四层模板 markers
    assert!(result.contains("# 角色（行为层）"));
    assert!(result.contains("# 记忆（脉络层）"));
    assert!(result.contains("## 说话风格"));
    assert!(result.contains("# 当前时间"));
}

#[test]
fn role_without_persona_uses_default() {
    let ctx = PromptContext::default();
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);
    assert!(result.contains("Ramaria"));
    assert!(
        result.contains("场景：你在社交软件上和对方即时聊天"),
        "默认身份含场景行"
    );
    assert!(result.contains("你有自己的说话习惯"), "默认身份行");
}

/// 角色层与能力边界不再出现"AI 助手"身份声明（有 persona 与默认兜底两态）。
#[test]
fn role_and_capacity_never_claim_ai_assistant() {
    let contexts = [
        PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        },
        PromptContext::default(),
    ];
    for ctx in contexts {
        let result = assemble_prompt(&ctx, &PromptConfig::default());
        assert!(result.contains("# 能力边界"));
        assert!(result.contains("# 角色（行为层）"));
        assert!(
            !result.contains("AI 助手"),
            "角色层/能力边界不得含助手身份声明: {result}"
        );
    }
}

// ---- 表达层：自动风格规则注入（A3） ----

/// 构造无手工 speaking_style 的 persona（config 不含该字段）。
fn make_persona_without_manual_style() -> Persona {
    Persona {
        config: Some(r#"{"description":"一个喜欢编程的大学生"}"#.into()),
        ..make_test_persona()
    }
}

#[test]
fn auto_style_rule_injected_when_no_manual_style() {
    // 自动规则存在 + 无手工 speaking_style → 注入 `## 自动风格规则`
    let ctx = PromptContext {
        persona: Some(make_persona_without_manual_style()),
        style_rule_text: Some("你习惯使用口癖词「哇塞」，常聊「电影」等话题。".into()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        result.contains("## 自动风格规则"),
        "自动风格规则应注入: {result}"
    );
    assert!(result.contains("口癖词「哇塞」"), "规则文本在 prompt 中");
    assert!(
        result.contains("（以下是你的说话习惯。要求：按其表达，不复述该段文字。）"),
        "自动风格规则带说话习惯引导行"
    );
    assert!(
        !result.contains("## 说话风格\n"),
        "无手工风格时不产生手工子段"
    );
}

#[test]
fn manual_style_overrides_auto_rule() {
    // 手工 speaking_style 存在 → 只注入手工，自动规则被覆盖（手工优先，D-V17-004）
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        style_rule_text: Some("自动规则文本不应出现".into()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(result.contains("## 说话风格"), "手工风格子段存在");
    assert!(result.contains("热情活泼"), "手工风格内容注入");
    assert!(
        result.contains("（以下是你的说话习惯。要求：按其表达，不复述该段文字。）"),
        "手工风格带说话习惯引导行"
    );
    assert!(
        !result.contains("## 自动风格规则"),
        "手工覆盖优先，自动规则不注入: {result}"
    );
    assert!(!result.contains("自动规则文本不应出现"));
}

// ---- TOML 形态 persona.config（persona.toml 原文）读取 ----

/// TOML 形态配置：覆盖 `[identity].description`（背景行）与
/// `[blocks].speaking_style`（手工说话风格）两个可选数据源。
const TOML_PERSONA_CONFIG: &str = r#"[identity]
    assistant_name = "黎杋枫"
    user_name = "用户"
    description = "知性稳重的学习伙伴"

    [blocks]

    A_persona = """
    你是黎杋枫。性格知性稳重。
    """

    speaking_style = """
    说话简短，偶尔用冷幽默。
    """
    "#;

/// 构造 TOML 形态 config 的 persona（无 description 列值）。
fn make_persona_with_toml_config() -> Persona {
    Persona {
        config: Some(TOML_PERSONA_CONFIG.into()),
        description: None,
        ..make_test_persona()
    }
}

/// 构造 JSON 形态 config 且不含 description 的 persona（背景行"无来源"态）。
fn make_persona_without_description() -> Persona {
    Persona {
        config: Some(r#"{"speaking_style":"热情活泼，喜欢用emoji"}"#.into()),
        description: None,
        ..make_test_persona()
    }
}

/// 构造指定 kind 与 config 的 persona（Anim/Hist 数据源用例）。
fn persona_with_kind(kind: PersonaKind, config: Option<&str>) -> Persona {
    Persona {
        kind,
        config: config.map(str::to_string),
        description: None,
        ..make_test_persona()
    }
}

/// TOML 形态 config 端到端：`[identity].description` → `背景：` 行；
/// `[blocks].speaking_style` → `## 说话风格` 段。
#[test]
fn toml_config_renders_identity_description_and_speaking_style() {
    let ctx = PromptContext {
        persona: Some(make_persona_with_toml_config()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());

    assert!(
        result.contains("背景：知性稳重的学习伙伴"),
        "TOML [identity].description 应渲染背景行: {result}"
    );
    assert!(
        result.contains("## 说话风格"),
        "TOML [blocks].speaking_style 应渲染说话风格段"
    );
    assert!(
        result.contains("说话简短，偶尔用冷幽默。"),
        "说话风格正文注入"
    );
    assert!(
        !result.contains("## 自动风格规则"),
        "手工风格存在时不注入自动风格规则"
    );
}

/// 背景行数据源三态：`description` 列优先 → TOML 键 → 均无则不渲染。
#[test]
fn role_background_source_priority_column_then_toml_then_absent() {
    let config = PromptConfig::default();

    // ① 列值优先于 TOML [identity].description
    let mut persona_with_column = make_persona_with_toml_config();
    persona_with_column.description = Some("列上的描述".into());
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(persona_with_column),
            ..Default::default()
        },
        &config,
    );
    assert!(result.contains("背景：列上的描述"), "列值优先: {result}");
    assert!(
        !result.contains("背景：知性稳重的学习伙伴"),
        "列值命中后不再渲染 TOML 描述"
    );

    // ② 无列值 → TOML 键
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(make_persona_with_toml_config()),
            ..Default::default()
        },
        &config,
    );
    assert!(result.contains("背景：知性稳重的学习伙伴"), "TOML 键生效");

    // ③ 三处来源均无 → 不渲染背景行
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(make_persona_without_description()),
            ..Default::default()
        },
        &config,
    );
    assert!(
        !result.contains("背景："),
        "无描述来源时不渲染背景行: {result}"
    );
}

/// JSON 形态 config 的历史兼容：`description` 仍渲染为背景行。
#[test]
fn role_json_config_description_still_rendered_for_compatibility() {
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        },
        &PromptConfig::default(),
    );
    assert!(
        result.contains("背景：一个喜欢编程的大学生"),
        "JSON 兼容分支保留: {result}"
    );
}

/// Anim/Hist 身份行读取 `[identity].work` / `[identity].era`，缺失回退通用句。
#[test]
fn role_anim_hist_read_identity_work_era_with_fallback() {
    let config = PromptConfig::default();
    let anim_toml = "[identity]\nassistant_name = \"小绿\"\nwork = \"某作品\"\n\n[blocks]\nA_persona = \"\"\"角色设定\"\"\"\n";
    let hist_toml = "[identity]\nassistant_name = \"阿史\"\nera = \"唐朝\"\n\n[blocks]\nA_persona = \"\"\"角色设定\"\"\"\n";

    let render = |persona: Persona| {
        assemble_prompt(
            &PromptContext {
                persona: Some(persona),
                ..Default::default()
            },
            &config,
        )
    };

    // Anim：有 work → 作品名句
    let result = render(persona_with_kind(PersonaKind::Anim, Some(anim_toml)));
    assert!(
        result.contains("你是《某作品》中的「小明」，说话的语气就是你的语气。"),
        "Anim 作品名注入: {result}"
    );
    // Anim：无 config → 回退句
    let result = render(persona_with_kind(PersonaKind::Anim, None));
    assert!(
        result.contains("你是「小明」，说话的语气就是你的语气。"),
        "Anim 缺 work 回退: {result}"
    );

    // Hist：有 era → 时代句
    let result = render(persona_with_kind(PersonaKind::Hist, Some(hist_toml)));
    assert!(
        result.contains("你是唐朝的「小明」，说话的语气符合你的身份和时代。"),
        "Hist 时代注入: {result}"
    );
    // Hist：无 config → 回退句
    let result = render(persona_with_kind(PersonaKind::Hist, None));
    assert!(
        result.contains("你是「小明」，说话的语气符合你的身份和时代。"),
        "Hist 缺 era 回退: {result}"
    );
}

/// 生产 persona（`config/personas/rama-0001.toml`）的显式规则形态锁定：
/// `E_rules` 存在 → 核心规则 4 条（含"讲解"）且不注入共享规则的示例段
/// （有意设计，防格式契约回归误判）。
#[test]
fn production_persona_explicit_rules_override_shared_examples() {
    let persona_config = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/personas/rama-0001.toml"
    ));
    let rules = crate::init::resolve_chat_style_rules(Some(persona_config));

    assert!(rules.contains("1. 分条"), "显式规则第 1 条: {rules}");
    assert!(rules.contains("2. 接话"), "显式规则第 2 条");
    assert!(rules.contains("3. 收尾"), "显式规则第 3 条");
    assert!(rules.contains("4. 讲解"), "显式规则第 4 条（讲解）");
    assert!(
        !rules.contains("示例（模仿长度与分条方式"),
        "显式规则不包含共享规则示例段"
    );

    // 端到端：显式规则（经 resolve_chat_style_rules 解析）进入 `## 回复规范` 核心规则段
    let persona = Persona {
        uid: "rama-0001".into(),
        name: "Ramaria".into(),
        kind: PersonaKind::Rama,
        config: Some(persona_config.into()),
        description: None,
        ..make_test_persona()
    };
    let result = assemble_prompt(
        &PromptContext {
            persona: Some(persona),
            chat_style_rules: Some(rules),
            ..Default::default()
        },
        &PromptConfig::default(),
    );
    assert!(result.contains("4. 讲解"), "核心规则段含显式第 4 条");
    assert!(!result.contains("先去吃饭"), "不注入共享规则示例");
}

#[test]
fn no_style_rule_keeps_v16_prompt_equivalent() {
    // style_rule_text=None（风格关闭/数据不足）→ prompt 与 v1.6 语义等价
    // （不产生 `## 自动风格规则` 段落，回归红线 1）
    let ctx = PromptContext {
        persona: Some(make_persona_without_manual_style()),
        style_rule_text: None,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        !result.contains("## 自动风格规则"),
        "无自动规则时 prompt 与 v1.6 语义等价: {result}"
    );
}

#[test]
fn blank_style_rule_treated_as_missing() {
    // 空白规则文本（防御）→ 不注入
    let ctx = PromptContext {
        persona: Some(make_persona_without_manual_style()),
        style_rule_text: Some("   ".into()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(!result.contains("## 自动风格规则"));
}
