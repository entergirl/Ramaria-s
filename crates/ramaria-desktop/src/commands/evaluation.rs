//! crates/ramaria-desktop/src/commands/evaluation.rs - 评估调试只读面板命令
//!
//! 设计特点:
//! - 只读面板（查看 probe 产物）：选择产物目录 → 列出 JSON 产物 → 解析单个结果文件。
//! - 目录经原生目录选择对话框获取（不信任前端直接传入任意路径做写操作；
//!   读取仍限定 .json 后缀 + 大小上限防误读）。
//! - 读取范围收敛为"允许根目录集合"：用户主目录白名单 ∪ 数据目录 ∪ 用户显式
//!   选择过的目录（选择动作即授权，本次会话有效），越权路径直接拒绝。
//! - 路径日志一律经 `redact_path_label` 脱敏（仅保留文件名 + 短哈希），绝对路径不入日志。
//! - 全程只读：不写库、不触发后端运行、不改任何运行时状态。
//! - 返回产物 JSON 原样解析后的 Value，供前端渲染档位得分/辅助指标等。

use ramaria_core::lock::lock_recover;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::DesktopState;

/// 单文件读取上限（10 MB，防御异常大文件占用内存）。
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

/// 显式授权目录列表的最大长度（超出后淘汰最早登记的一条）。
const MAX_EVAL_ALLOWED_DIRS: usize = 32;

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
// 允许读取范围
// =========================================================

/// 计算评估面板允许读取的根目录集合。
///
/// 返回:
/// - 去重排序后的真实路径集合（用户主目录白名单 ∪ 数据目录 ∪ 显式授权目录）。
///
/// 说明:
/// - 用户主目录白名单由 `path_guard::read_allowed_roots` 统一提供；
/// - 数据目录取数据库文件所在目录（与启动期数据根同一约定）；
/// - 显式授权目录来自原生目录对话框的选择结果（本次会话内有效）；
/// - 集合为空时调用方应拒绝读取。
fn eval_allowed_roots(state: &DesktopState) -> Vec<PathBuf> {
    let mut extra: Vec<PathBuf> = Vec::new();
    if let Some(data_dir) = state.engine.db_path().parent() {
        extra.push(data_dir.to_path_buf());
    }
    extra.extend(
        lock_recover(&state.eval_allowed_dirs, "desktop.eval_allowed_dirs")
            .iter()
            .cloned(),
    );
    crate::path_guard::read_allowed_roots(&extra)
}

// =========================================================
// pick_eval_dir — 选择产物目录
// =========================================================

/// 弹出原生目录选择框，返回用户选择的目录（取消返回 null）。
///
/// 说明:
/// - 选择动作即授权：选择成功且可规范化时，目录登记到本次会话的允许读取集合，
///   供 `list_eval_files` / `read_eval_result` 只读访问；
/// - 登记去重，且最多保留 32 条（超出淘汰最早登记的一条）；
/// - 无法规范化的目录仍原样返回，后续列表命令会给出明确拒绝原因。
#[tauri::command]
#[tracing::instrument(skip(app_handle, state))]
pub async fn pick_eval_dir(
    app_handle: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<String>, String> {
    // 默认定位到数据目录（评估产物常存放于 test-data/ 或 data dir 下）
    let start_dir = state
        .engine
        .db_path()
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let dialog = app_handle.dialog().file();
    let dialog = if start_dir.is_empty() {
        dialog
    } else {
        dialog.set_directory(std::path::PathBuf::from(&start_dir))
    };
    let picked = dialog.blocking_pick_folder();

    match picked {
        Some(folder) => {
            let picked_str = folder.to_string();
            match std::fs::canonicalize(&picked_str) {
                Ok(real) => {
                    {
                        let mut dirs =
                            lock_recover(&state.eval_allowed_dirs, "desktop.eval_allowed_dirs");
                        if !dirs.contains(&real) {
                            if dirs.len() >= MAX_EVAL_ALLOWED_DIRS {
                                dirs.remove(0);
                            }
                            dirs.push(real.clone());
                        }
                    }
                    tracing::info!(
                        dir = %crate::path_guard::redact_path_label(&real),
                        "评估目录已授权（本次会话有效）"
                    );
                    Ok(Some(real.to_string_lossy().to_string()))
                }
                Err(_) => {
                    tracing::warn!(
                        dir = %crate::path_guard::redact_path_label(Path::new(&picked_str)),
                        "评估目录无法规范化，未登记授权"
                    );
                    Ok(Some(picked_str))
                }
            }
        }
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
/// - `dir`: 目录路径（应来自 pick_eval_dir 选择结果，或位于白名单/数据目录内）。
///
/// 说明:
/// - 读取范围收敛为允许根目录集合，越权目录返回面向界面的拒绝原因；
/// - 返回项中的 `path` 为真实文件路径，供前端展示与再次读取（不写日志）。
#[tauri::command]
#[tracing::instrument(skip_all)]
pub async fn list_eval_files(
    state: State<'_, DesktopState>,
    dir: String,
) -> Result<EvalFileListResponse, String> {
    let real_dir = crate::path_guard::validate_read_dir_path(&dir, &eval_allowed_roots(&state))?;

    let entries = fs::read_dir(&real_dir).map_err(|e| format!("读取目录失败: {e}"))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                tracing::warn!("跳过不可读目录项（路径不入日志）");
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
    tracing::debug!(
        dir = %crate::path_guard::redact_path_label(&real_dir),
        total,
        "list_eval_files 完成"
    );

    Ok(EvalFileListResponse {
        dir: real_dir.to_string_lossy().to_string(),
        total,
        files,
    })
}

// =========================================================
// read_eval_result — 解析单个产物
// =========================================================

/// 读取并解析单个评估/报告 JSON 产物（只读）。
///
/// 参数:
/// - `path`: 产物文件完整路径（须位于允许根目录集合内）。
///
/// 说明:
/// - 读取范围与 `list_eval_files` 一致，越权路径返回面向界面的拒绝原因；
/// - 仅接受 `.json` 后缀，且文件大小不超过上限。
#[tauri::command]
#[tracing::instrument(skip_all)]
pub async fn read_eval_result(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<serde_json::Value, String> {
    // ① 后缀校验：只读 JSON 产物
    let file_path = Path::new(&path);
    if file_path.extension().and_then(|s| s.to_str()) != Some("json") {
        return Err("仅支持读取 .json 产物文件".to_string());
    }

    // ② 路径安全校验：真实路径须位于允许根目录集合内（防任意路径读取）
    let real_file = crate::path_guard::validate_read_file_path(&path, &eval_allowed_roots(&state))?;

    // ③ 元数据与大文件上限
    let meta = fs::metadata(&real_file).map_err(|e| format!("读取产物元数据失败: {e}"))?;
    if !meta.is_file() {
        return Err("产物路径不是文件".to_string());
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(format!("产物文件过大（>{MAX_READ_BYTES} 字节），拒绝读取"));
    }

    // ④ 读取与解析
    let content = fs::read_to_string(&real_file).map_err(|e| format!("读取产物失败: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("产物 JSON 解析失败: {e}"))?;

    tracing::debug!(
        file = %crate::path_guard::redact_path_label(&real_file),
        bytes = content.len(),
        "read_eval_result 完成"
    );
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
