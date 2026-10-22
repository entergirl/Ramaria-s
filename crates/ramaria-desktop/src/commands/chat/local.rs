//! crates/ramaria-desktop/src/commands/chat/local.rs - Ramaria 本地会话消息命令
//!
//! 设计特点:
//! - `recall_message`: 客户端内撤回，窗口与发送方校验由服务层裁定
//! - `send_local_image`: 本地发图 = 路径校验 → 体积预检 → 落库 → 后台理解（不阻塞返回）
//! - 视图结构只承载前端展示所需三个字段，序列化为 camelCase

use crate::DesktopState;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

// =========================================================
// recall_message — 客户端内撤回
// =========================================================

/// 撤回一条本地消息（客户端内撤回）。
///
/// 参数:
/// - `message_id`: 目标消息 UUID 字符串（仅本人发送的本地消息、窗口内）。
///
/// 返回:
/// - `Ok(())` 表示撤回标记已落库；
/// - 校验失败（不存在 / 非本人 / 超窗口）返回面向界面的原因。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn recall_message(
    state: State<'_, DesktopState>,
    message_id: String,
) -> Result<(), String> {
    let id =
        uuid::Uuid::parse_str(message_id.trim()).map_err(|_| "消息 ID 格式无效".to_string())?;

    state
        .engine
        .recall_local_message(id)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "撤回消息失败"))?;

    tracing::info!(message_id = %id, "客户端内撤回完成");
    Ok(())
}

// =========================================================
// send_local_image — 本地会话发送图片
// =========================================================

/// 本地发图结果视图（前端展示用）。
///
/// 字段约定:
/// - `message_id`: 落库的用户消息 id（字符串形式）；
/// - `placeholder`: 正文中的图片占位符（`[图片#hash]`）；
/// - `content`: 最终落库正文（说明文字 + 占位符）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendLocalImageView {
    pub message_id: String,
    pub placeholder: String,
    pub content: String,
}

/// 在本地会话中发送一张本地图片。
///
/// 流程:
/// 1. 会话 ID 解析与文件路径安全校验（用户主动选择的本地文件）；
/// 2. 读取字节并取文件名；
/// 3. 委托服务层落库（消息 + 附件行 + 缓存落盘）；
/// 4. 后台理解图片（不阻塞命令返回，失败仅告警）。
///
/// 参数:
/// - `session_id`: 目标本地会话 UUID 字符串（社交通道会话拒绝）；
/// - `file_path`: 用户选择的图片文件路径；
/// - `caption`: 可选说明文字。
///
/// 返回:
/// - 落库消息定位与正文（图片描述由后台理解稍后回填）。
#[tauri::command]
#[tracing::instrument(skip(state, file_path, caption))]
pub async fn send_local_image(
    state: State<'_, DesktopState>,
    session_id: String,
    file_path: String,
    caption: Option<String>,
) -> Result<SendLocalImageView, String> {
    // ---- 参数与路径校验 ----
    let session_id =
        uuid::Uuid::parse_str(session_id.trim()).map_err(|_| "会话 ID 格式无效".to_string())?;
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    // ---- 读取字节与文件名（读前按体积预检，避免整体读入超大文件） ----
    let metadata =
        std::fs::metadata(&real_path).map_err(|e| format!("读取图片文件信息失败: {e}"))?;
    if metadata.len() > ramaria_service::MAX_LOCAL_IMAGE_BYTES {
        return Err(format!(
            "图片超过体积上限（{} MB）",
            ramaria_service::MAX_LOCAL_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(&real_path).map_err(|e| format!("读取图片文件失败: {e}"))?;
    let file_name = real_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| "无法获取图片文件名".to_string())?;

    // ---- 落库（消息 + 附件行 + 缓存） ----
    let outcome = state
        .engine
        .send_local_image(ramaria_service::LocalImageSendRequest {
            session_id,
            file_name,
            bytes: bytes.clone(),
            caption,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "发送图片失败"))?;

    // ---- 后台理解（不阻塞返回；失败不改变已落库结果） ----
    let engine = Arc::clone(&state.engine);
    let attachment_id = outcome.attachment_id;
    let md5 = outcome.md5.clone();
    tokio::spawn(async move {
        if let Err(e) = engine
            .local_understand_image(attachment_id, &md5, &bytes)
            .await
        {
            tracing::warn!(error = %e, "本地图片理解任务失败（不阻塞）");
        }
    });

    tracing::info!(
        session_id = %session_id,
        message_id = %outcome.message_id,
        "本地图片已发送（理解任务后台执行）"
    );
    Ok(SendLocalImageView {
        message_id: outcome.message_id.to_string(),
        placeholder: outcome.placeholder,
        content: outcome.content,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 视图序列化：字段名 camelCase、字段完整且无多余键。
    #[test]
    fn send_local_image_view_serializes_camel_case() {
        let view = SendLocalImageView {
            message_id: "0b1c2d3e".to_string(),
            placeholder: "[图片#abcd1234]".to_string(),
            content: "看这个[图片#abcd1234]".to_string(),
        };
        let value = serde_json::to_value(&view).expect("序列化应成功");
        assert_eq!(value["messageId"], "0b1c2d3e");
        assert_eq!(value["placeholder"], "[图片#abcd1234]");
        assert_eq!(value["content"], "看这个[图片#abcd1234]");
        let object = value.as_object().expect("应为 JSON 对象");
        assert_eq!(object.len(), 3, "字段应完整且无多余键");
    }
}
