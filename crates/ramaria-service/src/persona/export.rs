//! crates/ramaria-service/src/persona/export.rs - Ramaria 预设导出模块
//!
//! 设计特点:
//! - 画像级只读导出：注册列 + L3 结构化条目（traits / facts / behavior_rules）→ 预设超集 TOML
//! - 对话身份 / 自由文本块 / 提示词补丁自 `personas.config` 解析重放（不可解析时降级省略并提示）
//! - 隐私：不含证据链（trait / rule 证据与来源引用）、对话示例与会话原文
//! - 输出可被预设导入通道原样消费（导出 → 导入往返一致）
//! - 不写库、不调 LLM；日志中的个人标识经 `mask_id` 脱敏

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::TraitLayer;
use ramaria_memory::preset::{
    FactEntry, IdentitySection, KindSource, ParsedPreset, PersonaSection, PresetFormat,
    PresetParseContext, PromptPatch, RuleEntry, TraitEntry, TraitsSection, parse_toml,
    to_superset_toml,
};

use crate::engine::Engine;
use crate::types::{PersonaExportOutcome, PersonaExportStats};

/// 配置缺失 / 不可解析时的导出提示（不含任何原文）。
const CONFIG_UNDECODABLE_HINT: &str =
    "personas.config 缺失或不可解析：未包含自由文本块、对话身份还原与提示词补丁";

// =========================================================
// 导出用例
// =========================================================

/// 导出人格画像为预设超集 TOML（只读：不写库、不调 LLM）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标人格 uid（空白 → `Validation`）。
///
/// 返回:
/// - 导出结果（TOML 文本 + 逐类计数 + 降级提示）；uid 空或人格不存在返回 `Validation`。
///
/// 说明:
/// - traits 取 active 三层（按层内 seq 顺序）；facts 取 active；behavior_rules 取启用项；
/// - 无关键词的行为规则被跳过（预设格式要求 keywords 非空）并在 `hints` 中计数；
/// - `personas.config` 解析失败时降级为「无自由文本块 + 注册名兜底身份 + 无提示词补丁」。
pub(crate) async fn export(engine: &Engine, uid: &str) -> RamariaResult<PersonaExportOutcome> {
    let uid = uid.trim();
    if uid.is_empty() {
        return Err(RamariaError::validation("uid 不能为空"));
    }

    let storage = engine.storage_ref().as_ref();
    let Some(persona) = storage.get_persona_by_uid(uid).await? else {
        return Err(RamariaError::validation(format!("人格不存在: {uid}")));
    };

    let mut hints: Vec<String> = Vec::new();

    // ---- L3 结构化条目 ----
    let traits = collect_traits(storage, uid).await?;
    let facts = collect_facts(storage, uid).await?;
    let (rules, dropped_rules) = collect_rules(storage, uid).await?;
    if dropped_rules > 0 {
        hints.push(format!(
            "已跳过 {dropped_rules} 条无关键词的行为规则（预设格式要求 keywords 非空）"
        ));
    }

    // ---- config 重放（对话身份 / 自由文本块 / 提示词补丁）----
    let replayed = replay_config(persona.config.as_deref(), &persona.uid, &persona.name);
    if replayed.is_none() {
        hints.push(CONFIG_UNDECODABLE_HINT.to_string());
    }
    let identity = replayed
        .as_ref()
        .map(|replayed| replayed.identity.clone())
        .unwrap_or_else(|| fallback_identity(&persona.name));
    let blocks = replayed
        .as_ref()
        .map(|replayed| replayed.blocks.clone())
        .unwrap_or_default();
    let prompt = replayed.map(|replayed| replayed.prompt).unwrap_or_default();

    let stats = PersonaExportStats {
        traits: traits.base.len() + traits.primary.len() + traits.accent.len(),
        facts: facts.len(),
        behavior_rules: rules.len(),
        blocks: blocks.len(),
        prompt_patch: prompt.role_suffix.is_some() || prompt.style_suffix.is_some(),
    };

    let parsed = ParsedPreset {
        format: PresetFormat::Toml,
        persona: PersonaSection {
            uid: Some(persona.uid.clone()),
            kind: Some(persona.kind),
            kind_source: KindSource::Explicit,
            name: persona.name.clone(),
            avatar: persona.avatar.clone(),
            description: persona.description.clone(),
            source: persona.source.clone(),
        },
        identity,
        blocks,
        traits,
        facts,
        // 对话示例与会话原文不导出（隐私口径；示例为真实对话片段）
        examples: Vec::new(),
        rules,
        prompt,
        personality_raw: None,
        hints: Vec::new(),
        skipped: Vec::new(),
    };
    let toml = to_superset_toml(&parsed)?;

    tracing::info!(
        uid = %mask_id(&persona.uid),
        traits = stats.traits,
        facts = stats.facts,
        behavior_rules = stats.behavior_rules,
        blocks = stats.blocks,
        "预设导出完成"
    );

    Ok(PersonaExportOutcome {
        uid: persona.uid,
        name: persona.name,
        toml,
        stats,
        hints,
    })
}

