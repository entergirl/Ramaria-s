//! crates/ramaria-desktop/src/commands/attachment.rs - 会话浏览图片 Tauri Commands
//!
//! 设计特点:
//! - `read_attachment_image`: 按消息与序号读取图片字节（原生二进制响应，前端直接构造 Blob URL）
//! - `clear_image_cache`: 清理客户端图片缓存并返回统计（文件数 / 字节数）
//! - 缓存目录固定在数据库同级的 `image_cache` 子目录；路径不入日志
//! - 读取失败返回业务文案（消息不存在 / 序号越界 / 无本地文件 / 读取失败）

use std::path::PathBuf;

use ramaria_service::Engine;
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::DesktopState;

// =========================================================
// 缓存目录
// =========================================================

/// 客户端图片缓存目录（数据库文件同级的 `image_cache` 子目录）。
///
/// 返回:
/// - `Some(path)`: 数据库文件有父目录时的缓存目录；
/// - `None`: 数据库路径无父目录（调用方按降级处置）。
pub(crate) fn image_cache_dir(engine: &Engine) -> Option<PathBuf> {
    engine
        .db_path()
        .parent()
        .map(|parent| parent.join("image_cache"))
}

// =========================================================
// read_attachment_image — 读取会话图片
// =========================================================

/// 读取消息中第 `index` 个图片的字节（原生二进制响应）。
///
/// 参数:
/// - `state`: 桌面状态（含服务层引擎）。
/// - `message_id`: 目标消息 UUID 字符串。
/// - `index`: 图片序号（正文中第几张图片，从 0 起）。
///
/// 返回:
/// - `Ok(Response)`: 图片字节（前端构造 Blob URL 展示）。
/// - `Err(String)`: 消息不存在 / 序号越界 / 无本地文件 / 读取失败。
///
/// 说明:
/// - 读取链路（客户端缓存优先 → 导出根定位）由服务层用例承载；
/// - 缓存目录定位失败按错误返回（不写入不明目录）。
#[tauri::command]
pub async fn read_attachment_image(
    state: State<'_, DesktopState>,
    message_id: String,
    index: u32,
) -> Result<tauri::ipc::Response, String> {
    let message_id = Uuid::parse_str(&message_id).map_err(|e| format!("无效的消息 ID: {}", e))?;
    let Some(cache_dir) = image_cache_dir(&state.engine) else {
        tracing::warn!("数据库路径无父目录，无法确定图片缓存目录");
        return Err("无法确定图片缓存目录".to_string());
    };

    let image = state
        .engine
        .read_message_attachment_image(message_id, index as usize, &cache_dir)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "读取图片失败"))?;

    tracing::debug!(
        attachment_id = image.attachment_id,
        bytes_len = image.bytes.len(),
        mime = %image.mime,
        from_cache = image.from_cache,
        "read_attachment_image 完成"
    );
    Ok(tauri::ipc::Response::new(image.bytes))
}

// =========================================================
// clear_image_cache — 清理图片缓存
// =========================================================

/// 图片缓存清理结果。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCacheResult {
    /// 删除的文件数
    pub files: usize,
    /// 删除的字节数
    pub bytes: u64,
}

/// 清理客户端图片缓存（仅第一层文件，不递归子目录）。
///
/// 参数:
/// - `state`: 桌面状态（含服务层引擎）。
///
/// 返回:
/// - `Ok(ClearCacheResult)`: 删除的文件数与字节数（目录不存在时全 0）。
/// - `Err(String)`: 缓存目录不可读。
#[tauri::command]
pub async fn clear_image_cache(state: State<'_, DesktopState>) -> Result<ClearCacheResult, String> {
    let Some(cache_dir) = image_cache_dir(&state.engine) else {
        tracing::warn!("数据库路径无父目录，无法确定图片缓存目录");
        return Err("无法确定图片缓存目录".to_string());
    };

    let stat = state
        .engine
        .clear_attachment_cache(&cache_dir)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "清理图片缓存失败"))?;

    tracing::info!(files = stat.files, bytes = stat.bytes, "图片缓存已清理");
    Ok(ClearCacheResult {
        files: stat.files,
        bytes: stat.bytes,
    })
}
