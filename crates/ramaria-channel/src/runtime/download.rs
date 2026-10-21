//! crates/ramaria-channel/src/runtime/download.rs - Ramaria 图片下载模块
//!
//! 设计特点:
//! - 下载抽象 trait：编排层依赖 [`ImageDownloader`]，HTTP 实现独立（测试可注入 stub）
//! - 上限与图片理解链路对齐（20MB）；先按响应头预检、读体后复检
//! - 错误文案不含完整 URL（平台图片地址含时效性凭据），仅记原因分类

use std::time::Duration;

use async_trait::async_trait;

use crate::{ChannelError, ChannelResult};

// =========================================================
// 常量
// =========================================================

/// 单张图片体积上限（字节；与图片理解链路对齐）。
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 单次下载总超时。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

// =========================================================
// 下载抽象
// =========================================================

/// 图片下载器（编排层依赖抽象，测试注入 stub）。
///
/// 实现约定:
/// - 返回完整字节；体积超限 / 非 2xx / 网络失败均为 `ChannelError`；
/// - 错误文案与日志不含完整 URL。
#[async_trait]
pub trait ImageDownloader: Send + Sync {
    /// 下载一个图片地址的完整字节。
    async fn download(&self, url: &str) -> ChannelResult<Vec<u8>>;
}

// =========================================================
// HTTP 实现
// =========================================================

/// HTTP 图片下载器（reqwest）。
///
/// 字段约定:
/// - `max_bytes`: 单张体积上限（默认 [`MAX_IMAGE_BYTES`]）。
pub struct HttpImageDownloader {
    client: reqwest::Client,
    max_bytes: u64,
}

impl HttpImageDownloader {
    /// 以默认参数构造（总超时 15s、上限 20MB）。
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            max_bytes: MAX_IMAGE_BYTES,
        }
    }

    /// 以指定体积上限构造（0 回落默认值）。
    pub fn with_max_bytes(max_bytes: u64) -> Self {
        Self {
            client: reqwest::Client::new(),
            max_bytes: if max_bytes == 0 {
                MAX_IMAGE_BYTES
            } else {
                max_bytes
            },
        }
    }

    /// 当前体积上限（字节）。
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

impl Default for HttpImageDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ImageDownloader for HttpImageDownloader {
    async fn download(&self, url: &str) -> ChannelResult<Vec<u8>> {
        if url.trim().is_empty() {
            return Err(ChannelError::Protocol("图片地址为空".to_string()));
        }

        let response = self
            .client
            .get(url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .map_err(classify_request_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ChannelError::Protocol(format!(
                "图片下载失败（HTTP {}）",
                status.as_u16()
            )));
        }
        if let Some(length) = response.content_length() {
            if length > self.max_bytes {
                return Err(ChannelError::Protocol(format!(
                    "图片超过体积上限（{length} 字节）"
                )));
            }
        }
        read_body_capped(response, self.max_bytes, "图片").await
    }
}

/// 按块读取响应体并在超限时立即中止（不把超大响应整体缓冲进内存）。
///
/// 参数:
/// - `response`: 已确认状态码成功的下载响应。
/// - `max_bytes`: 允许的最大字节数。
/// - `label`: 错误文案中的对象名（图片 / 文件）。
///
/// 返回:
/// - 完整字节；任一块读取失败或累计超限时返回协议错误（文案不含 URL）。
pub(crate) async fn read_body_capped(
    response: reqwest::Response,
    max_bytes: u64,
    label: &str,
) -> Result<Vec<u8>, ChannelError> {
    use futures::StreamExt;

    let mut stream = response.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ChannelError::Protocol(format!("{label}下载读取失败")))?;
        let next_len = bytes.len() as u64 + chunk.len() as u64;
        if next_len > max_bytes {
            return Err(ChannelError::Protocol(format!(
                "{label}超过体积上限（{next_len} 字节）"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// 请求错误分类（文案不含 URL）。
fn classify_request_error(error: reqwest::Error) -> ChannelError {
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "连接失败"
    } else {
        "网络错误"
    };
    ChannelError::Protocol(format!("图片下载失败（{reason}）"))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
