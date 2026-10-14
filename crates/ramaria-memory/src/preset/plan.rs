//! crates/ramaria-memory/src/preset/plan.rs - Ramaria 预设导入计划模块
//!
//! 设计特点:
//! - 把解析产物与库侧现状快照裁决为「将写入 / 跳过」的单一计划数据源
//! - dry-run 预览与实写消费同一计划，保证「将写入」逐项一致
//! - 合并语义「只增不改」：注册列只补空缺、条目按内容判重、配置与提示词补丁默认跳过
//! - 纯函数，零 I/O，不依赖数据库或异步运行时

use std::collections::HashSet;

use ramaria_core::ProfileField;
use ramaria_core::types::PersonaKind;

use super::PresetFormat;
use super::model::{
    ExampleEntry, FactEntry, HintLevel, IdentitySection, KindSource, ParsedPreset, PresetHint,
    RuleEntry, SkipReason, SkippedEntry, TraitEntry,
};
use super::preview::PreviewMode;

// =========================================================
// 输入快照与注册决议
// =========================================================

/// 现有人格画像快照（调用方从存储读取后传入；全字段拼接成只读现状）。
#[derive(Debug, Clone, Default)]
pub struct ExistingProfile {
    /// 目标 uid 是否已有记录
    pub exists: bool,
    /// 现有记录的 kind（Merge 时保留，不可变更）
    pub kind: Option<PersonaKind>,
    /// 现有注册名（空串视同空缺）
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub description: Option<String>,
    /// 现有配置文本是否非空
    pub has_config: bool,
    /// 现有 traits 的 label（全部状态，跨层；判重用）
    pub trait_labels: Vec<String>,
    /// 现有 traits 各层最大 seq（索引 0/1/2 = Base/Primary/Accent；写入 seq 续号用）
    pub trait_max_seq: [i32; 3],
    /// 现有 facts 的 (field, content)（全部版本；判重用）
    pub fact_pairs: Vec<(ProfileField, String)>,
    /// 现有 examples 的 (partner, reply)（全部候选；判重用）
    pub example_pairs: Vec<(String, String)>,
    /// 现有 rules 的 (keywords 已排序去重, reaction)（含禁用；判重用）
    pub rule_keys: Vec<(Vec<String>, Option<String>)>,
    /// 库内全部 active persona 的 uid（uid 自动生成的序号来源）
    pub all_uids: Vec<String>,
}

/// 生效注册决议（uid / kind / kind 来源）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRegistration {
    pub uid: String,
    pub kind: PersonaKind,
    pub kind_source: KindSource,
}

/// 解析生效注册决议（单一来源；调用方读库快照与 plan 裁决共用同一函数）。
///
/// 优先级:
/// - uid: `merge_target` > `parsed.persona.uid` > 自动生成
/// - kind: `parsed.persona.kind` > 缺省 `Char`（`KindSource::Default`）
/// - 自动生成: `{kind}-{序号:04}`，序号 = `all_uids` 中匹配 `^{kind}-\d+$` 的最大数字 + 1
///
/// 参数:
/// - `parsed`: 预设解析产物。
/// - `merge_target`: 合并目标 uid（`--into`）。
/// - `all_uids`: 库内全部 active persona 的 uid。
///
/// 返回:
/// - 生效 uid / kind / kind 来源。
pub fn resolve_registration(
    parsed: &ParsedPreset,
    merge_target: Option<&str>,
    all_uids: &[String],
) -> ResolvedRegistration {
    let kind = parsed.persona.kind.unwrap_or(PersonaKind::Char);
    let uid = merge_target
        .map(str::to_string)
        .or_else(|| parsed.persona.uid.clone())
        .unwrap_or_else(|| generate_uid(kind, all_uids));
    ResolvedRegistration {
        uid,
        kind,
        kind_source: parsed.persona.kind_source,
    }
}

/// 生成 `{kind}-{序号:04}` 形式的 uid（同类最大序号 + 1）。
fn generate_uid(kind: PersonaKind, all_uids: &[String]) -> String {
    let prefix = format!("{}-", kind.as_str());
    let max_seq = all_uids
        .iter()
        .filter_map(|uid| uid.strip_prefix(&prefix))
        .filter_map(|seq| seq.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{:04}", max_seq + 1)
}

// =========================================================
// 计划选项与裁决类型
// =========================================================

/// 计划选项（调用方输入）。
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// 合并目标 uid（--into；Some 时以其为准）
    pub merge_target: Option<String>,
    /// 追加拆解条目（已由调用方执行 LLM 拆解；dry-run 时为空）
    pub decomposed: DecomposedEntries,
}

