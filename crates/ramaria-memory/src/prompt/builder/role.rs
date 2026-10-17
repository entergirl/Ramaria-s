//! crates/ramaria-memory/src/prompt/builder/role.rs - 底线与角色层（行为层）构建
//!
//! 设计特点:
//! - `build_bottom_line`: 底线子段（诚实/安全约束恒常注入 + 自定义知识边界追加）
//! - `build_role`: 角色身份段（场景行 + 身份行 + 背景描述），解析 persona.toml / JSON 兼容
//! - `build_role_layer`: 角色身份 + 底线 + 性格 + 关于对方 + 说话方式/规则/格式
//! - 性格/事实格式化按 layer 与 ProfileField 分组，条件边界缺失时省略对应段
//! - 纯字符串拼接，无 I/O 与 LLM 依赖

use ramaria_core::types::{Persona, PersonaFact, PersonalityTrait, ProfileField, TraitStatus};

use super::experiment::build_experiment_section;
use super::{BOTTOM_LINE, PromptConfig, PromptContext, ROLE_DEFAULT_TEXT};

// =========================================================
// 底线子段（角色段内）
// =========================================================

/// 组装底线子段（`## 底线`）：诚实与安全约束；自定义知识边界非空时追加为末行。
///
/// 说明:
/// - 固定红线恒常注入（不随作用域开关变化）；
/// - `include_knowledge_boundary` 控制自定义知识边界（`context.knowledge_boundary`）的追加。
pub(super) fn build_bottom_line(context: &PromptContext, config: &PromptConfig) -> String {
    let custom = if config.include_knowledge_boundary {
        context
            .knowledge_boundary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    match custom {
        Some(boundary) => format!("{BOTTOM_LINE}\n{boundary}"),
        None => BOTTOM_LINE.to_string(),
    }
}

// =========================================================
// Role 块: 角色身份
// =========================================================

/// 助手（`Rama`）身份名缺失时的兜底名。
///
/// 说明:
/// - 正常路径下身份名跟随 `personas.name`（`rama-0001` 即产品自身身份的注册名）；
/// - 仅在注册名为空白（数据异常）时回退为产品名，避免渲染出空名字的身份行。
const ASSISTANT_FALLBACK_NAME: &str = "Ramaria";

/// 组装角色身份段：场景行 + 身份行（kind）+ 背景描述（`# 角色（行为层）` 的头部）。
///
/// 背景描述数据源（按优先级取值，均为空则不渲染该行）:
/// 1. `persona.description` 列（GUI 可编辑的简要描述）；
/// 2. `persona.config`（persona.toml 原文）`[identity].description` 可选键；
/// 3. `persona.config` 的 JSON `description`（历史兼容：早期写入过 JSON 形态配置）。
///
/// `Anim` / `Hist` 的身份行读取 `[identity].work` / `[identity].era`（用户自设可选键），
/// 缺失时回退通用句。
fn build_role(context: &PromptContext) -> String {
    if let Some(ref persona) = context.persona {
        let mut parts = vec![String::from(
            "# 角色（行为层）\n\
             场景：你在社交软件上和对方即时聊天（「对方」指正在和你聊天的人）。",
        )];

        let persona_toml = parse_persona_config(persona);

        // persona kind 描述（一行式身份事实；Anim/Hist 的作品名/时代名由
        // persona 设定数据源提供，缺失时使用回退句）
        let kind_desc = match persona.kind {
            // 产品自身身份（`rama-0001`）：身份名跟随人格注册名（`personas.name`），
            // 不再写死产品名；注册名为空白时回退产品名兜底。
            ramaria_core::types::PersonaKind::Rama => {
                let name = persona.name.trim();
                let name = if name.is_empty() {
                    ASSISTANT_FALLBACK_NAME
                } else {
                    name
                };
                format!("你是「{name}」，你有自己的说话习惯。")
            }
            ramaria_core::types::PersonaKind::User => {
                format!("你以「{}」的视角说话。", persona.name)
            }
            ramaria_core::types::PersonaKind::Char | ramaria_core::types::PersonaKind::Oc => {
                format!("你是「{}」，有自己的脾气和说话习惯。", persona.name)
            }
            ramaria_core::types::PersonaKind::Anim => match persona_toml
                .as_ref()
                .and_then(|parsed| parsed.work.as_deref())
            {
                Some(work) => {
                    format!(
                        "你是《{work}》中的「{}」，说话的语气就是你的语气。",
                        persona.name
                    )
                }
                None => format!("你是「{}」，说话的语气就是你的语气。", persona.name),
            },
            ramaria_core::types::PersonaKind::Hist => match persona_toml
                .as_ref()
                .and_then(|parsed| parsed.era.as_deref())
            {
                Some(era) => format!(
                    "你是{era}的「{}」，说话的语气符合你的身份和时代。",
                    persona.name
                ),
                None => format!("你是「{}」，说话的语气符合你的身份和时代。", persona.name),
            },
            _ => format!("你是「{}」，有自己的说话习惯。", persona.name),
        };
        parts.push(kind_desc);

        // 背景行：列值优先 → TOML [identity].description → JSON 兼容
        let description = persona
            .description
            .as_deref()
            .map(str::trim)
            .filter(|desc| !desc.is_empty())
            .map(str::to_string)
            .or_else(|| {
                persona_toml
                    .as_ref()
                    .and_then(|parsed| parsed.description.clone())
            })
            .or_else(|| {
                persona
                    .config
                    .as_deref()
                    .and_then(|cfg| serde_json::from_str::<serde_json::Value>(cfg).ok())
                    .and_then(|obj| {
                        obj.get("description")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .map(|desc| desc.trim().to_string())
                    .filter(|desc| !desc.is_empty())
            });
        if let Some(desc) = description {
            parts.push(format!("背景：{desc}"));
        }

        parts.join("\n")
    } else {
        ROLE_DEFAULT_TEXT.to_string()
    }
}

/// 解析 persona 配置原文为结构化 persona.toml（解析失败 / 非 TOML 形态 → `None`）。
///
/// 说明:
/// - `persona.config` 的真实存储形态是 persona.toml 原文（`persona reload` / 桌面端写入），
///   历史上有 JSON 形态写入，故调用方在 TOML 解析失败时仍需保留 JSON 兼容分支。
/// - 解析失败按"无配置"处理，不产生日志噪音（JSON 形态属预期输入）。
pub(super) fn parse_persona_config(persona: &Persona) -> Option<crate::init::PersonaToml> {
    persona
        .config
        .as_deref()
        .and_then(|cfg| crate::init::parse_persona_toml(cfg).ok())
}

/// 读取 persona 配置中的提示词补丁（`[prompt]` 节；无 persona / 解析失败 → `None`）。
///
/// 说明:
/// - 补丁为追加语义的纯文本后缀，空值在解析期已归一为未配置；
/// - 正文长度由解析期统一约束（超长截断），渲染侧不做二次截断。
pub(super) fn persona_patch(persona: Option<&Persona>) -> Option<crate::preset::PromptPatch> {
    persona
        .and_then(parse_persona_config)
        .map(|parsed| parsed.patch)
}

// =========================================================
// 角色层（行为层）: 角色身份 + 底线 + 性格 + 关于对方 + 说话与回复规则
// =========================================================

/// 组装角色层块（`# 角色（行为层）`）：角色身份 + 底线 + 性格 + 关于对方 + 说话与回复规则。
///
/// 对应 Role + Insight + Experiment 三块；
/// 行为规则槽位（情境-反应规则）由 `render_behavior_block` 在装配时挂载。
///
/// 提示词补丁:
/// - `[prompt].role_suffix` 非空时追加为末段（追加语义，不覆盖既有子段）；
///   未配置 → 不产生任何输出。
pub(super) fn build_role_layer(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = vec![build_role(context)];

    // 底线（诚实与安全约束；身份信息之后）
    parts.push(format!("\n\n{}", build_bottom_line(context, config)));

    // 性格标签（按 layer 分组；无 traits 时省略）
    if config.include_traits && !context.traits.is_empty() {
        let trait_text = format_traits_for_prompt(&context.traits, config.max_traits_per_layer);
        if !trait_text.is_empty() {
            parts.push(format!("\n\n## 性格\n{trait_text}"));
        }
    }

    // 关于对方（无 facts 时省略）
    if config.include_facts && !context.facts.is_empty() {
        let fact_text = format_facts_for_prompt(&context.facts);
        if !fact_text.is_empty() {
            parts.push(format!("\n\n## 关于对方\n{fact_text}"));
        }
    }

    // 说话与回复规则（说话方式 + 核心规则 + 回复格式；无自定义规则时使用中性默认）
    parts.push(build_experiment_section(context, config));

    // 主动开口场景（仅主动生成注入；None / 空 → 不产生段落，既有输出零变化）
    let proactive_block = super::proactive::build_proactive_block(context);
    if !proactive_block.is_empty() {
        parts.push(format!("\n\n{proactive_block}"));
    }

    // 提示词补丁（[prompt].role_suffix）：角色层末尾追加（不覆盖任何既有子段）；
    // 未配置 → 不产生任何输出（零字节变化），不随消融开关关闭
    if let Some(suffix) =
        persona_patch(context.persona.as_ref()).and_then(|patch| patch.role_suffix)
    {
        parts.push(format!("\n\n{suffix}"));
    }

    parts.join("")
}

/// 将性格标签格式化为 prompt 文本，按 layer 分组。
///
/// 标签行在含义后追加条件边界：`｜会在：{trigger}｜不会在：{suppress}`；
/// 触发/抑制条件缺失（None 或空白）时省略对应段，两者皆缺时保持原格式。
fn format_traits_for_prompt(traits: &[PersonalityTrait], max_per_layer: usize) -> String {
    use ramaria_core::types::TraitLayer;
    use std::collections::BTreeMap;

    // 按 layer 分组，只取 active 的
    let mut by_layer: BTreeMap<&str, Vec<&PersonalityTrait>> = BTreeMap::new();
    for t in traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        let layer_name = match t.layer {
            TraitLayer::Base => "基础性格",
            TraitLayer::Primary => "主要特征",
            TraitLayer::Accent => "次要特征",
            _ => "其他特征",
        };
        by_layer.entry(layer_name).or_default().push(t);
    }

    if by_layer.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    for (layer_name, layer_traits) in &by_layer {
        lines.push(format!("【{layer_name}】"));
        for t in layer_traits.iter().take(max_per_layer) {
            let mut desc = format!("  - {}", t.trait_label);
            if !t.meaning.is_empty() {
                desc.push_str(&format!("（{}）", t.meaning));
            }
            let trigger = t
                .trigger
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let Some(trigger) = trigger {
                desc.push_str(&format!("｜会在：{trigger}"));
            }
            let suppress = t
                .suppress
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let Some(suppress) = suppress {
                desc.push_str(&format!("｜不会在：{suppress}"));
            }
            lines.push(desc);
        }
    }

    lines.join("\n")
}

/// 将事实信息格式化为 prompt 文本，按 ProfileField 分组。
fn format_facts_for_prompt(facts: &[PersonaFact]) -> String {
    if facts.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    for fact in facts {
        let field_label = match fact.field {
            ProfileField::BasicInfo => "基础信息",
            ProfileField::PersonalStatus => "近期状态",
            ProfileField::Interests => "兴趣爱好",
            ProfileField::Social => "社交情况",
            ProfileField::History => "历史事件",
            ProfileField::RecentContext => "近期背景",
            ProfileField::SpeakingStyle => "说话风格",
            _ => "其他",
        };
        lines.push(format!("  {field_label}：{}", fact.content));
    }

    lines.join("\n")
}
