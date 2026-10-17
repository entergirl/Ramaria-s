//! crates/ramaria-desktop/src/commands/preset.rs - 预设导入 Tauri Commands
//!
//! 设计特点:
//! - 两段式接线：dry-run 预览（只读）与确认导入（写库）分命令，前端先预览后确认
//! - 委托服务层预设用例（解析 / 裁决 / 可选拆解 / 写入），桌面只做路径校验与视图映射
//! - 路径三层防御复用导入白名单（用户显式选择的 .toml / .json / .png 文件）
//! - 预览文本由服务层渲染（与 CLI 同一口径），前端不重算裁决结果
//! - 隐私：日志只记脱敏路径标签与 uid，不含预设正文

use crate::DesktopState;
use ramaria_service::types::{DecomposeExecution, ImportWriteStats, PersonaImportOutcome};
use ramaria_service::{PresetPreview, PreviewMode};
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 预设导入预览视图（dry-run）。
///
/// 字段约定:
/// - `text`: 逐条预览文本（服务层渲染；不含自由文本块与描述原文）；
/// - `uid`: 生效 uid（`None` = 待自动生成）；
/// - `mode`: `create` / `merge`；
/// - `decompose_enabled` / `privacy_confirmed`: 拆解开关与隐私门禁状态（预览展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct PresetImportPreviewView {
    pub text: String,
    pub uid: Option<String>,
    pub mode: String,
    pub name: String,
    pub decompose_enabled: bool,
    pub privacy_confirmed: bool,
}

/// 预设导入结果视图。
///
/// 字段约定:
/// - `text`: 结果预览文本（与 dry-run 同一裁决数据源）；
/// - `written`: 实际写入统计（四类条目与失败数）；
/// - `decompose`: 拆解执行情况（未启用 / 已执行 / 跳过原因）。
#[derive(Debug, Clone, Serialize)]
pub struct PresetImportOutcomeView {
    pub text: String,
    pub uid: String,
    pub mode: String,
    pub name: String,
    pub written: ImportWriteStats,
    pub decompose: DecomposeExecution,
}

// =========================================================
// preview_persona_import — 预设导入预览（dry-run）
// =========================================================

/// 预览预设文件将写入的内容（dry-run：不写库、不调用 LLM）。
///
/// 参数:
/// - `file_path`: 用户选择的预设文件路径（.toml / .json / .png）。
/// - `into`: 合并目标 uid（缺省 / 空白 = 新建）。
/// - `no_ai`: 关闭 LLM 拆解（缺省 false）。
///
/// 返回:
/// - 预览视图；路径被拒 / 文件读取 / 解析失败返回错误文案。
///
/// 日志:
/// - INFO: 目标 uid 与模式（不含预设正文）。
#[tauri::command]
#[tracing::instrument(skip(state, file_path))]
pub async fn preview_persona_import(
    state: State<'_, DesktopState>,
    file_path: String,
    into: Option<String>,
    no_ai: Option<bool>,
) -> Result<PresetImportPreviewView, String> {
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;
    let merge_target = normalize_merge_target(into);
    let preview = state
        .engine
        .persona_import_preview(&real_path, merge_target.as_deref(), no_ai.unwrap_or(false))
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "预设预览失败"))?;

    let view = preview_view(preview);
    tracing::info!(
        uid = %view.uid.as_deref().unwrap_or("<auto>"),
        mode = %view.mode,
        "preview_persona_import 完成"
    );
    Ok(view)
}

// =========================================================
// import_persona_preset — 预设导入执行
// =========================================================

/// 执行预设导入（解析 → 可选拆解 → 写入注册列与 L3 条目）。
///
/// 参数:
/// - `file_path`: 用户选择的预设文件路径（.toml / .json / .png）。
/// - `into`: 合并目标 uid（缺省 / 空白 = 新建；目标不存在按新建处理）。
/// - `no_ai`: 关闭 LLM 拆解（缺省 false）。
///
/// 返回:
/// - 导入结果视图（预览文本 + 写入统计 + 拆解执行情况）；
/// - 路径被拒 / 文件读取 / 解析失败与注册列写入失败返回错误文案。
#[tauri::command]
#[tracing::instrument(skip(state, file_path))]
pub async fn import_persona_preset(
    state: State<'_, DesktopState>,
    file_path: String,
    into: Option<String>,
    no_ai: Option<bool>,
) -> Result<PresetImportOutcomeView, String> {
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;
    let merge_target = normalize_merge_target(into);
    let outcome = state
        .engine
        .persona_import(&real_path, merge_target.as_deref(), no_ai.unwrap_or(false))
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "预设导入失败"))?;

    let view = outcome_view(outcome);
    tracing::info!(
        uid = %view.uid,
        mode = %view.mode,
        traits = view.written.traits,
        facts = view.written.facts,
        behavior_rules = view.written.behavior_rules,
        "import_persona_preset 完成"
    );
    Ok(view)
}

// =========================================================
// 参数归一与视图映射
// =========================================================

/// 归一化合并目标：trim 后为空按未提供（新建）。
fn normalize_merge_target(into: Option<String>) -> Option<String> {
    into.map(|uid| uid.trim().to_string())
        .filter(|uid| !uid.is_empty())
}

/// 预览模式字符串化（`create` / `merge`，对齐 JSON 序列化口径）。
fn mode_label(mode: PreviewMode) -> String {
    match mode {
        PreviewMode::Create => "create".to_string(),
        PreviewMode::Merge => "merge".to_string(),
    }
}

/// 服务层预览结果 → 前端视图。
fn preview_view(preview: PresetPreview) -> PresetImportPreviewView {
    PresetImportPreviewView {
        text: preview.render_text(),
        uid: preview.registration.uid,
        mode: mode_label(preview.registration.mode),
        name: preview.registration.name,
        decompose_enabled: preview.decompose.enabled,
        privacy_confirmed: preview.decompose.privacy_confirmed,
    }
}

/// 服务层导入结果 → 前端视图。
fn outcome_view(outcome: PersonaImportOutcome) -> PresetImportOutcomeView {
    let uid = outcome
        .preview
        .registration
        .uid
        .clone()
        .unwrap_or_else(|| "<auto>".to_string());
    PresetImportOutcomeView {
        text: outcome.preview.render_text(),
        uid,
        mode: mode_label(outcome.preview.registration.mode),
        name: outcome.preview.registration.name.clone(),
        written: outcome.written,
        decompose: outcome.decompose,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 合并目标归一：None / 空串 / 纯空白按未提供；有内容时去除首尾空白。
    #[test]
    fn normalize_merge_target_trims_and_drops_blank() {
        assert_eq!(normalize_merge_target(None), None);
        assert_eq!(normalize_merge_target(Some(String::new())), None);
        assert_eq!(normalize_merge_target(Some("   ".to_string())), None);
        assert_eq!(
            normalize_merge_target(Some(" char-0007 ".to_string())),
            Some("char-0007".to_string())
        );
    }

    /// 模式字符串化：与坐标 JSON 序列化口径一致（create / merge）。
    #[test]
    fn mode_label_matches_serialization() {
        assert_eq!(mode_label(PreviewMode::Create), "create");
        assert_eq!(mode_label(PreviewMode::Merge), "merge");
    }
}
