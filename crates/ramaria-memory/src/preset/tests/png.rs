//! crates/ramaria-memory/src/preset/tests/png.rs - PNG 角色卡提取与 base64 解码用例
//!
//! 设计特点:
//! - 覆盖最小 PNG 构造下的 chara 提取成功、无 chara、签名/数据损坏
//! - 覆盖 base64 解码向量（含 padding 缺失、空白换行与非法字符）

use crate::preset::png::{decode_base64, extract_chara_text};

use super::*;

// =========================================================
// PNG 提取
// =========================================================

/// 含 tEXt chara 的最小 PNG：提取出 JSON 文本（含中文，验证 UTF-8 链路）。
#[test]
fn extract_chara_from_minimal_png() {
    let json = r#"{"name":"黎杋枫","description":"测试"}"#;
    let png = build_png_with_chara(json);
    let extracted = extract_chara_text(&png).expect("提取应成功");
    assert_eq!(extracted.as_deref(), Some(json));
}

/// 无 chara 的 PNG：Ok(None)。
#[test]
fn extract_without_chara_returns_none() {
    let png = build_png_without_chara();
    assert_eq!(extract_chara_text(&png).expect("解析应成功"), None);
}

/// 非 chara 的 tEXt 块：继续遍历，返回 None。
#[test]
fn extract_ignores_other_text_chunks() {
    let mut png = png_signature();
    png.extend_from_slice(&png_chunk(b"IHDR", &[0u8; 13]));
    let mut text = b"Comment".to_vec();
    text.push(0);
    text.extend_from_slice(b"not a character card");
    png.extend_from_slice(&png_chunk(b"tEXt", &text));
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    assert_eq!(extract_chara_text(&png).expect("解析应成功"), None);
}

/// 签名非法：Err。
#[test]
fn invalid_signature_is_error() {
    let bytes = b"not a png file at all";
    assert!(extract_chara_text(bytes).is_err());
    // 长度不足签名
    assert!(extract_chara_text(&[0x89, 0x50]).is_err());
}

/// chara 数据含非法 base64 字符：Err。
#[test]
fn invalid_base64_in_chara_is_error() {
    let mut png = png_signature();
    png.extend_from_slice(&png_chunk(b"IHDR", &[0u8; 13]));
    let mut text = b"chara".to_vec();
    text.push(0);
    text.extend_from_slice(b"not*valid*base64");
    png.extend_from_slice(&png_chunk(b"tEXt", &text));
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    assert!(extract_chara_text(&png).is_err());
}

/// chunk 声明长度超出实际数据：Err（截断检测）。
#[test]
fn truncated_chunk_data_is_error() {
    let mut png = png_signature();
    png.extend_from_slice(&png_chunk(b"IHDR", &[0u8; 13]));
    // 声明 100 字节但只给 4 字节
    png.extend_from_slice(&100u32.to_be_bytes());
    png.extend_from_slice(b"tEXt");
    png.extend_from_slice(b"data");
    assert!(extract_chara_text(&png).is_err());
}

/// chunk 头不完整：Err。
#[test]
fn incomplete_chunk_header_is_error() {
    let mut png = png_signature();
    png.extend_from_slice(&[0u8; 3]);
    assert!(extract_chara_text(&png).is_err());
}

// =========================================================
// base64 解码
// =========================================================

/// 标准向量与省略 padding。
#[test]
fn base64_decodes_standard_vectors() {
    let cases = [
        ("aGVsbG8=", b"hello".to_vec()),
        ("aGVsbG8", b"hello".to_vec()),
        ("aGk=", b"hi".to_vec()),
        ("aGk", b"hi".to_vec()),
        ("YQ==", b"a".to_vec()),
        ("YQ", b"a".to_vec()),
        ("", Vec::new()),
    ];
    for (input, expected) in cases {
        assert_eq!(
            decode_base64(input).expect("解码应成功"),
            expected,
            "input={input}"
        );
    }
}

/// 容忍空白与换行（多行 base64 文本）。
#[test]
fn base64_tolerates_whitespace_and_newlines() {
    let decoded = decode_base64("aGVs\nbG8g\r\nd29ybGQ=").expect("解码应成功");
    assert_eq!(decoded, b"hello world".to_vec());
}

/// 非法字符：Err。
#[test]
fn base64_rejects_invalid_characters() {
    for input in ["aGVs*bG8=", "aGVs🎉", "-_-"] {
        assert!(decode_base64(input).is_err(), "input={input}");
    }
}

/// 填充后仍含有效字符：Err。
#[test]
fn base64_rejects_data_after_padding() {
    assert!(decode_base64("aGk=aGk").is_err());
}

/// 非法长度（单字符尾）：Err。
#[test]
fn base64_rejects_invalid_length() {
    assert!(decode_base64("a").is_err());
    assert!(decode_base64("aGVsb").is_err());
}

/// 编码-解码往返一致（含各字节长度余数）。
#[test]
fn base64_roundtrip_with_encoder() {
    for len in [0usize, 1, 2, 3, 4, 5, 6, 7] {
        let data: Vec<u8> = (0..len).map(|i| (i * 37 + 3) as u8).collect();
        let encoded = encode_base64(&data);
        assert_eq!(
            decode_base64(&encoded).expect("解码应成功"),
            data,
            "len={len}"
        );
    }
}