// =========================================================
// L3 条目收集
// =========================================================

/// 收集 active 性格标签并按三层归类（存储层已按 layer, seq 排序）。
async fn collect_traits(storage: &dyn StorageBackend, uid: &str) -> RamariaResult<TraitsSection> {
    let mut traits = TraitsSection::default();

    for row in storage.list_traits_by_persona(uid).await? {
        if row.trait_label.trim().is_empty() || row.meaning.trim().is_empty() {
            continue;
        }
        let entry = TraitEntry {
            layer: row.layer,
            label: row.trait_label,
            meaning: row.meaning,
            not_meaning: row.not_meaning,
            trigger: row.trigger,
            suppress: row.suppress,
            related: row.related,
            motives: Vec::new(),
        };
        match row.layer {
            TraitLayer::Base => traits.base.push(entry),
            TraitLayer::Primary => traits.primary.push(entry),
            _ => traits.accent.push(entry),
        }
    }

    Ok(traits)
}

/// 收集 active 事实（内容为空白的条目跳过）。
async fn collect_facts(storage: &dyn StorageBackend, uid: &str) -> RamariaResult<Vec<FactEntry>> {
    let rows = storage.list_active_facts_by_persona(uid).await?;
    Ok(rows
        .into_iter()
        .filter(|fact| !fact.content.trim().is_empty())
        .map(|fact| FactEntry {
            field: fact.field,
            content: fact.content,
        })
        .collect())
}

/// 收集启用的行为规则（返回条目与无关键词跳过计数）。
///
/// 说明:
/// - 关键词去重保首现（与预设解析口径一致，保证往返可比）；
/// - 无关键词的规则无法被预设格式表达，跳过并计数。
async fn collect_rules(
    storage: &dyn StorageBackend,
    uid: &str,
) -> RamariaResult<(Vec<RuleEntry>, usize)> {
    let rows = storage.list_behavior_rules_by_persona(uid).await?;
    let mut entries = Vec::new();
    let mut dropped = 0usize;

    for rule in rows.into_iter().filter(|rule| rule.enabled) {
        let mut keywords: Vec<String> = Vec::new();
        for keyword in rule.situation.keywords {
            let keyword = keyword.trim().to_string();
            if keyword.is_empty() || keywords.contains(&keyword) {
                continue;
            }
            keywords.push(keyword);
        }
        if keywords.is_empty() {
            dropped += 1;
            continue;
        }
        entries.push(RuleEntry {
            keywords,
            reaction: rule.reaction,
            avoid: rule.avoid,
            params: rule.params,
        });
    }

    Ok((entries, dropped))
}

// =========================================================
// config 重放
// =========================================================

/// 注册名兜底身份（config 不可重放时的缺省段）。
fn fallback_identity(name: &str) -> IdentitySection {
    IdentitySection {
        assistant_name: name.to_string(),
        user_name: None,
        work: None,
        era: None,
    }
}

/// config 重放结果（对话身份 / 自由文本块 / 提示词补丁）。
struct ReplayedConfig {
    identity: IdentitySection,
    blocks: Vec<(String, String)>,
    prompt: PromptPatch,
}

/// 自 `personas.config` 重放对话身份 / 自由文本块 / 提示词补丁。
///
/// 参数:
/// - `config`: `personas.config` 文本（None / 空白 = 无配置）。
/// - `uid`: 目标人格 uid（作为文件名 stem 兜底，令旧格式配置也可解析）。
/// - `name`: 注册名（名称兜底链把 uid 当自称时替换为注册名）。
///
/// 返回:
/// - 解析成功时返回三段重放结果；
/// - 缺失 / 非预设文本 / 解析失败返回 None（导出降级为不含自由文本块与提示词补丁）。
fn replay_config(config: Option<&str>, uid: &str, name: &str) -> Option<ReplayedConfig> {
    let text = config.map(str::trim).filter(|text| !text.is_empty())?;
    let ctx = PresetParseContext {
        file_stem: Some(uid.to_string()),
        default_source: "manual".to_string(),
    };
    let parsed = parse_toml(text, &ctx).ok()?;

    let mut identity = parsed.identity;
    if identity.assistant_name == uid {
        identity.assistant_name = name.to_string();
    }
    Some(ReplayedConfig {
        identity,
        blocks: parsed.blocks,
        prompt: parsed.prompt,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
