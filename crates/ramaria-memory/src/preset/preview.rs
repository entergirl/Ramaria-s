//! crates/ramaria-memory/src/preset/preview.rs - Ramaria 预设导入预览模块
//!
//! 设计特点:
//! - dry-run 预览构建：注册信息 / 将写入计数 / 跳过明细 / 拆解状态 / 提示
//! - 预览与实际写入共用同一解析产物（一致性承诺的数据源）
//! - 文本与 JSON 两形态；文本与 JSON 均不含自由文本块与描述原文
//! - 纯函数，零 I/O，不依赖数据库或异步运行时

use ramaria_core::types::PersonaKind;
use serde::Serialize;

use super::PresetFormat;
use super::model::{KindSource, ParsedPreset, PresetHint, SkipCategory, SkipReason};

// =========================================================
// 预览类型
// =========================================================

/// 预览模式（新建 / 合并到目标人格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewMode {
    /// 新建人格
    Create,
    /// 合并到目标人格
    Merge,
}

/// 预览构建选项（调用方输入：文件名、合并目标、拆解开关与隐私确认状态）。
#[derive(Debug, Clone, Default)]
pub struct PreviewOptions {
    /// 文件名（含扩展名）
    pub file_name: Option<String>,
    /// Some → Merge 模式（目标 uid）
    pub merge_target: Option<String>,
    /// 是否请求 LLM 拆解（--no-ai 的反面）
    pub decompose_enabled: bool,
    /// 隐私门禁确认状态（调用方查询）
    pub privacy_confirmed: bool,
}

/// 跳过项（预览形态；label 为条目标识，不是原文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkipItem {
    /// 条目标识（无则 None，JSON 中省略该键）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 跳过原因
    pub reason: SkipReason,
}

/// traits 段预览（三层计数与跳过明细）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraitsPreview {
    pub base: usize,
    pub primary: usize,
    pub accent: usize,
    pub skipped: Vec<SkipItem>,
}

/// 计数段预览（facts / examples / behavior_rules）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CountPreview {
    pub count: usize,
    pub skipped: Vec<SkipItem>,
}

/// 提示词补丁预览。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PromptPreview {
    pub role_suffix: bool,
    pub style_suffix: bool,
}

/// 自由文本块预览（仅段数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BlocksPreview {
    pub segments: usize,
}

/// 对话身份预览。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityPreview {
    pub assistant_name: String,
    pub user_name: Option<String>,
}

/// 将写入段预览。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WillWritePreview {
    pub traits: TraitsPreview,
    pub facts: CountPreview,
    pub examples: CountPreview,
    pub behavior_rules: CountPreview,
    pub prompt: PromptPreview,
    pub blocks: BlocksPreview,
    pub identity: IdentityPreview,
}

/// 注册信息预览。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistrationPreview {
    /// 生效 uid（合并目标 > 预设 uid；None = 待自动生成）
    pub uid: Option<String>,
    pub mode: PreviewMode,
    pub kind: Option<PersonaKind>,
    pub kind_source: KindSource,
    pub name: String,
    pub source: String,
    /// 是否提供头像
    pub avatar: bool,
    /// 是否提供描述
    pub description: bool,
}

/// 拆解状态预览。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DecomposePreview {
    pub enabled: bool,
    pub privacy_confirmed: bool,
}

/// dry-run 预览（字段名 snake_case，对齐 dry-run 输出形态规范）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PresetPreview {
    /// 文件名（含扩展名；None = 未提供）
    pub file: Option<String>,
    pub format: PresetFormat,
    pub registration: RegistrationPreview,
    pub will_write: WillWritePreview,
    pub decompose: DecomposePreview,
    pub hints: Vec<PresetHint>,
}

// =========================================================
// 构建
// =========================================================