/// 拆解条目集合（LLM 拆解产物，由调用方转换后传入；不落库）。
#[derive(Debug, Clone, Default)]
pub struct DecomposedEntries {
    pub traits: Vec<TraitEntry>,
    pub facts: Vec<FactEntry>,
}

/// 条目裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryDecision {
    Write,
    Skip(SkipReason),
}

/// 条目裁决对（保留原条目与理由）。
#[derive(Debug, Clone, PartialEq)]
pub struct Planned<T> {
    pub entry: T,
    pub decision: EntryDecision,
}

/// 注册段计划。
#[derive(Debug, Clone, PartialEq)]
pub struct RegistrationPlan {
    pub uid: String,
    pub mode: PreviewMode,
    pub uid_exists: bool,
    pub kind: PersonaKind,
    pub kind_source: KindSource,
    pub name: String,
    pub source: String,
    pub avatar: Option<String>,
    pub description: Option<String>,
    /// 各列写入裁决（Merge 只补空缺；Create 全写）
    pub write_name: bool,
    pub write_avatar: bool,
    pub write_description: bool,
    /// personas.config 是否写入（Merge 且目标已有配置 → false）
    pub write_config: bool,
}

/// 提示词补丁裁决（补丁文本随 config 载体保存；config 不写则补丁不生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptPlan {
    pub write_role_suffix: bool,
    pub write_style_suffix: bool,
}

/// 导入计划（裁决单一数据源；预览与实写共用）。
#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub format: PresetFormat,
    pub registration: RegistrationPlan,
    pub traits: Vec<Planned<TraitEntry>>,
    pub facts: Vec<Planned<FactEntry>>,
    pub examples: Vec<Planned<ExampleEntry>>,
    pub rules: Vec<Planned<RuleEntry>>,
    pub prompt: PromptPlan,
    /// [blocks] 段数（预览用）
    pub blocks_segments: usize,
    /// 对话身份（预览用；透传 parsed.identity）
    pub identity: IdentitySection,
    /// 解析期跳过明细（透传 parsed.skipped，供预览合并展示）
    pub parsed_skipped: Vec<SkippedEntry>,
    /// 提示（解析期透传 + 库侧新增）
    pub hints: Vec<PresetHint>,
}

// =========================================================
// 计划构建
// =========================================================

/// 构建导入计划（纯函数；dry-run 预览与实写共用，保证「将写入」逐项一致）。
///
/// 参数:
/// - `parsed`: 预设解析产物。
/// - `resolved`: 生效注册决议（与库侧 uid 列表同源）。
/// - `existing`: 目标 uid 的库侧现状快照。
/// - `options`: 合并目标与拆解条目。
///
/// 返回:
/// - 注册段 / 四类条目 / 提示词补丁的逐项裁决与提示。
pub fn plan_import(
    parsed: &ParsedPreset,
    resolved: &ResolvedRegistration,
    existing: &ExistingProfile,
    options: &PlanOptions,
) -> ImportPlan {
    let merge = existing.exists;
    let registration = plan_registration(parsed, resolved, existing, merge);
    let (traits, facts, examples, rules) = plan_entries(parsed, options, existing, merge);
    let prompt = PromptPlan {
        write_role_suffix: registration.write_config && parsed.prompt.role_suffix.is_some(),
        write_style_suffix: registration.write_config && parsed.prompt.style_suffix.is_some(),
    };

    let mut hints = parsed.hints.clone();
    push_library_hints(&mut hints, parsed, existing, options, &registration, merge);

    ImportPlan {
        format: parsed.format,
        registration,
        traits,
        facts,
        examples,
        rules,
        prompt,
        blocks_segments: parsed.blocks.len(),
        identity: parsed.identity.clone(),
        parsed_skipped: parsed.skipped.clone(),
        hints,
    }
}

