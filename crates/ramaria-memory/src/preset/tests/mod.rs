//! crates/ramaria-memory/src/preset/tests/mod.rs - Ramaria 预设解析测试模块
//!
//! 设计特点:
//! - 目录化拆分：超集解析 / 注册段 / 结构化段 / PNG / ST 卡 / 兼容包装 / 序列化 / 预览 / 导入计划按域分文件
//! - 本文件仅保留子模块声明与跨域共享辅助（上下文构造、PNG 字节构造、base64 编码）
//! - 由 preset/mod.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离
//! - 用例为确定性断言，可离线运行

mod compat;
mod identity;
mod plan;
mod png;
mod preview;
mod sections;
mod ser;
mod st;
mod superset;

use super::*;

use ramaria_core::types::PersonaKind;
use ramaria_core::{ProfileField, TraitLayer};

// =========================================================
// 共享辅助
// =========================================================

/// 默认解析上下文（无文件 stem，source 为 manual）。
fn ctx_default() -> PresetParseContext {
    PresetParseContext::default()
}

/// 指定文件名 stem 的解析上下文。
fn ctx_with_stem(stem: &str) -> PresetParseContext {
    PresetParseContext {
        file_stem: Some(stem.to_string()),
        default_source: "manual".to_string(),
    }
}

/// 解析并断言成功。
fn parse_ok(content: &str, ctx: &PresetParseContext) -> ParsedPreset {
    parse_toml(content, ctx).expect("解析应当成功")
}

/// 断言存在指定 hint 码。
fn assert_hint(parsed: &ParsedPreset, code: &str) {
    let codes: Vec<&str> = parsed.hints.iter().map(|h| h.code.as_str()).collect();
    assert!(
        parsed.hints.iter().any(|h| h.code == code),
        "缺少提示 {code}；实际: {codes:?}"
    );
}

/// 统计指定类别与原因的跳过条目数。
fn count_skipped(parsed: &ParsedPreset, category: SkipCategory, reason: SkipReason) -> usize {
    parsed
        .skipped
        .iter()
        .filter(|s| s.category == category && s.reason == reason)
        .count()
}

/// 标准 base64 编码（测试辅助，生成 PNG chara 数据）。
fn encode_base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 构造 PNG chunk 字节（长度 + 类型 + 数据 + CRC 占位）。
fn png_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0u8; 4]);
    out
}

/// PNG 文件签名。
fn png_signature() -> Vec<u8> {
    vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
}

/// 构造含 tEXt chara 的最小 PNG（签名 + IHDR + tEXt(chara) + IEND）。
fn build_png_with_chara(json: &str) -> Vec<u8> {
    let mut out = png_signature();
    out.extend_from_slice(&png_chunk(b"IHDR", &[0u8; 13]));
    let mut text = b"chara".to_vec();
    text.push(0);
    text.extend_from_slice(encode_base64(json.as_bytes()).as_bytes());
    out.extend_from_slice(&png_chunk(b"tEXt", &text));
    out.extend_from_slice(&png_chunk(b"IEND", &[]));
    out
}

/// 构造无 chara 的最小 PNG（签名 + IHDR + IEND）。
fn build_png_without_chara() -> Vec<u8> {
    let mut out = png_signature();
    out.extend_from_slice(&png_chunk(b"IHDR", &[0u8; 13]));
    out.extend_from_slice(&png_chunk(b"IEND", &[]));
    out
}