/// 从解析产物构建预览（解析侧信息 + 调用方选项）。
pub fn build_preview(parsed: &ParsedPreset, options: &PreviewOptions) -> PresetPreview {
    let mode = if options.merge_target.is_some() {
        PreviewMode::Merge
    } else {
        PreviewMode::Create
    };
    let uid = options
        .merge_target
        .clone()
        .or_else(|| parsed.persona.uid.clone());

    PresetPreview {
        file: options.file_name.clone(),
        format: parsed.format,
        registration: RegistrationPreview {
            uid,
            mode,
            kind: parsed.persona.kind,
            kind_source: parsed.persona.kind_source,
            name: parsed.persona.name.clone(),
            source: parsed.persona.source.clone(),
            avatar: parsed.persona.avatar.is_some(),
            description: parsed.persona.description.is_some(),
        },
        will_write: WillWritePreview {
            traits: TraitsPreview {
                base: parsed.traits.base.len(),
                primary: parsed.traits.primary.len(),
                accent: parsed.traits.accent.len(),
                skipped: collect_skipped(parsed, SkipCategory::Traits),
            },
            facts: CountPreview {
                count: parsed.facts.len(),
                skipped: collect_skipped(parsed, SkipCategory::Facts),
            },
            examples: CountPreview {
                count: parsed.examples.len(),
                skipped: collect_skipped(parsed, SkipCategory::Examples),
            },
            behavior_rules: CountPreview {
                count: parsed.rules.len(),
                skipped: collect_skipped(parsed, SkipCategory::BehaviorRules),
            },
            prompt: PromptPreview {
                role_suffix: parsed.prompt.role_suffix.is_some(),
                style_suffix: parsed.prompt.style_suffix.is_some(),
            },
            blocks: BlocksPreview {
                segments: parsed.blocks.len(),
            },
            identity: IdentityPreview {
                assistant_name: parsed.identity.assistant_name.clone(),
                user_name: parsed.identity.user_name.clone(),
            },
        },
        decompose: DecomposePreview {
            enabled: options.decompose_enabled,
            privacy_confirmed: options.privacy_confirmed,
        },
        hints: parsed.hints.clone(),
    }
}

/// 按类别分拣跳过明细为预览项。
fn collect_skipped(parsed: &ParsedPreset, category: SkipCategory) -> Vec<SkipItem> {
    parsed
        .skipped
        .iter()
        .filter(|entry| entry.category == category)
        .map(|entry| SkipItem {
            label: entry.label.clone(),
            reason: entry.reason,
        })
        .collect()
}

// =========================================================
// 文本渲染
// =========================================================

