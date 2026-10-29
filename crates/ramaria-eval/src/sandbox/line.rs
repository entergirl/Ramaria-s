//! crates/ramaria-eval/src/sandbox/line.rs - Ramaria 评测沙盒行模式 I/O
//!
//! 设计特点:
//! - 只做标准输入读取与阶段行格式化，解析与运行在各自模块
//! - 空输入按校验错误返回并附格式提示，避免运行空沙盒
//! - 阶段行写标准错误，最终报告写标准输出

use ramaria_core::error::{RamariaError, RamariaResult};
use tokio::io::AsyncReadExt;

use super::report::StageReport;

/// 行模式的格式提示（读取失败与解析失败共用）。
pub(crate) const LINE_MODE_HINT: &str = "行模式格式：可选头部指令 @backend inherit|local|cloud、@base-url <地址>、@model <标识>、@embedding <路径>、@persona <路径>，随后 user:/assistant: 对话，单独一行 --- 后写问题";

/// 读取标准输入全文。
///
/// 返回:
/// - 成功时返回原始文本（未做裁剪）；
/// - 读取失败返回 `Io` 错误；输入为空或仅空白返回 `Validation` 错误。
pub(crate) async fn read_stdin() -> RamariaResult<String> {
    let mut text = String::new();
    tokio::io::stdin()
        .read_to_string(&mut text)
        .await
        .map_err(|error| RamariaError::io("读取标准输入失败", Some(error)))?;
    if text.trim().is_empty() {
        return Err(RamariaError::validation(format!(
            "标准输入为空；{LINE_MODE_HINT}"
        )));
    }
    Ok(text)
}

/// 把阶段上报格式化为单行文本（写标准错误）。
pub(crate) fn format_stage(stage: &StageReport) -> String {
    format!(
        "[{}] {} {}ms - {}",
        stage.status.as_str(),
        stage.kind.as_str(),
        stage.elapsed_ms,
        stage.detail
    )
}
