//! crates/ramaria-llm/src/model_manager/download.rs - 模型文件下载与断点续传
//!
//! 设计特点:
//! - `DownloadProgress` / `ProgressCallback`: 下载进度结构体与回调类型
//! - 流式下载（reqwest bytes_stream），写入 `.part` 临时文件后原子重命名
//! - 断点续传：仅服务器返回 206 且 Content-Range 起始偏移与本地进度一致时追加
//! - 服务器忽略 Range 返回 200 时截断重写，避免把完整内容追加到旧数据尾部
//! - HTTP 客户端统一超时（连接 30s / 请求 3600s），供多文件下载复用

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::StreamExt;
use ramaria_core::error::{RamariaError, RamariaResult};

use super::fs::path_log_label;
use super::manager::ModelManager;
use super::presets::{HF_DOMAIN, MODEL_PRESETS, REQUIRED_FILES, TEMP_SUFFIX};

// =========================================================
// 下载进度
// =========================================================

/// 下载进度信息。
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    /// 已下载字节数
    pub downloaded_bytes: u64,
    /// 总字节数（未知时为 0）
    pub total_bytes: u64,
    /// 当前正在下载的文件名
    pub current_file: String,
    /// 进度百分比 0.0..1.0
    pub progress: f64,
}

/// 下载进度回调类型。
pub type ProgressCallback = Arc<dyn Fn(DownloadProgress) + Send + Sync>;

/// HTTP 客户端默认连接超时（秒）。
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
/// HTTP 客户端默认请求总超时（秒）。
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 3600; // 1 小时，适应大型模型文件下载