impl PresetPreview {
    /// 渲染文本形态（CLI 展示；不含 blocks / description 原文）。
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let file = self.file.as_deref().unwrap_or("（未提供文件名）");
        out.push_str(&format!(
            "预设预览：{file}（格式：{}）\n",
            format_label(self.format)
        ));
        out.push_str(&self.render_registration());
        out.push_str(&self.render_will_write());
        out.push_str(&self.render_decompose());
        out.push_str(&self.render_hints());
        out
    }

    /// 注册行与人格行。
    fn render_registration(&self) -> String {
        let reg = &self.registration;
        let uid = reg.uid.as_deref().unwrap_or("（自动生成）");
        let mode = match reg.mode {
            PreviewMode::Create => "新建".to_string(),
            PreviewMode::Merge => format!("合并到 {}", reg.uid.as_deref().unwrap_or("")),
        };
        let kind = match reg.kind {
            Some(kind) => format!(
                "{}（{}）",
                kind.as_str(),
                kind_source_label(reg.kind_source)
            ),
            None => "未定".to_string(),
        };
        let mut out = format!(
            "注册：uid {uid}（{mode}）/ kind {kind} / name「{}」/ source {}\n",
            reg.name, reg.source
        );
        out.push_str(&format!(
            "      description {}（将写入列） / avatar {}\n",
            yes_no(reg.description),
            yes_no(reg.avatar)
        ));
        out.push_str(&format!("人格：{uid}（{mode}）\n"));
        out
    }

    /// 将写入段。
    fn render_will_write(&self) -> String {
        let will = &self.will_write;
        let mut out = String::from("将写入：\n");
        out.push_str(&section_line(
            "traits",
            &format!(
                "base {} / primary {} / accent {}（{}）",
                will.traits.base,
                will.traits.primary,
                will.traits.accent,
                skip_desc(&will.traits.skipped)
            ),
        ));
        out.push_str(&section_line(
            "facts",
            &format!(
                "{} 条（{}）",
                will.facts.count,
                skip_desc(&will.facts.skipped)
            ),
        ));
        out.push_str(&section_line(
            "examples",
            &format!(
                "{} 对（{}）",
                will.examples.count,
                skip_desc(&will.examples.skipped)
            ),
        ));
        out.push_str(&section_line(
            "behavior_rules",
            &format!(
                "{} 条（{}）",
                will.behavior_rules.count,
                skip_desc(&will.behavior_rules.skipped)
            ),
        ));
        out.push_str(&section_line(
            "prompt 补丁",
            &format!(
                "role_suffix {} / style_suffix {}",
                yes_no(will.prompt.role_suffix),
                yes_no(will.prompt.style_suffix)
            ),
        ));
        out.push_str(&section_line(
            "blocks",
            &format!("{} 段 → personas.config", will.blocks.segments),
        ));
        out.push_str(&section_line(
            "identity",
            &format!(
                "assistant_name「{}」/ user_name「{}」",
                will.identity.assistant_name,
                will.identity.user_name.as_deref().unwrap_or("未设置")
            ),
        ));
        out
    }

    /// 拆解状态行。
    fn render_decompose(&self) -> String {
        let text = if !self.decompose.enabled {
            "未启用（--no-ai）".to_string()
        } else if self.decompose.privacy_confirmed {
            "将调用 LLM 拆解 blocks → traits/facts".to_string()
        } else {
            "已请求，隐私未确认，将跳过（blocks 原文直存）".to_string()
        };
        format!("可选拆解：{text}\n")
    }

    /// 提示节（有提示才输出）。
    fn render_hints(&self) -> String {
        if self.hints.is_empty() {
            return String::new();
        }
        let mut out = String::from("提示：\n");
        for hint in &self.hints {
            out.push_str(&format!("  {}\n", hint.message));
        }
        out
    }
}

/// 段行渲染（段名定宽两空格缩进）。
fn section_line(name: &str, text: &str) -> String {
    format!("  {name:<14} {text}\n")
}

/// 有无标记。
fn yes_no(value: bool) -> &'static str {
    if value { "有" } else { "无" }
}

/// 格式标签。
fn format_label(format: PresetFormat) -> &'static str {
    match format {
        PresetFormat::Toml => "toml",
        PresetFormat::StJson => "ST-json",
        PresetFormat::StPng => "ST-png",
    }
}

/// kind 来源标签。
fn kind_source_label(source: KindSource) -> &'static str {
    match source {
        KindSource::Explicit => "显式",
        KindSource::Inferred => "由 uid 推断",
        KindSource::Default => "缺省",
    }
}

/// 跳过汇总：无跳过输出「跳过 0」，否则「跳过 N：原因「标识」…」。
fn skip_desc(items: &[SkipItem]) -> String {
    if items.is_empty() {
        return "跳过 0".to_string();
    }
    let details: Vec<String> = items
        .iter()
        .map(|item| match &item.label {
            Some(label) => format!("{}「{label}」", reason_label(item.reason)),
            None => reason_label(item.reason).to_string(),
        })
        .collect();
    format!("跳过 {}：{}", items.len(), details.join(" / "))
}

/// 跳过原因标签。
fn reason_label(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::LabelDuplicate => "label 重复",
        SkipReason::Duplicate => "重复",
        SkipReason::Invalid => "非法",
        SkipReason::Overflow => "超限",
    }
}
