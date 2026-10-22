//! crates/ramaria-service/src/digest.rs - Ramaria 内容摘要辅助
//!
//! 设计特点:
//! - 单一实现：本地图片与表情资产共用同一份 md5 计算，避免两份等价代码漂移
//! - 输出小写 hex，直接用作缓存 / 资产文件名
//! - 不读文件、不落盘，纯字节计算

use md5::{Digest, Md5};

/// 计算字节的 md5（小写 hex）。
pub(crate) fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
