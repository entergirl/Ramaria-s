//! crates/ramaria-service/src/import/detect.rs - Ramaria QQ 聊天记录格式探测与解析前校验模块
//!
//! 设计特点:
//! - 基于文件内容特征判定 qq-chat-exporter v6.x JSON（扩展名白名单与文件存在性校验由调用方承担）
//! - 探测失败（读取 / 编码级错误）记 warn 后上抛结构化错误，入口按需补错误前缀
//! - 扩展名校验（仅 `.json`）与日志路径标签（只留文件名）为解析链路共用
//! - Engine 门面 `detect_qq_format` 供桌面 / CLI 共用

use std::path::Path;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_importer::ImportSource;
use ramaria_importer::qq::QqImporter;

use crate::engine::Engine;

// =========================================================
// 用例入口
// =========================================================

/// 探测文件是否为 QQ 聊天记录支持的格式（桌面 / CLI 共用）。
///
/// 说明:
/// - 检测基于文件内容特征（qq-chat-exporter v6.x JSON），扩展名白名单与
///   文件存在性校验由调用方自行处理；
/// - 探测失败（读取 / 编码级错误）记 warn 后上抛结构化错误，入口按需补错误前缀。
///
/// 参数:
/// - `_engine`: 服务层引擎（探测不依赖引擎，保留参数与其余用例入口一致）；
/// - `path`: 待检测文件路径。
///
/// 返回:
/// - `Ok(true)`: 文件格式匹配；
/// - `Ok(false)`: 格式不匹配（调用方按各自文案提示用户）。
pub(crate) async fn detect_format(_engine: &Engine, path: &Path) -> RamariaResult<bool> {
    let importer = QqImporter::new();
    let is_qq = importer.detect_format(path).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录格式检测失败");
    })?;
    Ok(is_qq)
}

// =========================================================
// 内部工具
// =========================================================

/// 校验导入文件扩展名（仅支持 qq-chat-exporter v6.x 导出的 .json）。
pub(super) fn ensure_json_extension(path: &Path) -> RamariaResult<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" {
        return Err(RamariaError::validation(format!(
            "不支持的文件类型: .{ext}（仅支持 qq-chat-exporter v6.x 导出的 .json）"
        )));
    }
    Ok(())
}

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
pub(super) fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}
