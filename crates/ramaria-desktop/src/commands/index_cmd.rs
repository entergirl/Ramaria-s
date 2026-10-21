//! crates/ramaria-desktop/src/commands/index_cmd.rs - 索引管理 Tauri Commands
//!
//! 设计特点:
//! - get_index_status: 读取索引状态视图（状态 / 进度 / 模型 / 失败原因）
//! - rebuild_index: 触发后台重建并立即返回回执（不阻塞命令线程）
//! - 构建进度经 `index-progress` 事件推送（引擎回调在 setup 中注册）

use crate::DesktopState;
use ramaria_service::types::{IndexRebuildReceipt, IndexStatusView};
use tauri::State;

// =========================================================
// get_index_status — 读取索引状态
// =========================================================

/// 读取当前索引状态视图。
///
/// 返回:
/// - 状态 / 已加载与总数 / 百分比 / 模型标识 / 最近失败原因 / 更新时间
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_index_status(state: State<'_, DesktopState>) -> Result<IndexStatusView, String> {
    Ok(state.engine.index_status())
}

// =========================================================
// rebuild_index — 触发后台重建
// =========================================================

/// 触发全部检索索引（BM25 + 图谱 + 向量）的后台重建。
///
/// 返回:
/// - `started=true`: 本次已启动构建；
/// - `started=false`: 已有构建在途，本次忽略
///
/// 说明:
/// - 构建进度经 `index-progress` 事件推送；
/// - 构建失败时旧索引保持可用，状态切到 `degraded` 并携带脱敏原因
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn rebuild_index(state: State<'_, DesktopState>) -> Result<IndexRebuildReceipt, String> {
    let started = state.engine.start_index_build();
    if started {
        tracing::info!("索引后台重建已启动");
    }
    Ok(IndexRebuildReceipt { started })
}
