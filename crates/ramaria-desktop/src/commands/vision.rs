//! crates/ramaria-desktop/src/commands/vision.rs - 图片理解声明 Tauri Command
//!
//! 设计特点:
//! - 单键写入：只改 `[vision].model_supports_vision`，其余配置键由服务层按当前生效值保留
//! - 双侧回执原样透出（文件 / DB / 失败明细），前端据此提示同步失败
//! - 只做参数转换与结果映射，配置读写全部委托服务层用例

use crate::DesktopState;
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 图片理解声明写入结果。
///
/// 字段约定:
/// - `file_ok`: config.toml 是否写入成功；
/// - `db_ok`: settings / backend_config 表是否全部写入成功；
/// - `failures`: 未成功步骤的失败原因（双侧均成功时为空）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionSupportResult {
    pub file_ok: bool,
    pub db_ok: bool,
    pub failures: Vec<String>,
}

// =========================================================
// set_vision_support — 写入图片理解声明
// =========================================================

/// 写入图片理解声明（`[vision].model_supports_vision`）。
///
/// 参数:
/// - `enabled`: 当前对话模型是否支持图片识别。
///
/// 返回:
/// - 双侧写入结果（单侧失败降级不阻塞，失败原因在 `failures`）；
/// - 读取或写入失败时返回用户可读错误文案。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_vision_support(
    state: State<'_, DesktopState>,
    enabled: bool,
) -> Result<VisionSupportResult, String> {
    let outcome = state
        .engine
        .set_vision_support(enabled)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "保存图片理解声明失败"))?;

    tracing::info!(
        enabled,
        file_ok = outcome.file_ok,
        db_ok = outcome.db_ok,
        failures = outcome.failures.len(),
        "图片理解声明写入完成"
    );

    Ok(VisionSupportResult {
        file_ok: outcome.file_ok,
        db_ok: outcome.db_ok,
        failures: outcome.failures,
    })
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 回执字段按 camelCase 序列化，前端直接读取 fileOk / dbOk。
    #[test]
    fn result_serializes_camel_case() {
        let view = VisionSupportResult {
            file_ok: true,
            db_ok: false,
            failures: vec!["settings 表写入失败".to_string()],
        };
        let json = serde_json::to_string(&view).expect("序列化失败");
        assert!(json.contains(r#""fileOk":true"#), "{json}");
        assert!(json.contains(r#""dbOk":false"#), "{json}");
        assert!(json.contains("settings 表写入失败"), "{json}");
    }
}
