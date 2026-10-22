//! crates/ramaria-service/src/persona/import/parse.rs - Ramaria 预设读取与解析
//!
//! 设计特点:
//! - 文件读取与解析共用一套上下文（格式解析参数与文件名口径）
//! - 隐私确认只在预览 / 执行前询问一次，结果随预览参数传递
//! - 文件名只留文件名本身，路径错误信息不含完整路径

use std::path::Path;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::TraitLayer;
use ramaria_memory::preset::{ParsedPreset, PresetParseContext, PreviewOptions, parse_preset};

use crate::engine::Engine;

// =========================================================
// 辅助
// =========================================================

/// 读取预设文件字节并解析（错误消息含文件名、不含正文）。
pub(super) fn read_and_parse(path: &Path) -> RamariaResult<(Vec<u8>, ParsedPreset)> {
    let bytes = std::fs::read(path).map_err(|e| {
        RamariaError::io(format!("读取预设文件失败: {}", file_label(path)), Some(e))
    })?;
    let parsed = parse_preset(&bytes, &parse_context())?;
    Ok((bytes, parsed))
}

/// 预设导入通道的解析上下文。
///
/// 说明:
/// - uid 缺省口径为自动生成（文件名 stem 仅 `persona reload` 目录通道使用），
///   故不携带文件 stem，避免任意文件名被当作主键；显式 `[persona].uid` 仍优先（解析器内部处理）。
fn parse_context() -> PresetParseContext {
    PresetParseContext {
        file_stem: None,
        default_source: "manual".to_string(),
    }
}

/// 预览选项（dry-run 与执行结果预览共用同一构造口径）。
pub(super) fn preview_options(
    path: &Path,
    merge_target: Option<&str>,
    no_ai: bool,
    privacy_confirmed: bool,
) -> PreviewOptions {
    PreviewOptions {
        file_name: file_name(path),
        merge_target: merge_target.map(str::to_string),
        decompose_enabled: !no_ai,
        privacy_confirmed,
    }
}

/// 预览用隐私确认查询：读取失败按未确认处理（预览不应因隐私读取失败而失败）。
pub(super) async fn preview_privacy_confirmed(engine: &Engine, no_ai: bool) -> bool {
    if no_ai {
        return false;
    }
    match crate::privacy::online_privacy_confirmed(engine).await {
        Ok(confirmed) => confirmed,
        Err(e) => {
            tracing::debug!(error = %e, "隐私确认读取失败，预览按未确认处理");
            false
        }
    }
}

/// 文件显示名（含扩展名；用于预览展示与日志）。
pub(super) fn file_label(path: &Path) -> String {
    file_name(path).unwrap_or_else(|| path.display().to_string())
}

/// 文件名（含扩展名；无文件名时为 None）。
pub(super) fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// 层索引（0/1/2 = Base/Primary/Accent；`trait_max_seq` 数组下标）。
pub(super) fn layer_index(layer: TraitLayer) -> usize {
    match layer {
        TraitLayer::Base => 0,
        TraitLayer::Primary => 1,
        TraitLayer::Accent => 2,
        _ => 2,
    }
}

/// 从 uid 提取序号（`{kind}-{seq}` 的 seq 段；无法解析时回退 1）。
pub(super) fn extract_seq(uid: &str) -> i64 {
    uid.rsplit('-')
        .next()
        .and_then(|seq| seq.parse::<i64>().ok())
        .unwrap_or(1)
}
