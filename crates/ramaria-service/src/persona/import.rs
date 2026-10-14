//! crates/ramaria-service/src/persona/import.rs - Ramaria 预设导入模块
//!
//! 设计特点:
//! - 两条用例：dry-run 预览（不调用 LLM、不写库）与导入执行（解析 → 可选拆解 → 计划 → 写入）
//! - dry-run 与实写共用同一裁决数据源（`plan_import`），保证「将写入」逐项一致
//! - 导入永不失败：拆解失败 / 隐私未确认 / 条目写入失败均降级，persona 照常建成
//! - 注册列失败上抛（人格记录是根基）；L3 条目逐条独立，写入失败计数不中断其余条目
//! - 拆解受隐私门禁前置：未确认时不调用 LLM，仅保留原文进 `personas.config`
//! - uid 缺省为自动生成（文件名 stem 仅 `persona reload` 目录通道使用）
//! - 隐私：日志与错误消息不含预设原文；日志中的个人标识经 `mask_id` 脱敏

use std::path::Path;
use std::sync::Arc;

use ramaria_core::behavior::{BehaviorRule, BehaviorSituation, RuleSource};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    FactSource, Persona, PersonaExample, PersonaFact, PersonalityTrait, TraitLayer, TraitSource,
    TraitStatus,
};

use ramaria_memory::fact::tier_for_field;
use ramaria_memory::preset::{
    DecomposedEntries, EntryDecision, ExistingProfile, ImportPlan, ParsedPreset, PlanOptions,
    PresetFormat, PresetParseContext, PresetPreview, PreviewMode, PreviewOptions,
    build_preview_from_plan, parse_preset, plan_import, resolve_registration, to_superset_toml,
};
use ramaria_memory::prompt::example_selector::extract_keywords;
use ramaria_memory::{ColdStartConfig, decompose_profile_text};

use crate::engine::Engine;
use crate::types::{DecomposeExecution, ImportWriteStats, PersonaImportOutcome};

// =========================================================
// 导入预览（dry-run）
// =========================================================

/// 预设导入预览（dry-run）：解析 → 注册决议 → 库侧快照 → 裁决，不调用 LLM、不写库。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `path`: 预设文件路径（TOML 超集 / ST 卡 JSON / ST 卡 PNG）。
/// - `merge_target`: 合并目标 uid（`--into`）；`None` 表示新建。
/// - `no_ai`: true 表示关闭 LLM 拆解（`--no-ai`）。
///
/// 返回:
/// - dry-run 预览（注册信息 / 将写入计数 / 跳过明细 / 拆解状态 / 提示）；
/// - 文件读取与解析失败上抛；隐私确认读取失败降级为未确认展示。
pub(crate) async fn preview(
    engine: &Engine,
    path: &Path,
    merge_target: Option<&str>,
    no_ai: bool,
) -> RamariaResult<PresetPreview> {
    let (_, parsed) = read_and_parse(path)?;
    let all_uids = list_persona_uids(engine).await?;
    let resolved = resolve_registration(&parsed, merge_target, &all_uids);
    let existing = read_existing_profile(engine, &resolved.uid, all_uids).await?;

    let plan = plan_import(
        &parsed,
        &resolved,
        &existing,
        &PlanOptions {
            merge_target: merge_target.map(str::to_string),
            decomposed: DecomposedEntries::default(),
        },
    );

    let privacy_confirmed = preview_privacy_confirmed(engine, no_ai).await;
    Ok(build_preview_from_plan(
        &plan,
        &preview_options(path, merge_target, no_ai, privacy_confirmed),
    ))
}

// =========================================================
// 导入执行
// =========================================================

