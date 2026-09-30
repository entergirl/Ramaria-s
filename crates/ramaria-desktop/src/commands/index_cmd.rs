//! crates/ramaria-desktop/src/commands/index_cmd.rs - 索引管理 Tauri Commands
//!
//! 设计特点:
//! - rebuild_index: 触发检索索引全量重建（委托服务层索引用例）
//! - 重建完成后索引立即生效（不受刷新间隔约束）

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
/// - 重建失败时旧索引保持可用、告警位置位并上抛错误
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn rebuild_index(state: State<'_, DesktopState>) -> Result<usize, String> {
    let count = state
        .engine
        .rebuild_index()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "索引重建失败"))?;

    tracing::info!(doc_count = count, "索引重建完成");
    Ok(count)
}
