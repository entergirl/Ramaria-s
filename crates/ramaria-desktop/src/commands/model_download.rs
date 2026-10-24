//! crates/ramaria-desktop/src/commands/model_download.rs - 嵌入模型下载 Tauri Command
//!
//! 设计特点:
//! - 下载期间应用状态推进到 `DownloadingModel`，结束（成功或失败）恢复原状态
//! - 进度经服务层节流后转成 `model-download-progress` 事件，前端全局横幅消费
//! - 只做参数校验与事件桥接，下载、续传与校验全部由服务层与模型管理器完成

use std::sync::Arc;

use crate::DesktopState;
use tauri::State;

// =========================================================
// download_embedding_model — 下载嵌入模型
// =========================================================

/// 归一化模型标识（去首尾空白；空值拒绝）。
///
/// 返回:
/// - `Ok(id)`: 可直接用于预置清单查询的标识；
/// - `Err(message)`: 用户可读的校验文案。
fn normalized_model_id(raw: &str) -> Result<&str, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("模型标识不能为空".to_string());
    }
    Ok(trimmed)
}

/// 下载嵌入模型（断点续传 + 校验由模型管理器实现）。
///
/// 参数:
/// - `model_id`: 模型标识（须在预置清单内）。
///
/// 返回:
/// - `"downloaded"`: 下载完成；
/// - 失败时返回用户可读错误文案（状态已恢复，可重试）。
///
/// 说明:
/// - 进度通过 `model-download-progress` 事件推送（开始 / 进度 / 完成 / 失败四态）；
/// - 下载不修改后端配置与嵌入模型实例：调用方在下载完成后自行加载模型。
#[tauri::command]
#[tracing::instrument(skip(app, state))]
pub async fn download_embedding_model(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    model_id: String,
) -> Result<String, String> {
    let model_id = normalized_model_id(&model_id)?;

    let engine = Arc::clone(&state.engine);
    let sink: ramaria_service::ModelDownloadSink = {
        let app = app.clone();
        Arc::new(move |progress| {
            crate::events::emit_model_download_progress(&app, &progress);
        })
    };

    engine
        .download_model_with_progress(model_id, None, Some(sink))
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "下载嵌入模型失败"))?;

    tracing::info!(model_id, "嵌入模型下载命令完成");
    Ok("downloaded".to_string())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 模型标识归一：去空白后透传，全空白拒绝。
    #[test]
    fn model_id_is_trimmed_and_validated() {
        assert_eq!(
            normalized_model_id("  bge-small-zh-v1.5  "),
            Ok("bge-small-zh-v1.5")
        );
        assert!(normalized_model_id("   ").is_err());
    }
}