/// 预设导入执行：解析 → 库侧快照 → 可选拆解 → 计划 → 写入注册列与 L3 条目。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `path`: 预设文件路径（TOML 超集 / ST 卡 JSON / ST 卡 PNG）。
/// - `merge_target`: 合并目标 uid（`--into`）；目标不存在按新建处理。
/// - `no_ai`: true 表示关闭 LLM 拆解（`--no-ai`）。
///
/// 返回:
/// - 导入结果（预览 + 实际写入统计 + 拆解执行情况）；
/// - 文件读取 / 解析失败与注册列写入失败上抛（L3 条目失败降级计数）。
pub(crate) async fn execute(
    engine: &Engine,
    path: &Path,
    merge_target: Option<&str>,
    no_ai: bool,
) -> RamariaResult<PersonaImportOutcome> {
    let (bytes, parsed) = read_and_parse(path)?;
    let all_uids = list_persona_uids(engine).await?;
    let resolved = resolve_registration(&parsed, merge_target, &all_uids);
    let existing = read_existing_profile(engine, &resolved.uid, all_uids).await?;

    tracing::info!(
        file = %file_label(path),
        uid = %mask_id(&resolved.uid),
        "预设导入开始"
    );

    let (decomposed, decompose) = run_decompose(engine, &parsed, no_ai).await;
    let plan = plan_import(
        &parsed,
        &resolved,
        &existing,
        &PlanOptions {
            merge_target: merge_target.map(str::to_string),
            decomposed,
        },
    );

    let config_text = config_text(&parsed, &bytes)?;
    write_registration(engine.storage_ref(), &plan, &existing, &config_text).await?;
    let written = write_entries(
        engine.storage_ref(),
        &plan.registration.uid,
        &plan,
        existing.trait_max_seq,
    )
    .await;

    let privacy_confirmed = preview_privacy_confirmed(engine, no_ai).await;
    let preview = build_preview_from_plan(
        &plan,
        &preview_options(path, merge_target, no_ai, privacy_confirmed),
    );

    tracing::info!(
        uid = %mask_id(&plan.registration.uid),
        traits = written.traits,
        facts = written.facts,
        examples = written.examples,
        behavior_rules = written.behavior_rules,
        failed = written.failed,
        "预设导入完成"
    );

    Ok(PersonaImportOutcome {
        preview,
        written,
        decompose,
    })
}

// =========================================================
// 配置文本与注册段写入
// =========================================================

/// 生成写入 `personas.config` 的配置文本。
///
/// 说明:
/// - TOML 来源：文件原文（保证 `persona reload` 重放与原文一致）；
/// - ST 卡来源：解析产物的超集 TOML 序列化文本（映射后形态，保证可重放）。
fn config_text(parsed: &ParsedPreset, bytes: &[u8]) -> RamariaResult<String> {
    match parsed.format {
        PresetFormat::Toml => String::from_utf8(bytes.to_vec())
            .map_err(|e| RamariaError::config(format!("预设文件不是合法 UTF-8: {e}"))),
        PresetFormat::StJson | PresetFormat::StPng => to_superset_toml(parsed),
    }
}

/// 写入注册段（新建全量写入；合并只补空缺列）。
///
/// 说明:
/// - 注册列是人格记录根基，写入失败上抛；
/// - 合并模式四列均无写入裁决时不触达 `update_persona`（避免无意义刷新）；
/// - `update_persona` 的 `None` 实参表示不修改对应列，故仅写入裁决为真的列。
async fn write_registration(
    storage: &Arc<dyn StorageBackend>,
    plan: &ImportPlan,
    existing: &ExistingProfile,
    config_text: &str,
) -> RamariaResult<()> {
    let registration = &plan.registration;
    match registration.mode {
        PreviewMode::Create => {
            let mut persona = Persona::new(
                registration.uid.clone(),
                registration.name.clone(),
                registration.kind,
                extract_seq(&registration.uid),
                registration.source.clone(),
            );
            if registration.write_avatar {
                persona.avatar = registration.avatar.clone();
            }
            if registration.write_description {
                persona.description = registration.description.clone();
            }
            if registration.write_config {
                persona.config = Some(config_text.to_string());
            }
            storage.create_persona(&persona).await?;
        }
        PreviewMode::Merge => {
            if !registration.write_name
                && !registration.write_avatar
                && !registration.write_description
                && !registration.write_config
            {
                return Ok(());
            }
            let name = if registration.write_name {
                registration.name.clone()
            } else {
                existing
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| registration.name.clone())
            };
            let avatar = if registration.write_avatar {
                registration.avatar.as_deref()
            } else {
                None
            };
            let config = if registration.write_config {
                Some(config_text)
            } else {
                None
            };
            let description = if registration.write_description {
                registration.description.as_deref()
            } else {
                None
            };
            storage
                .update_persona(&registration.uid, &name, avatar, config, description)
                .await?;
        }
    }
    Ok(())
}

