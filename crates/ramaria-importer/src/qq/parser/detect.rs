//! crates/ramaria-importer/src/qq/parser/detect.rs - QQ 导出格式检测与多编码解码
//!
//! 设计特点:
//! - 格式检测以 `{` 开头且同时含 `chatInfo` / `messages` 字段为特征
//! - 检测读取失败时回退二进制路径，兼容 GBK 与 UTF-8 BOM
//! - `decode_bytes` 五级降级链（UTF-8 → BOM → UTF-16-LE → GBK → Latin-1）
//! - UTF-16-LE 依赖 BOM 识别避免误判，Latin-1 兜底永不失败、不 panic

use std::fs;
use std::path::Path;

use ramaria_core::error::RamariaResult;

use crate::error;

// =========================================================
// 格式检测
// =========================================================

/// 检测文件是否为 qq-chat-exporter 导出的 JSON 格式 QQ 聊天记录。
///
/// 检测方式:
/// - 读取文件内容（多编码尝试），判断是否以 `{` 开头且同时包含 `"chatInfo"` 和 `"messages"` 字段。
///
/// 返回:
/// - `true`: 文件是 qce v6.x JSON 格式。
/// - `false`: 文件格式不匹配。
pub fn detect_qq_format(file_path: &Path) -> RamariaResult<bool> {
    // 尝试以 UTF-8 读取；失败则走二进制多编码路径
    let content = match fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(_) => {
            let bytes = fs::read(file_path)
                .map_err(|e| error::read_error(&file_path.display().to_string(), e))?;

            // 检测 UTF-8 BOM → 去掉 BOM 后再转 UTF-8
            if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
                match String::from_utf8(bytes[3..].to_vec()) {
                    Ok(s) => s,
                    Err(_) => return Ok(false),
                }
            } else {
                // 尝试 GBK（部分中文环境可能以 GBK 编码保存）
                match encoding_rs::GBK.decode(&bytes) {
                    (s, _, false) => s.into_owned(),
                    _ => return Ok(false),
                }
            }
        }
    };

    let trimmed = content.trim();

    // 检测 qce JSON 格式特征：以 { 开头且同时含 chatInfo 和 messages 字段
    if trimmed.starts_with('{')
        && trimmed.contains("\"chatInfo\"")
        && trimmed.contains("\"messages\"")
    {
        return Ok(true);
    }

    Ok(false)
}

// =========================================================
// 编码解码
// =========================================================

/// 多编码尝试解码字节数组为字符串。
///
/// 五级降级链（按优先级）:
/// 1. UTF-8 ── 绝大多数 qce 导出文件的编码
/// 2. UTF-8 BOM ── 部分编辑器添加 BOM 头
/// 3. UTF-16 LE ── Windows 某些版本 QQ 的默认编码
/// 4. GBK ── 简体中文 Windows 的旧版默认编码
/// 5. Latin-1 兜底 ── 永不失败，单字节映射（可能乱码但不会 panic）
pub(super) fn decode_bytes(bytes: &[u8]) -> RamariaResult<String> {
    // 1. UTF-8
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        return Ok(s);
    }

    // 2. UTF-8 BOM
    if bytes.len() >= 3
        && bytes[0] == 0xEF
        && bytes[1] == 0xBB
        && bytes[2] == 0xBF
        && let Ok(s) = String::from_utf8(bytes[3..].to_vec())
    {
        return Ok(s);
    }

    // 3. UTF-16 LE (BOM: 0xFF 0xFE)
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let utf16: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| u16::from_le_bytes(*chunk))
            .collect();
        if let Ok(s) = String::from_utf16(&utf16) {
            return Ok(s);
        }
    }

    // 4. GBK
    let (decoded, _, has_errors) = encoding_rs::GBK.decode(bytes);
    if !has_errors {
        return Ok(decoded.into_owned());
    }

    // 5. Latin-1 兜底（永不失败）
    let latin1: String = bytes.iter().map(|&b| b as char).collect();
    Ok(latin1)
}
