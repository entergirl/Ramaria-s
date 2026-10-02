//! crates/ramaria-service/src/persona/load.rs - Ramaria 人格文件导入模块
//!
//! 设计特点:
//! - 两种入口：目录扫描导入（文件名不含扩展名 = persona uid）与单文件显式 uid 导入（旧布局兼容）
//! - 单文件失败（读取 / 查询 / 写入）不中断其余文件，以结果条目如实回传动作与消息
//! - 已存在处置由 [`PersonaLoadMode`] 表达：创建或更新 / 仅创建缺失（跳过，不写库）
//! - 文件按路径排序处理，顺序确定；`uid_filter` 指定时只处理文件名匹配的文件
//! - 名称提取为行级轻量解析（不引入 TOML 解析器），缺失时按调用方口径回退
//! - 隐私：日志中的个人标识经 `mask_id` 脱敏；错误消息不含文件正文

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Persona, PersonaKind};

use crate::engine::Engine;
use crate::types::{PersonaFileAction, PersonaFileOutcome};

// =========================================================
// 人格文件导入（目录扫描 / 单文件显式 uid）
// =========================================================

/// 人格文件导入模式（记录已存在时的处置）。
///
/// 格式:
/// - 各变体只表达"记录已存在时"的处置；记录不存在时一律新建。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaLoadMode {
    /// 仅创建缺失：已存在的记录跳过（不写库、不刷新 updated_at）。
    CreateMissing,
    /// 创建或更新：已存在的记录按文件同步名称与配置（其他字段保持）。
    CreateOrUpdate,
}

/// 从人格文件目录导入 `.toml` 文件（文件名不含扩展名 = persona uid）。
///
/// 行为:
/// - 记录不存在 → 新建（source=file，config=文件全文）；
///   记录已存在 → 按 `mode` 处置（创建或更新 / 仅创建缺失跳过）；
/// - 名称取文件中的 `assistant_name`，缺失时回退 uid；
/// - 单文件失败（读取 / 查询 / 写入）不中断其余文件，以 [`PersonaFileOutcome`]
///   如实回传动作与消息；
/// - 文件按路径排序处理，顺序确定；`uid_filter` 指定时只处理文件名匹配的文件。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `dir`: 人格文件目录（目录解析由调用方负责）。
/// - `uid_filter`: 只处理指定 uid（文件名 stem 精确匹配）；`None` 表示全部。
/// - `mode`: 记录已存在时的处置模式（见 [`PersonaLoadMode`]）。
///
/// 返回:
/// - 每个文件的处理结果；目录不可读时返回 `Io` 错误。
///
/// 说明:
/// - 目录无匹配文件不属于本用例错误（返回空结果），由调用方按各自现状
///   给出提示或回退（如旧单文件兼容路径）。
pub(crate) async fn load_from_dir(
    engine: &Engine,
    dir: &Path,
    uid_filter: Option<&str>,
    mode: PersonaLoadMode,
) -> RamariaResult<Vec<PersonaFileOutcome>> {
    let files = collect_persona_files(dir, uid_filter)?;
    let storage = engine.storage_ref();

    let mut outcomes = Vec::with_capacity(files.len());
    for path in files {
        outcomes.push(load_single_file(storage, &path, mode).await);
    }

    tracing::info!(
        files = outcomes.len(),
        created = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Created)
            .count(),
        updated = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Updated)
            .count(),
        skipped = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Skipped)
            .count(),
        failed = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Failed)
            .count(),
        "人格文件导入完成"
    );

    Ok(outcomes)
}

/// 收集目录下的 `.toml` 人格文件（可选按 uid 过滤；按路径排序，顺序确定）。
fn collect_persona_files(dir: &Path, uid_filter: Option<&str>) -> RamariaResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        RamariaError::io(format!("读取人格文件目录失败: {}", dir.display()), Some(e))
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            RamariaError::io(
                format!("读取人格文件目录条目失败: {}", dir.display()),
                Some(e),
            )
        })?;
        let path = entry.path();

        // 仅处理 .toml 文件（扩展名大小写不敏感）
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            continue;
        }

        // 按 uid 过滤（文件名不含扩展名精确匹配）
        if let Some(uid) = uid_filter {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            if stem.as_ref() != uid {
                continue;
            }
        }

        files.push(path);
    }

    files.sort();
    Ok(files)
}

/// 从单个 `.toml` 文件导入人格（旧单文件布局兼容路径）。
///
/// 行为:
/// - 记录不存在 → 新建（source=file，config=文件全文）；
///   记录已存在 → 按 `mode` 处置（与目录导入同一实现）；
/// - 名称取文件中的 `assistant_name`，缺失时使用 `fallback_name`；
/// - 读取 / 查询 / 写入失败转为 `Failed` 条目（不上抛），调用方按各自现状处理日志与提示。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `path`: 人格文件路径（旧布局的文件名不携带 uid，uid 由调用方显式给出）。
/// - `uid`: 目标人格 uid（旧单文件布局使用固定的 `rama-0001`）。
/// - `fallback_name`: `assistant_name` 缺失时的名称兜底。
/// - `mode`: 记录已存在时的处置模式（见 [`PersonaLoadMode`]）。
///
/// 返回:
/// - 本文件的处理结果（新建 / 更新 / 跳过 / 失败）。
pub(crate) async fn load_file(
    engine: &Engine,
    path: &Path,
    uid: &str,
    fallback_name: &str,
    mode: PersonaLoadMode,
) -> PersonaFileOutcome {
    import_single_file(engine.storage_ref(), path, uid, Some(fallback_name), mode).await
}

