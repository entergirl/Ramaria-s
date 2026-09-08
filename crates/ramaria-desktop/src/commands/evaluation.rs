//! crates/ramaria-desktop/src/commands/evaluation.rs - 评估调试只读面板命令（M7）
//!
//! 设计特点:
//! - 只读面板，服务 M8：选择 probe 产物目录 → 列出 JSON 产物 → 解析单个结果文件。
//! - 目录经原生目录选择对话框获取（不信任前端直接传入任意路径做写操作；
//!   读取仍限定 .json 后缀 + 大小上限防误读）。
//! - 全程只读：不写库、不触发后端运行、不改任何运行时状态。
//! - 返回产物 JSON 原样解析后的 Value，供前端渲染档位得分/辅助指标等。

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::DesktopState;

/// 单文件读取上限（10 MB，防御异常大文件占用内存）。
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

// =========================================================
// 前端展示结构体
// =========================================================

/// 目录内 JSON 产物文件信息。
#[derive(Debug, Clone, Serialize)]
pub struct EvalFileInfo {
    /// 文件名
    pub name: String,
    /// 完整路径
    pub path: String,
    /// 文件大小（字节）
    pub size: u64,
    /// 最近修改时间（Unix 毫秒，null = 不可读）
    pub modified_at: Option<i64>,
}

/// 产物文件列表响应。
#[derive(Debug, Clone, Serialize)]
pub struct EvalFileListResponse {
    /// 目录路径
    pub dir: String,
    /// 命中文件数
    pub total: usize,
    pub files: Vec<EvalFileInfo>,
}

// =========================================================
// pick_eval_dir — 选择产物目录
// =========================================================

/// 弹出原生目录选择框，返回用户选择的目录（取消返回 null）。
#[tauri::command]
#[tracing::instrument(skip(app_handle, state))]
pub async fn pick_eval_dir(
    app_handle: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<String>, String> {
    // 默认定位到数据目录（评估产物常存放于 test-data/ 或 data dir 下）
    let start_dir = state.app.config().paths.data_dir.clone();
    let dialog = app_handle.dialog().file();
    let dialog = if start_dir.is_empty() {
        dialog
    } else {
        dialog.set_directory(std::path::PathBuf::from(&start_dir))
    };
    let picked = dialog.blocking_pick_folder();

    match picked {
        Some(folder) => Ok(Some(folder.to_string())),
        None => {
            tracing::debug!("用户取消了评估目录选择");
            Ok(None)
        }
    }
}

// =========================================================
// list_eval_files — 列出目录内 JSON 产物
// =========================================================

/// 列出指定目录内的 JSON 文件（递归不上溯），按修改时间倒序。
///
/// 参数:
/// - `dir`: 目录路径（应来自 pick_eval_dir 选择结果）。
#[tauri::command]
#[tracing::instrument(skip_all)]
pub async fn list_eval_files(dir: String) -> Result<EvalFileListResponse, String> {
    let path = PathBuf::from(&dir);
    if !path.is_dir() {
        return Err(format!("目录不存在或不可访问: {dir}"));
    }

    let entries = fs::read_dir(&path).map_err(|e| format!("读取目录失败: {e}"))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "跳过不可读目录项");
                continue;
            }
        };
        let file_path = entry.path();
        if file_path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let meta = match fs::metadata(&file_path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        files.push(EvalFileInfo {
            name: entry.file_name().to_string_lossy().to_string(),
            path: file_path.to_string_lossy().to_string(),
            size: meta.len(),
            modified_at: modified_millis(&meta),
        });
    }

    // 修改时间倒序（最近产物在前）
    files.sort_by_key(|f| std::cmp::Reverse(f.modified_at.unwrap_or(0)));
    let total = files.len();
    tracing::debug!(dir = %dir, total, "list_eval_files 完成");

    Ok(EvalFileListResponse { dir, total, files })
}

// =========================================================
// read_eval_result — 解析单个产物
// =========================================================

/// 读取并解析单个评估/报告 JSON 产物（只读）。
///
/// 参数:
/// - `path`: 产物文件完整路径。
#[tauri::command]
#[tracing::instrument(skip_all)]
pub async fn read_eval_result(path: String) -> Result<serde_json::Value, String> {
    let file_path = Path::new(&path);
    if file_path.extension().and_then(|s| s.to_str()) != Some("json") {
        return Err("仅支持读取 .json 产物文件".to_string());
    }
    let meta = fs::metadata(file_path).map_err(|e| format!("读取产物元数据失败: {e}"))?;
    if !meta.is_file() {
        return Err("产物路径不是文件".to_string());
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(format!("产物文件过大（>{MAX_READ_BYTES} 字节），拒绝读取"));
    }

    let content = fs::read_to_string(file_path).map_err(|e| format!("读取产物失败: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("产物 JSON 解析失败: {e}"))?;

    tracing::debug!(path = %file_path.display(), bytes = content.len(), "read_eval_result 完成");
    Ok(value)
}

/// 将 SystemTime 转为 Unix 毫秒（失败返回 None，不阻塞列表）。
fn modified_millis(meta: &fs::Metadata) -> Option<i64> {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}