/// 注册段裁决：模式 / uid 生效状态 / kind 保留 / 各列只补空缺 / config 写入。
fn plan_registration(
    parsed: &ParsedPreset,
    resolved: &ResolvedRegistration,
    existing: &ExistingProfile,
    merge: bool,
) -> RegistrationPlan {
    let kind = match (merge, existing.kind) {
        (true, Some(kind)) => kind,
        _ => resolved.kind,
    };
    let write_name = if merge {
        is_blank(existing.name.as_deref())
    } else {
        true
    };
    let write_avatar = if merge {
        is_blank(existing.avatar.as_deref()) && parsed.persona.avatar.is_some()
    } else {
        parsed.persona.avatar.is_some()
    };
    let write_description = if merge {
        is_blank(existing.description.as_deref()) && parsed.persona.description.is_some()
    } else {
        parsed.persona.description.is_some()
    };

    RegistrationPlan {
        uid: resolved.uid.clone(),
        mode: if merge {
            PreviewMode::Merge
        } else {
            PreviewMode::Create
        },
        uid_exists: existing.exists,
        kind,
        kind_source: resolved.kind_source,
        name: parsed.persona.name.clone(),
        source: parsed.persona.source.clone(),
        avatar: parsed.persona.avatar.clone(),
        description: parsed.persona.description.clone(),
        write_name,
        write_avatar,
        write_description,
        write_config: !merge || !existing.has_config,
    }
}

/// 四类条目裁决集合（traits / facts / examples / rules）。
type PlannedEntries = (
    Vec<Planned<TraitEntry>>,
    Vec<Planned<FactEntry>>,
    Vec<Planned<ExampleEntry>>,
    Vec<Planned<RuleEntry>>,
);

/// 四类条目裁决：先直写（层序 / 列表序），后拆解条目。
fn plan_entries(
    parsed: &ParsedPreset,
    options: &PlanOptions,
    existing: &ExistingProfile,
    merge: bool,
) -> PlannedEntries {
    let mut seen_labels: HashSet<String> = if merge {
        existing.trait_labels.iter().cloned().collect()
    } else {
        HashSet::new()
    };
    let mut traits = Vec::new();
    for entry in parsed
        .traits
        .base
        .iter()
        .chain(parsed.traits.primary.iter())
        .chain(parsed.traits.accent.iter())
        .chain(options.decomposed.traits.iter())
    {
        traits.push(plan_trait_entry(entry, &mut seen_labels));
    }

    let mut seen_facts: HashSet<(&'static str, String)> = if merge {
        existing
            .fact_pairs
            .iter()
            .map(|(field, content)| fact_key(*field, content))
            .collect()
    } else {
        HashSet::new()
    };
    let mut facts = Vec::new();
    for entry in parsed.facts.iter().chain(options.decomposed.facts.iter()) {
        facts.push(plan_fact_entry(entry, &mut seen_facts));
    }

    let mut seen_examples: HashSet<(String, String)> = if merge {
        existing.example_pairs.iter().cloned().collect()
    } else {
        HashSet::new()
    };
    let examples = parsed
        .examples
        .iter()
        .map(|entry| plan_example_entry(entry, &mut seen_examples))
        .collect();

    let mut seen_rules: HashSet<(Vec<String>, Option<String>)> = if merge {
        existing
            .rule_keys
            .iter()
            .map(|(keywords, reaction)| rule_key(keywords, reaction.as_deref()))
            .collect()
    } else {
        HashSet::new()
    };
    let rules = parsed
        .rules
        .iter()
        .map(|entry| plan_rule_entry(entry, &mut seen_rules))
        .collect();

    (traits, facts, examples, rules)
}

/// 单条 trait 裁决：同 label（跨层全局、精确匹配）跳过。
fn plan_trait_entry(entry: &TraitEntry, seen: &mut HashSet<String>) -> Planned<TraitEntry> {
    let decision = if seen.contains(&entry.label) {
        EntryDecision::Skip(SkipReason::LabelDuplicate)
    } else {
        seen.insert(entry.label.clone());
        EntryDecision::Write
    };
    Planned {
        entry: entry.clone(),
        decision,
    }
}

