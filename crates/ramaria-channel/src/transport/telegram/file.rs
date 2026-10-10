//! crates/ramaria-channel/src/transport/telegram/file.rs - Telegram 媒体下载模块
//!
//! 设计特点:
//! - 两步下载：`getFile` 取文件路径（含体积预检）→ 按路径下载字节
//! - 下载入参即文件标识（事件解析层把图片段的下载引用填为文件标识）
//! - 上限与图片理解链路对齐（20MB）；先按 `getFile` 体积预检、读体后复检
//! - 失败分类与出站 API 同口径（超时 / 网络 / 应答分类）；错误文案不含令牌与文件路径

use std::time::Duration;

use async_trait::async_trait;

use super::api;
use crate::runtime::download::{ImageDownloader, MAX_IMAGE_BYTES};
use crate::{ChannelError, ChannelResult};

// =========================================================
// 常量
// =========================================================

/// 单次下载总超时（体积上限 20MB 下的保守取值）。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

// =========================================================
// 下载器
// =========================================================

/// Telegram 媒体下载器（`getFile` 两步）。
///
/// 职责:
/// - 实现 [`ImageDownloader`]：入参为平台文件标识（非 HTTP 地址）；
/// - 经 `getFile` 取得文件路径后下载字节，体积超限按协议错误拒绝。
///
/// 字段约定:
/// - `token`: Bot 令牌（仅随实例内存持有，不落日志）。
/// - `base_url`: Bot API 根地址。
/// - `request_timeout`: 单次 HTTP 请求超时。
/// - `max_bytes`: 单张体积上限（默认 [`MAX_IMAGE_BYTES`]）。
pub struct TelegramFileDownloader {
    client: reqwest::Client,
    token: String,
    base_url: String,
    request_timeout: Duration,
    max_bytes: u64,
}

impl TelegramFileDownloader {
    /// 以默认参数构造（总超时 30s、上限 20MB）。
    pub fn new(bot_token: impl Into<String>, api_base_url: impl Into<String>) -> Self {
        Self::with_options(bot_token, api_base_url, DOWNLOAD_TIMEOUT, MAX_IMAGE_BYTES)
    }

    /// 以指定超时与体积上限构造（上限 0 回落默认值）。
    pub fn with_options(
        bot_token: impl Into<String>,
        api_base_url: impl Into<String>,
        request_timeout: Duration,
        max_bytes: u64,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: bot_token.into(),
            base_url: api_base_url.into(),
            request_timeout,
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

#[async_trait]
impl ImageDownloader for TelegramFileDownloader {
    async fn download(&self, url: &str) -> ChannelResult<Vec<u8>> {
        let file_id = url.trim();
        if file_id.is_empty() {
            return Err(ChannelError::Protocol("文件标识为空".to_string()));
        }

        // ---- 第一步：getFile 取路径与体积 ----
        let info = api::get_file(
            &self.client,
            &self.token,
            &self.base_url,
            file_id,
            self.request_timeout,
        )
        .await?;
        if let Some(size) = info.file_size {
            if size > self.max_bytes {
                return Err(ChannelError::Protocol(format!(
                    "文件超过体积上限（{size} 字节）"
                )));
            }
        }

        // ---- 第二步：按路径下载字节（读体后复检体积） ----
        let response = self
            .client
            .get(api::download_url(
                &self.base_url,
                &self.token,
                &info.file_path,
            ))
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(api::classify_request_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ChannelError::Protocol(format!(
                "媒体下载失败（HTTP {}）",
                status.as_u16()
            )));
        }
        if let Some(length) = response.content_length() {
            if length > self.max_bytes {
                return Err(ChannelError::Protocol(format!(
                    "文件超过体积上限（{length} 字节）"
                )));
            }
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| ChannelError::Protocol("媒体下载读取失败".to_string()))?;
        if bytes.len() as u64 > self.max_bytes {
            return Err(ChannelError::Protocol(format!(
                "文件超过体积上限（{} 字节）",
                bytes.len()
            )));
        }
        Ok(bytes.to_vec())
    }
}
