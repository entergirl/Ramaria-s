//! rust/crates/ramaria-desktop/src/commands/index_cmd.rs - 索引管理 Tauri Commands
//!
//! 设计特点:
//! - rebuild_index: 触发检索索引全量重建
//! - 委托 ramaria_app::App::rebuild_retriever()

use crate::DesktopState;
use tauri::State;

// =========================================================
// rebuild_index — 重建检索索引
// =========================================================

/// 触发全部检索索引（BM25 + 图谱）的全量重建。
///
/// 返回:
/// - 重建的文档数量
///
/// 说明:
/// - 重建期间应用进入 Indexing 状态
/// - 前端应轮询 get_app_state 直到状态变为 Ready
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn rebuild_index(state: State<'_, DesktopState>) -> Result<usize, String> {
    let count = state
        .app
        .rebuild_retriever()
        .await
        .map_err(|e| format!("索引重建失败: {}", e))?;

    tracing::info!(doc_count = count, "索引重建完成");
    Ok(count)
}
