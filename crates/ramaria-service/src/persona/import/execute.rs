//! crates/ramaria-service/src/persona/import/execute.rs - Ramaria 预设导入执行
//!
//! 设计特点:
//! - 导入永不失败：拆解失败 / 隐私未确认 / 条目写入失败均降级，人格照常建成
//! - 注册列失败上抛（人格记录是根基）；条目逐条独立，失败计数不中断
//! - 拆解受隐私门禁前置：未确认时不调用 LLM，仅保留原文进人格配置

use std::path::Path;
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::Persona;

use ramaria_memory::preset::{
    ExistingProfile, ImportPlan, ParsedPreset, PlanOptions, PresetFormat, PreviewMode,
    build_preview_from_plan, plan_import, resolve_registration, to_superset_toml,
};

use crate::engine::Engine;
use crate::types::PersonaImportOutcome;

use super::decompose::run_decompose;
use super::parse::{
    extract_seq, file_label, preview_options, preview_privacy_confirmed, read_and_parse,
};
use super::write::{list_persona_uids, read_existing_profile, write_entries};

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
