//! crates/ramaria-memory/src/preset/png.rs - Ramaria 预设 PNG 角色卡提取模块
//!
//! 设计特点:
//! - 从 PNG 字节提取 tEXt chunk 的 chara 数据（SillyTavern 角色卡内嵌 JSON）
//! - 标准 base64 解码（RFC 4648；容忍空白换行与省略 padding），零外部依赖
//! - chunk 遍历只做边界与长度检查，不校验 CRC

use ramaria_core::{RamariaError, RamariaResult};

/// PNG 文件签名。
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
/// 单个 chunk 数据长度上限（16 MiB）。
const MAX_CHUNK_DATA: usize = 16 * 1024 * 1024;

/// 判断字节是否为 PNG 文件（签名匹配）。
pub(crate) fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= PNG_SIGNATURE.len() && bytes[..PNG_SIGNATURE.len()] == PNG_SIGNATURE
}

/// 从 PNG 字节提取 tEXt `chara` 数据（base64 解码后的 JSON 文本）。
///
/// 参数:
/// - `bytes`: PNG 文件原始字节。
///
/// 返回:
/// - `Ok(Some(json))`: 找到 chara 数据并解码成功。
/// - `Ok(None)`: PNG 合法但无 chara 数据。
/// - `Err`: 签名非法或 chunk 数据损坏。
pub fn extract_chara_text(bytes: &[u8]) -> RamariaResult<Option<String>> {
    if !is_png(bytes) {
        return Err(RamariaError::validation(
            "PNG 签名非法：文件不是有效的 PNG".to_string(),
        ));
    }

    let mut cursor = PNG_SIGNATURE.len();
    while cursor < bytes.len() {
        if cursor + 8 > bytes.len() {
            return Err(RamariaError::validation(
                "PNG 数据截断：chunk 头不完整".to_string(),
            ));
        }
        let length = u32::from_be_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]) as usize;
        if length > MAX_CHUNK_DATA {
            return Err(RamariaError::validation(
                "PNG chunk 数据超限（上限 16 MiB）".to_string(),
            ));
        }
        let data_start = cursor + 8;
        let Some(data_end) = data_start.checked_add(length) else {
            return Err(RamariaError::validation("PNG chunk 长度非法".to_string()));
        };
        if data_end > bytes.len() || bytes.len() - data_end < 4 {
            return Err(RamariaError::validation(
                "PNG 数据截断：chunk 内容或 CRC 不完整".to_string(),
            ));
        }

        let chunk_type = &bytes[cursor + 4..cursor + 8];
        let data = &bytes[data_start..data_end];
        if chunk_type == b"tEXt" {
            if let Some(encoded) = chara_payload(data)? {
                let decoded = decode_base64(encoded.trim())?;
                let text = String::from_utf8(decoded).map_err(|e| {
                    RamariaError::validation(format!("PNG chara 数据不是合法 UTF-8: {e}"))
                })?;
                return Ok(Some(text));
            }
        }

        cursor = data_end + 4;
    }

    Ok(None)
}

/// tEXt 数据解析：主关键字为 `chara` 时返回其 base64 文本，否则返回 None。
fn chara_payload(data: &[u8]) -> RamariaResult<Option<&str>> {
    let Some(separator) = data.iter().position(|&b| b == 0) else {
        return Ok(None);
    };
    if &data[..separator] != b"chara" {
        return Ok(None);
    }
    std::str::from_utf8(&data[separator + 1..])
        .map(Some)
        .map_err(|_| RamariaError::validation("PNG chara 数据编码非法".to_string()))
}

/// 标准 base64 解码（RFC 4648；容忍空白换行与省略 padding；非法字符 → 错误）。
pub(crate) fn decode_base64(input: &str) -> RamariaResult<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut padding = 0usize;

    for ch in input.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        if ch == '=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err(RamariaError::validation(
                "base64 数据在填充后仍含有效字符".to_string(),
            ));
        }
        let Some(value) = base64_value(ch) else {
            return Err(RamariaError::validation(format!(
                "base64 数据含非法字符: {ch}"
            )));
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }

    match bits {
        0 | 2 | 4 => Ok(out),
        _ => Err(RamariaError::validation("base64 数据长度非法".to_string())),
    }
}

/// 单字符 base64 值。
fn base64_value(ch: char) -> Option<u32> {
    match ch {
        'A'..='Z' => Some(ch as u32 - 'A' as u32),
        'a'..='z' => Some(ch as u32 - 'a' as u32 + 26),
        '0'..='9' => Some(ch as u32 - '0' as u32 + 52),
        '+' => Some(62),
        '/' => Some(63),
        _ => None,
    }
}