/// 创建一个配置了合理超时的 reqwest Client。
///
/// - 连接超时: `connect_timeout(30s)`——建立 TCP/TLS 连接的超时
/// - 请求超时: `timeout(3600s)`——整体请求的超时（含下载）
///
/// 说明: timeout 覆盖 download_single_file 中的流式下载，
/// 确保网络卡住时不会永久挂起。3600 秒足够下载 1.2GB 文件（~350KB/s）。
pub(crate) fn build_http_client() -> RamariaResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
        .user_agent(format!("Ramaria/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| RamariaError::validation(format!("创建 HTTP 客户端失败: {e}")))
}

// =========================================================
// 断点续传辅助函数
// =========================================================

/// 解析 `Content-Range` 响应头中的起始偏移。
///
/// 参数:
/// - `header`: `Content-Range` 头的原始字符串，格式为 `bytes {start}-{end}/{total}`。
///
/// 返回:
/// - `Some(start)`: 起始偏移解析成功。
/// - `None`: 头缺失、缺少 `bytes` 前缀、缺少 `-` 分隔或起始偏移不是合法数字。
pub(crate) fn parse_content_range_start(header: &str) -> Option<u64> {
    let rest = header.trim().strip_prefix("bytes ")?;
    let (start, _) = rest.split_once('-')?;
    start.trim().parse::<u64>().ok()
}

/// 判断是否允许在已有临时文件基础上追加续传。
///
/// 参数:
/// - `existing_size`: 本地临时文件已下载的字节数。
/// - `status_code`: 服务器响应状态码。
///
/// 返回:
/// - 仅当 `existing_size > 0` 且服务器返回 `206 Partial Content` 时为 `true`。
///
/// 说明:
/// - 服务器忽略 `Range` 请求返回 200 时，响应体是完整文件内容；
///   此时若继续追加写入会损坏文件，必须从头重写。
pub(crate) fn resume_append_allowed(existing_size: u64, status_code: u16) -> bool {
    existing_size > 0 && status_code == 206
}

// =========================================================
// 下载实现
// =========================================================

impl ModelManager {
    /// 下载模型文件。
    ///
    /// 参数:
    /// - `model_id`: 模型标识（如 "bge-small-zh-v1.5"）。
    /// - `progress_callback`: 可选的进度回调（每下载一个 chunk 触发一次）。
    ///
    /// 返回:
    /// - `Ok()`: 下载完成。
    ///
    /// 说明:
    /// - 支持断点续传：如果 .part 临时文件存在，从已下载位置继续。
    /// - 每个文件下载完成后做 SHA-256 校验（若提供了校验和）。
    /// - 全部文件下载完成后原子地将临时文件重命名为正式文件。
    /// - 可通过 `cancel_download` 取消进行中的下载。
    /// - 下载 URL 格式: `https://huggingface.co/{org}/{repo}/resolve/main/{file}`
    ///
    /// 错误场景:
    /// - 网络不可达。
    /// - 服务器返回非 200。
    /// - SHA-256 校验失败。
    /// - 磁盘写入失败。
    /// - model_id 不在预置列表中。
    pub async fn download_model(
        &self,
        model_id: &str,
        progress_callback: Option<ProgressCallback>,
    ) -> RamariaResult<()> {
        // 获取模型预置信息
        let preset = Self::get_preset(model_id).ok_or_else(|| {
            RamariaError::config(format!(
                "不支持的模型: {}。可用模型: {:?}",
                model_id,
                MODEL_PRESETS.iter().map(|p| p.model_id).collect::<Vec<_>>()
            ))
        })?;

        // 重置状态
        self.cancelled.store(false, Ordering::SeqCst);
        self.downloaded.store(0, Ordering::SeqCst);
        self.total_size.store(0, Ordering::SeqCst);

        let dir = self.model_dir(model_id);

        // 确保模型目录存在
        fs::create_dir_all(&dir).map_err(|e| {
            RamariaError::io(format!("无法创建模型子目录: {}", dir.display()), Some(e))
        })?;

        tracing::info!(
            model_id,
            model_dir = %path_log_label(&dir),
            repo = preset.hf_repo,
            file_count = preset.files.len(),
            estimated_size_mb = preset.estimated_size / 1_000_000,
            "开始下载嵌入模型"
        );

        // 构建下载 base URL
        let base_url = std::env::var("RAMARIA_MODEL_DOWNLOAD_URL")
            .unwrap_or_else(|_| format!("{}/{}/resolve/main", HF_DOMAIN, preset.hf_repo));

        // 下载每个文件
        for (filename, expected_sha256) in preset.files {
            if self.cancelled.load(Ordering::SeqCst) {
                tracing::info!("下载已被取消");
                return Err(RamariaError::validation("模型下载已取消"));
            }

            let url = format!("{}/{}", base_url, filename);
            let dest_path = dir.join(filename);

            // 临时文件名 = 原文件名 + ".part"
            let temp_name = format!(
                "{}{}",
                dest_path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_else(|| std::borrow::Cow::Borrowed(filename)),
                TEMP_SUFFIX
            );
            let temp_path = dest_path.with_file_name(temp_name);

            // 如果正式文件已存在且校验通过，跳过
            if dest_path.exists() && (!expected_sha256.is_empty()) {
                if self.verify_checksum(&dest_path, expected_sha256)? {
                    tracing::debug!(file = %filename, "文件已存在且校验通过，跳过下载");
                    continue;
                }
                tracing::warn!(file = %filename, "文件校验失败，重新下载");
            } else if dest_path.exists() {
                tracing::debug!(file = %filename, "文件已存在，跳过下载（无校验和）");
                continue;
            }

            tracing::info!(file = %filename, url = %url, "下载文件");

            // 记录当前文件信息
            if let Some(ref cb) = progress_callback {
                cb(DownloadProgress {
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    current_file: filename.to_string(),
                    progress: 0.0,
                });
            }

            // 下载文件（支持断点续传）
            self.download_single_file(&url, &temp_path, filename, progress_callback.as_ref())
                .await?;

            // SHA-256 校验
            if !expected_sha256.is_empty() {
                tracing::debug!(file = %filename, "校验 SHA-256...");
                if !self.verify_checksum(&temp_path, expected_sha256)? {
                    // 校验失败，删除临时文件
                    let _ = fs::remove_file(&temp_path);
                    return Err(RamariaError::validation(format!(
                        "文件 {} SHA-256 校验失败。预期: {}",
                        filename, expected_sha256
                    )));
                }
                tracing::info!(file = %filename, "SHA-256 校验通过");
            }

            // 原子重命名：临时文件 → 正式文件
            fs::rename(&temp_path, &dest_path).map_err(|e| {
                RamariaError::io(
                    format!(
                        "文件重命名失败: {} → {}",
                        temp_path.display(),
                        dest_path.display()
                    ),
                    Some(e),
                )
            })?;

            tracing::info!(file = %filename, path = %path_log_label(&dest_path), "文件下载完成");
        }

        // 验证模型完整性
        if !self.is_model_ready(model_id) {
            let missing: Vec<&str> = REQUIRED_FILES
                .iter()
                .filter(|f| !dir.join(f).exists())
                .copied()
                .collect();
            return Err(RamariaError::validation(format!(
                "模型 {} 下载后仍不完整。缺失文件: {:?}",
                model_id, missing
            )));
        }

        tracing::info!(model_id, "模型下载全部完成");
        Ok(())
    }

    /// 下载单个文件（支持断点续传）。
    ///
    /// 使用 `self.http_client`（在 `ModelManager::new` 中创建的可复用实例），
    /// 而非每次调用创建新 Client。好处:
    /// - 连接池复用，减少 TCP/TLS 握手开销（尤其是多文件下载时）
    /// - 超时设置在构造时统一配置（见 `build_http_client`）:
    ///   `connect_timeout(30s)` 约束建立连接，`timeout(3600s)` 覆盖整个请求（含流式 body）
    /// - 请求处无需额外包裹 `tokio::time::timeout`，客户端级超时已保证网络卡住时不会永久挂起
    ///
    /// 说明:
    /// - 仅当服务器返回 206 且 `Content-Range` 起始偏移等于本地临时文件大小时追加续传；
    ///   其余情况一律截断重写，避免服务器忽略 Range 返回 200 时把完整内容追加到旧数据尾部
    async fn download_single_file(
        &self,
        url: &str,
        dest: &Path,
        filename: &str,
        cb: Option<&ProgressCallback>,
    ) -> RamariaResult<()> {
        // 检查是否有断点续传的临时文件
        let existing_size = if dest.exists() {
            fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        // 构建请求（支持 Range 头用于断点续传）
        let mut request = self.http_client.get(url);
        if existing_size > 0 {
            request = request.header("Range", format!("bytes={}-", existing_size));
            tracing::debug!(file = %filename, existing_bytes = existing_size, "断点续传");
        }

        let response = request
            .send()
            .await
            .map_err(|e| RamariaError::validation(format!("下载请求失败: {} — URL: {}", e, url)))?;

        let status = response.status();
        if status != 200 && status != 206 {
            return Err(RamariaError::validation(format!(
                "下载失败: HTTP {} — URL: {}",
                status, url
            )));
        }

        let status_code = status.as_u16();

        // 服务器忽略 Range 返回 200 时，响应体是完整文件内容；
        // 此时追加写入会损坏文件，必须放弃续传并从头重写
        let append_existing = resume_append_allowed(existing_size, status_code);
        if existing_size > 0 && !append_existing {
            tracing::warn!(
                file = %filename,
                status = status_code,
                existing_bytes = existing_size,
                "服务器未返回 206 Partial Content，放弃断点续传并从头下载"
            );
        }

        // 获取总大小
        let total = if status == 206 {
            // 部分内容：从 Content-Range 头获取总大小
            response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split('/').next_back())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        } else {
            response.content_length().unwrap_or(0)
        };

        // 允许追加时，服务器返回的起始偏移必须与本地进度一致，否则临时文件不可信
        if append_existing {
            let range_start = response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range_start);

            if range_start != Some(existing_size) {
                if let Err(e) = fs::remove_file(dest) {
                    tracing::warn!(
                        file = %filename,
                        error = %e,
                        "清理断点续传临时文件失败"
                    );
                }
                return Err(RamariaError::validation(format!(
                    "文件 {} 断点续传范围不匹配，已清理临时文件，请重试（临时文件: {}）",
                    filename,
                    dest.display()
                )));
            }
        }

        self.total_size.store(total, Ordering::SeqCst);
        let mut downloaded = if append_existing { existing_size } else { 0 };
        self.downloaded.store(downloaded, Ordering::SeqCst);

        // 打开文件：允许续传时追加写，否则截断重写
        let mut file = if append_existing {
            std::fs::OpenOptions::new()
                .append(true)
                .open(dest)
                .map_err(|e| {
                    RamariaError::io(format!("无法打开文件: {}", dest.display()), Some(e))
                })?
        } else {
            std::fs::File::create(dest).map_err(|e| {
                RamariaError::io(format!("无法创建文件: {}", dest.display()), Some(e))
            })?
        };

        // 流式下载
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            if self.cancelled.load(Ordering::SeqCst) {
                tracing::info!("下载已取消");
                return Err(RamariaError::validation("模型下载已取消"));
            }

            let chunk =
                chunk.map_err(|e| RamariaError::validation(format!("下载数据块失败: {}", e)))?;

            file.write_all(&chunk).map_err(|e| {
                RamariaError::io(format!("写入文件失败: {}", dest.display()), Some(e))
            })?;

            downloaded += chunk.len() as u64;
            self.downloaded.store(downloaded, Ordering::SeqCst);

            // 进度回调
            if let Some(cb) = cb {
                let progress = if total > 0 {
                    downloaded as f64 / total as f64
                } else {
                    0.0
                };
                cb(DownloadProgress {
                    downloaded_bytes: downloaded,
                    total_bytes: total,
                    current_file: filename.to_string(),
                    progress,
                });
            }
        }

        file.flush().map_err(|e| {
            RamariaError::io(format!("刷新文件缓冲区失败: {}", dest.display()), Some(e))
        })?;

        tracing::info!(
            file = %filename,
            bytes = downloaded,
            "文件下载完成"
        );

        Ok(())
    }
}