/// 单条 fact 裁决：同 (field, trim 后 content) 跳过。
fn plan_fact_entry(
    entry: &FactEntry,
    seen: &mut HashSet<(&'static str, String)>,
) -> Planned<FactEntry> {
    let decision = if seen.insert(fact_key(entry.field, &entry.content)) {
        EntryDecision::Write
    } else {
        EntryDecision::Skip(SkipReason::Duplicate)
    };
    Planned {
        entry: entry.clone(),
        decision,
    }
}

/// 单条 example 裁决：(partner, reply) 精确一致跳过。
fn plan_example_entry(
    entry: &ExampleEntry,
    seen: &mut HashSet<(String, String)>,
) -> Planned<ExampleEntry> {
    let key = (entry.user.clone(), entry.assistant.clone());
    let decision = if seen.contains(&key) {
        EntryDecision::Skip(SkipReason::Duplicate)
    } else {
        seen.insert(key);
        EntryDecision::Write
    };
    Planned {
        entry: entry.clone(),
        decision,
    }
}

/// 单条 rule 裁决：keywords 归一集合 + reaction 一致跳过。
fn plan_rule_entry(
    entry: &RuleEntry,
    seen: &mut HashSet<(Vec<String>, Option<String>)>,
) -> Planned<RuleEntry> {
    let key = rule_key(&entry.keywords, entry.reaction.as_deref());
    let decision = if seen.contains(&key) {
        EntryDecision::Skip(SkipReason::Duplicate)
    } else {
        seen.insert(key);
        EntryDecision::Write
    };
    Planned {
        entry: entry.clone(),
        decision,
    }
}

/// 库侧提示追加（等级 / 稳定码 / 面向用户的简短消息，不含原文）。
fn push_library_hints(
    hints: &mut Vec<PresetHint>,
    parsed: &ParsedPreset,
    existing: &ExistingProfile,
    options: &PlanOptions,
    registration: &RegistrationPlan,
    merge: bool,
) {
    if merge && options.merge_target.is_none() {
        hints.push(hint(
            HintLevel::Info,
            "merge_mode",
            "目标 uid 已存在，按合并处理（只增不改）",
        ));
    }

    if options.merge_target.is_some() && !existing.exists {
        hints.push(hint(
            HintLevel::Warn,
            "merge_target_missing",
            "合并目标不存在，将作为新建人格处理",
        ));
    }

    if let (Some(target), Some(uid)) = (
        options.merge_target.as_deref(),
        parsed.persona.uid.as_deref(),
    ) {
        if target != uid {
            hints.push(hint(
                HintLevel::Info,
                "into_overrides_uid",
                "预设 uid 与 --into 不一致，以 --into 为准",
            ));
        }
    }

    if merge && existing.kind.is_some() {
        hints.push(hint(
            HintLevel::Info,
            "kind_immutable",
            "kind 创建后不可变更，保留现有值",
        ));
    }

    if registration.kind == PersonaKind::Rama
        && !existing.exists
        && existing.all_uids.iter().any(|uid| uid.starts_with("rama-"))
    {
        hints.push(hint(
            HintLevel::Info,
            "kind_rama_exists",
            "库中已存在助手人格，本预设将作为独立人格创建",
        ));
    }

    if registration.kind == PersonaKind::User && !existing.exists {
        hints.push(hint(
            HintLevel::Info,
            "kind_user_note",
            "用户身份人格通常由聊天记录导入生成；手动创建不参与主动对话",
        ));
    }

    if merge && existing.has_config {
        hints.push(hint(
            HintLevel::Info,
            "config_skipped",
            "目标已有配置，本次不覆盖（如需覆盖请直接编辑目标配置）",
        ));
        if parsed.prompt.role_suffix.is_some() || parsed.prompt.style_suffix.is_some() {
            hints.push(hint(
                HintLevel::Info,
                "prompt_skipped",
                "提示词补丁随配置一并跳过",
            ));
        }
    }
}

// =========================================================
// 判重辅助
// =========================================================

/// fact 判重键：字段稳定名 + 归一内容。
fn fact_key(field: ProfileField, content: &str) -> (&'static str, String) {
    (field.as_str(), normalize_fact(content))
}

/// fact 内容归一（去首尾空白）。
fn normalize_fact(content: &str) -> String {
    content.trim().to_string()
}

/// rule 判重键：keywords 去空白 / 排序 / 去重 + reaction 去空白（空串视同候选）。
fn rule_key(keywords: &[String], reaction: Option<&str>) -> (Vec<String>, Option<String>) {
    let mut normalized: Vec<String> = keywords
        .iter()
        .map(|keyword| keyword.trim().to_string())
        .filter(|keyword| !keyword.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    let reaction = reaction
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    (normalized, reaction)
}

/// 空缺判定（None 或 trim 后为空）。
fn is_blank(value: Option<&str>) -> bool {
    value.is_none_or(|text| text.trim().is_empty())
}

/// 构造一条提示。
fn hint(level: HintLevel, code: &str, message: &str) -> PresetHint {
    PresetHint {
        level,
        code: code.to_string(),
        message: message.to_string(),
    }
}
