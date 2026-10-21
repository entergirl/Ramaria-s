//! crates/ramaria-core/src/types/vector/tests.rs - 记忆向量类型测试
//!
//! 设计特点:
//! - 覆盖编解码往返、空字节与非法长度三类边界
//! - 校验落库字节序为小端

use super::*;

#[test]
fn encode_decode_roundtrip() {
    let v = vec![0.5_f32, -1.25, 3.0, 0.0];
    let bytes = encode_vector(&v);
    assert_eq!(bytes.len(), 16);
    assert_eq!(decode_vector(&bytes).unwrap(), v);
}

#[test]
fn decode_rejects_empty() {
    assert!(decode_vector(&[]).is_err());
}

#[test]
fn decode_rejects_misaligned_length() {
    assert!(decode_vector(&[0, 1, 2]).is_err());
}

#[test]
fn encode_is_little_endian() {
    let bytes = encode_vector(&[1.0_f32]);
    assert_eq!(bytes, 1.0_f32.to_le_bytes().to_vec());
}