/// 处理单个 `.toml` 文件（文件名（不含扩展名）= persona uid；失败转为结果条目，不上抛）。
///
/// 说明:
/// - `mode` 仅作用于"记录已存在"分支：创建或更新（同步名称与配置）或
///   仅创建缺失（跳过，不写库、不刷新 updated_at）。
async fn load_single_file(
    storage: &Arc<dyn StorageBackend>,
    path: &Path,
    mode: PersonaLoadMode,
) -> PersonaFileOutcome {
    let file_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    // 文件名（不含扩展名）= persona uid
    let Some(uid) = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
    else {
        return PersonaFileOutcome {
            uid: file_label.clone(),
            action: PersonaFileAction::Failed,
            message: format!(
                "无法从文件名提取 UID: {file_label}。文件名必须为 <uid>.toml 格式（如 rama-0001.toml）"
            ),
        };
    };

    import_single_file(storage, path, &uid, None, mode).await
}

/// 从单个文件导入人格（显式 uid；读取 / 名称提取 / 已存在处置的共用实现）。
///
/// 说明:
/// - `fallback_name`: 文件缺少 `assistant_name` 时的名称兜底；`None` 回退 uid；
/// - 单文件失败（读取 / 查询 / 写入）转为 `Failed` 条目，不上抛。
async fn import_single_file(
    storage: &Arc<dyn StorageBackend>,
    path: &Path,
    uid: &str,
    fallback_name: Option<&str>,
    mode: PersonaLoadMode,
) -> PersonaFileOutcome {
    let file_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(uid = %mask_id(uid), error = %e, "读取人格文件失败，跳过该文件");
            return PersonaFileOutcome {
                uid: uid.to_string(),
                action: PersonaFileAction::Failed,
                message: format!("读取文件失败: {file_label}: {e}"),
            };
        }
    };

    // 名称：assistant_name 缺失时按调用方口径回退（目录导入缺省回退 uid）
    let name = extract_assistant_name(&content)
        .unwrap_or_else(|| fallback_name.unwrap_or(uid).to_string());

    match storage.get_persona_by_uid(uid).await {
        Ok(Some(_)) => match mode {
            PersonaLoadMode::CreateMissing => {
                tracing::debug!(uid = %mask_id(uid), "人格已存在，按仅创建缺失模式跳过（未更新）");
                let message = format!("已跳过 persona: {uid} ({name})（已存在）");
                PersonaFileOutcome {
                    uid: uid.to_string(),
                    action: PersonaFileAction::Skipped,
                    message,
                }
            }
            PersonaLoadMode::CreateOrUpdate => match storage
                .update_persona(uid, &name, None, Some(&content), None)
                .await
            {
                Ok(()) => {
                    tracing::info!(uid = %mask_id(uid), "人格文件已同步（更新）");
                    let message = format!("已更新 persona: {uid} ({name})");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Updated,
                        message,
                    }
                }
                Err(e) => {
                    tracing::warn!(uid = %mask_id(uid), error = %e, "更新 persona 失败");
                    let message = format!("更新 persona 失败: {uid}: {e}");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Failed,
                        message,
                    }
                }
            },
        },
        Ok(None) => {
            let kind = PersonaKind::from_uid(uid);
            let mut persona =
                Persona::new(uid.to_string(), name.clone(), kind, 1, "file".to_string());
            persona.config = Some(content);
            match storage.create_persona(&persona).await {
                Ok(_) => {
                    tracing::info!(uid = %mask_id(uid), "人格文件已加载（新建）");
                    let message = format!("已创建 persona: {uid} ({name})");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Created,
                        message,
                    }
                }
                Err(e) => {
                    tracing::warn!(uid = %mask_id(uid), error = %e, "创建 persona 失败");
                    let message = format!("创建 persona 失败: {uid}: {e}");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Failed,
                        message,
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(uid = %mask_id(uid), error = %e, "查询已有 persona 失败");
            let message = format!("查询已有 persona 失败: {uid}: {e}");
            PersonaFileOutcome {
                uid: uid.to_string(),
                action: PersonaFileAction::Failed,
                message,
            }
        }
    }
}

/// 从人格文件文本中提取 `assistant_name` 字段值。
///
/// 说明:
/// - 行级轻量解析（不引入 TOML 解析器）：跳过空行 / 注释行 / 分组行；
/// - 支持引号（单 / 双）与裸值两种写法；键名后必须紧跟 `=`（避免
///   `assistant_name_extra` 之类的键被误匹配）；
/// - 提取失败返回 None，调用方回退使用 uid。
fn extract_assistant_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }

        let Some(rest) = trimmed.strip_prefix("assistant_name") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim();
        if value.is_empty() {
            continue;
        }

        // 引号包裹：去一层引号；裸值原样返回
        for quote in ['"', '\''] {
            if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
                return Some(value[1..value.len() - 1].to_string());
            }
        }
        return Some(value.to_string());
    }
    None
}