// =========================================================
// L3 条目写入
// =========================================================

/// 写入四类 L3 条目（仅裁决为写入的条目；逐条独立，失败计数不中断）。
///
/// 说明:
/// - traits 分层续号：起始 = 库侧各层最大 seq，按计划顺序逐条 +1（层内声明顺序）；
/// - 落库默认值：traits `Manual / Active / 0.5 / 1.0 / 0.5`；
///   facts `Manual / tier 按字段分层 / 1.0`；examples `selected` + 关键词 tags；
///   rules `Manual / 1.0 / 1.0 / enabled`。
async fn write_entries(
    storage: &Arc<dyn StorageBackend>,
    uid: &str,
    plan: &ImportPlan,
    trait_max_seq: [i32; 3],
) -> ImportWriteStats {
    let mut stats = ImportWriteStats::default();
    let mut trait_seq = trait_max_seq;

    for planned in plan
        .traits
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let index = layer_index(entry.layer);
        trait_seq[index] += 1;

        let mut trait_row = PersonalityTrait::new(
            uid.to_string(),
            entry.layer,
            entry.label.clone(),
            entry.meaning.clone(),
            TraitSource::Manual,
            trait_seq[index],
        );
        trait_row.not_meaning = entry.not_meaning.clone();
        trait_row.trigger = entry.trigger.clone();
        trait_row.suppress = entry.suppress.clone();
        trait_row.related = entry.related.clone();
        trait_row.status = TraitStatus::Active;
        trait_row.confidence = 0.5;
        trait_row.evidence = 1.0;
        trait_row.consistency = 0.5;

        match storage.save_trait(&trait_row).await {
            Ok(_) => stats.traits += 1,
            Err(e) => {
                tracing::warn!(label = %entry.label, error = %e, "写入预设 trait 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    for planned in plan
        .facts
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let mut fact = PersonaFact::new(
            uid.to_string(),
            entry.field,
            entry.content.clone(),
            FactSource::Manual,
        );
        fact.tier = tier_for_field(entry.field);

        match storage.save_fact(&fact).await {
            Ok(_) => stats.facts += 1,
            Err(e) => {
                tracing::warn!(field = %entry.field, error = %e, "写入预设 fact 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    for planned in plan
        .examples
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let mut example =
            PersonaExample::new(uid.to_string(), entry.user.clone(), entry.assistant.clone());
        example.selected = true;
        example.tags =
            Some(extract_keywords(&format!("{} {}", entry.user, entry.assistant)).join(","));

        match storage.save_example(&example).await {
            Ok(_) => stats.examples += 1,
            Err(e) => {
                tracing::warn!(error = %e, "写入预设 example 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    for planned in plan
        .rules
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let situation = BehaviorSituation {
            keywords: entry.keywords.clone(),
            ..BehaviorSituation::empty()
        };
        let mut rule = BehaviorRule::new(
            uid.to_string(),
            situation,
            entry.reaction.clone(),
            entry.params,
            RuleSource::Manual,
        );
        rule.avoid = entry.avoid.clone();
        rule.confidence = 1.0;
        rule.stability = 1.0;

        match storage.save_behavior_rule(&rule).await {
            Ok(_) => stats.behavior_rules += 1,
            Err(e) => {
                tracing::warn!(error = %e, "写入预设 behavior_rules 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    stats
}

// =========================================================
// 库侧现状快照
// =========================================================

/// 读取目标 uid 的库侧现状快照（目标不存在时返回 exists=false；`all_uids` 总是填充）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标 uid（生效注册决议）。
/// - `all_uids`: 库内全部 active persona 的 uid（与注册决议同源，避免重复读取）。
///
/// 返回:
/// - 现状快照：注册列 / traits label 与各层最大 seq / facts / examples / rules 判重键。
async fn read_existing_profile(
    engine: &Engine,
    uid: &str,
    all_uids: Vec<String>,
) -> RamariaResult<ExistingProfile> {
    let storage = engine.storage_ref();
    let mut profile = ExistingProfile {
        all_uids,
        ..ExistingProfile::default()
    };

    let Some(persona) = storage.get_persona_by_uid(uid).await? else {
        return Ok(profile);
    };

    profile.exists = true;
    profile.kind = Some(persona.kind);
    profile.name = Some(persona.name);
    profile.avatar = persona.avatar;
    profile.description = persona.description;
    profile.has_config = persona
        .config
        .as_deref()
        .is_some_and(|config| !config.trim().is_empty());

    for trait_row in storage.list_traits_by_persona(uid).await? {
        let index = layer_index(trait_row.layer);
        if trait_row.seq > profile.trait_max_seq[index] {
            profile.trait_max_seq[index] = trait_row.seq;
        }
        profile.trait_labels.push(trait_row.trait_label);
    }
    for fact in storage.list_all_facts_by_persona(uid).await? {
        profile.fact_pairs.push((fact.field, fact.content));
    }
    for example in storage.list_all_examples(uid).await? {
        profile.example_pairs.push((example.partner, example.reply));
    }
    for rule in storage.list_behavior_rules_by_persona(uid).await? {
        profile
            .rule_keys
            .push((rule.situation.keywords, rule.reaction));
    }

    Ok(profile)
}

/// 库内全部 persona 的 uid 列表（注册决议与自动生成序号来源）。
async fn list_persona_uids(engine: &Engine) -> RamariaResult<Vec<String>> {
    Ok(engine
        .storage_ref()
        .list_personas()
        .await?
        .into_iter()
        .map(|persona| persona.uid)
        .collect())
}

// =========================================================
// 可选 LLM 拆解
// =========================================================

/// 执行可选 LLM 拆解（隐私门禁 + 输入组装；失败降级不阻断）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `parsed`: 预设解析产物（拆解输入 = ST personality 原文与 `[blocks]` 汇总）。
/// - `no_ai`: true 表示用户关闭拆解（未请求，不计跳过原因）。
///
/// 返回:
/// - 拆解条目（失败 / 跳过时为空）与执行情况；
/// - 拆解失败（LLM 调用 / 响应解析）与隐私未确认均降级，错误不上抛。
async fn run_decompose(
    engine: &Engine,
    parsed: &ParsedPreset,
    no_ai: bool,
) -> (DecomposedEntries, DecomposeExecution) {
    if no_ai {
        return (
            DecomposedEntries::default(),
            DecomposeExecution {
                requested: false,
                executed: false,
                skipped_reason: None,
                fact_count: 0,
                trait_count: 0,
            },
        );
    }

    let text = compose_decompose_text(parsed);
    if text.is_empty() {
        return (DecomposedEntries::default(), skipped_execution("no_input"));
    }

    match crate::privacy::online_privacy_confirmed(engine).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!("预设拆解跳过：线上 provider 隐私未确认");
            return (
                DecomposedEntries::default(),
                skipped_execution("privacy_unconfirmed"),
            );
        }
        Err(e) => {
            // 隐私状态不可判定时保守按未确认处理（不调用 LLM）
            tracing::warn!(error = %e, "隐私确认读取失败，跳过预设拆解");
            return (
                DecomposedEntries::default(),
                skipped_execution("privacy_unconfirmed"),
            );
        }
    }

    match decompose_profile_text(
        engine.llm_ref().as_ref(),
        &text,
        &ColdStartConfig::default(),
    )
    .await
    {
        Ok(decomposed) => {
            let entries = DecomposedEntries {
                traits: decomposed.traits,
                facts: decomposed.facts,
            };
            let execution = DecomposeExecution {
                requested: true,
                executed: true,
                skipped_reason: None,
                fact_count: entries.facts.len(),
                trait_count: entries.traits.len(),
            };
            (entries, execution)
        }
        Err(e) => {
            // 隐私红线：错误细节不含原文
            tracing::warn!(error = %e, "预设拆解失败，降级为不拆解");
            (
                DecomposedEntries::default(),
                skipped_execution("llm_failed"),
            )
        }
    }
}

/// 构造拆解输入文本：ST personality 原文与 `[blocks]` 段以空行连接（整体 trim）。
fn compose_decompose_text(parsed: &ParsedPreset) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(raw) = parsed.personality_raw.as_deref() {
        if !raw.trim().is_empty() {
            parts.push(raw.trim().to_string());
        }
    }
    for (key, value) in &parsed.blocks {
        parts.push(format!("## {key}\n\n{value}"));
    }
    parts.join("\n\n").trim().to_string()
}

/// 未执行拆解的执行情况（requested=true，附跳过原因）。
fn skipped_execution(reason: &str) -> DecomposeExecution {
    DecomposeExecution {
        requested: true,
        executed: false,
        skipped_reason: Some(reason.to_string()),
        fact_count: 0,
        trait_count: 0,
    }
}

// =========================================================
// 辅助
// =========================================================

/// 读取预设文件字节并解析（错误消息含文件名、不含正文）。
fn read_and_parse(path: &Path) -> RamariaResult<(Vec<u8>, ParsedPreset)> {
    let bytes = std::fs::read(path).map_err(|e| {
        RamariaError::io(format!("读取预设文件失败: {}", file_label(path)), Some(e))
    })?;
    let parsed = parse_preset(&bytes, &parse_context())?;
    Ok((bytes, parsed))
}

/// 预设导入通道的解析上下文。
///
/// 说明:
/// - uid 缺省口径为自动生成（文件名 stem 仅 `persona reload` 目录通道使用），
///   故不携带文件 stem，避免任意文件名被当作主键；显式 `[persona].uid` 仍优先（解析器内部处理）。
fn parse_context() -> PresetParseContext {
    PresetParseContext {
        file_stem: None,
        default_source: "manual".to_string(),
    }
}

/// 预览选项（dry-run 与执行结果预览共用同一构造口径）。
fn preview_options(
    path: &Path,
    merge_target: Option<&str>,
    no_ai: bool,
    privacy_confirmed: bool,
) -> PreviewOptions {
    PreviewOptions {
        file_name: file_name(path),
        merge_target: merge_target.map(str::to_string),
        decompose_enabled: !no_ai,
        privacy_confirmed,
    }
}

/// 预览用隐私确认查询：读取失败按未确认处理（预览不应因隐私读取失败而失败）。
async fn preview_privacy_confirmed(engine: &Engine, no_ai: bool) -> bool {
    if no_ai {
        return false;
    }
    match crate::privacy::online_privacy_confirmed(engine).await {
        Ok(confirmed) => confirmed,
        Err(e) => {
            tracing::debug!(error = %e, "隐私确认读取失败，预览按未确认处理");
            false
        }
    }
}

/// 文件显示名（含扩展名；用于预览展示与日志）。
fn file_label(path: &Path) -> String {
    file_name(path).unwrap_or_else(|| path.display().to_string())
}

/// 文件名（含扩展名；无文件名时为 None）。
fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// 层索引（0/1/2 = Base/Primary/Accent；`trait_max_seq` 数组下标）。
fn layer_index(layer: TraitLayer) -> usize {
    match layer {
        TraitLayer::Base => 0,
        TraitLayer::Primary => 1,
        TraitLayer::Accent => 2,
        _ => 2,
    }
}

/// 从 uid 提取序号（`{kind}-{seq}` 的 seq 段；无法解析时回退 1）。
fn extract_seq(uid: &str) -> i64 {
    uid.rsplit('-')
        .next()
        .and_then(|seq| seq.parse::<i64>().ok())
        .unwrap_or(1)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
