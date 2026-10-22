//! crates/ramaria-service/src/persona/import/preview.rs - Ramaria 预设导入预览（dry-run）
//!
//! 设计特点:
//! - 不调用 LLM、不写库：解析 → 注册决议 → 库侧快照 → 裁决
//! - 与实写共用同一裁决数据源，保证「将写入」逐项一致

use std::path::Path;

use ramaria_core::error::RamariaResult;
use ramaria_memory::preset::{
    DecomposedEntries, PlanOptions, PresetPreview, build_preview_from_plan, plan_import,
    resolve_registration,
};

use crate::engine::Engine;

use super::parse::{preview_options, preview_privacy_confirmed, read_and_parse};
use super::write::{list_persona_uids, read_existing_profile};

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
