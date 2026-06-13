//! rust/crates/ramaria-app/src/model_manager.rs - 嵌入模型下载与校验管理
//!
//! 设计特点:
//! - 管理嵌入模型的下载、SHA-256 校验、断点续传和目录管理
//! - 下载进度通过回调函数实时推送，供前端进度条展示
//! - 支持用户自行放置模型文件（跳过下载），自动检测 model.onnx + tokenizer.json
//! - 下载使用 reqwest 流式传输，写入临时文件后原子重命名
//! - 所有 I/O 错误有明确日志，包含文件路径和具体原因
//!
//! 模型目录约定:
//! - Windows: `%APPDATA%\Ramaria\models\bge-small-zh-v1.5\`
//! - 开发模式: 通过 `RAMARIA_DATA_DIR` 覆盖
//! - 模型文件: model.onnx, tokenizer.json
//!
//! 下载源:
//! - 默认从 HuggingFace 镜像下载（可通过配置覆盖）
//! - 下载 URL 格式: {base_url}/{model_name}/resolve/main/{file}

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ramaria_core::error::{RamariaError, RamariaResult};
use sha2::{Digest, Sha256};

// =========================================================
// 常量
// =========================================================

/// 默认嵌入模型标识
pub const DEFAULT_MODEL_ID: &str = "bge-small-zh-v1.5";

/// 默认模型下载基础 URL（HuggingFace 镜像）
const DEFAULT_DOWNLOAD_BASE: &str = "https://huggingface.co/BAAI/bge-small-zh-v1.5/resolve/main";

/// 预期模型文件名及其 SHA-256 校验和
/// 校验和为空表示跳过校验
const MODEL_FILES: &[(&str, &str)] = &[
    ("model.onnx", ""),     // ONNX 模型文件（SHA-256 需用户提供）
    ("tokenizer.json", ""), // 分词器配置文件
];

/// 下载缓冲区大小（64KB）
const _DOWNLOAD_BUF_SIZE: usize = 64 * 1024;

/// 下载临时文件后缀
const TEMP_SUFFIX: &str = ".part";

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

// =========================================================
// ModelManager
// =========================================================

/// 嵌入模型管理器。
///
/// 职责:
/// - 管理模型目录：创建、检查文件完整性
/// - 下载模型文件：支持进度回调、断点续传、SHA-256 校验
/// - 发现已安装模型：扫描模型目录查找可用的 ONNX 模型
///
/// 用法:
/// ```ignore
/// let manager = ModelManager::new(models_dir)?;
/// if !manager.is_model_ready("bge-small-zh-v1.5") {
///     manager.download_model("bge-small-zh-v1.5", Some(progress_callback)).await?;
/// }
/// let model_path = manager.model_dir("bge-small-zh-v1.5");
/// ```
pub struct ModelManager {
    /// 模型根目录（如 %APPDATA%\Ramaria\models）
    models_root: PathBuf,
    /// 是否已取消当前下载
    cancelled: AtomicBool,
    /// 当前下载的已下载字节数
    downloaded: AtomicU64,
    /// 当前下载的总字节数
    total_size: AtomicU64,
}

impl ModelManager {
    /// 创建新的模型管理器。
    ///
    /// 参数:
    /// - `models_root`: 模型根目录路径。
    ///
    /// 返回:
    /// - 成功时返回 ModelManager 实例。
    ///
    /// 说明:
    /// - 如果目录不存在，会自动创建。
    /// - 创建失败时返回 Io 错误。
    pub fn new(models_root: impl Into<PathBuf>) -> RamariaResult<Self> {
        let root: PathBuf = models_root.into();

        // 确保目录存在
        fs::create_dir_all(&root).map_err(|e| {
            RamariaError::io(format!("无法创建模型目录: {}", root.display()), Some(e))
        })?;

        tracing::info!(models_root = %root.display(), "ModelManager 已初始化");

        Ok(Self {
            models_root: root,
            cancelled: AtomicBool::new(false),
            downloaded: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
        })
    }

    /// 获取指定模型的目录路径。
    ///
    /// 参数:
    /// - `model_id`: 模型标识（如 "bge-small-zh-v1.5"）。
    pub fn model_dir(&self, model_id: &str) -> PathBuf {
        self.models_root.join(model_id)
    }

    /// 检查模型是否已就绪（所有必需文件存在）。
    ///
    /// 参数:
    /// - `model_id`: 模型标识。
    ///
    /// 返回:
    /// - `true`: model.onnx 和 tokenizer.json 均存在。
    pub fn is_model_ready(&self, model_id: &str) -> bool {
        let dir = self.model_dir(model_id);
        let model_ok = dir.join("model.onnx").exists();
        let tokenizer_ok = dir.join("tokenizer.json").exists();

        tracing::debug!(
            model_id,
            model_ok,
            tokenizer_ok,
            model_dir = %dir.display(),
            "模型就绪检查"
        );

        model_ok && tokenizer_ok
    }

    /// 列出所有已安装的模型。
    ///
    /// 返回:
    /// - 模型 ID 列表（目录名）。
    pub fn list_installed_models(&self) -> RamariaResult<Vec<String>> {
        let mut models = Vec::new();

        if !self.models_root.exists() {
            return Ok(models);
        }

        let entries = fs::read_dir(&self.models_root).map_err(|e| {
            RamariaError::io(
                format!("无法读取模型目录: {}", self.models_root.display()),
                Some(e),
            )
        })?;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            let dir = entry.path();
            if dir.join("model.onnx").exists()
                && dir.join("tokenizer.json").exists()
                && let Some(name) = dir.file_name().and_then(|n| n.to_str())
            {
                models.push(name.to_string());
            }
        }

        tracing::debug!(count = models.len(), "已发现已安装模型");
        Ok(models)
    }

    /// 下载模型文件。
    ///
    /// 参数:
    /// - `model_id`: 模型标识。
    /// - `progress_callback`: 可选的进度回调（每下载一个 chunk 触发一次）。
    ///
    /// 返回:
    /// - `Ok(())`: 下载完成。
    ///
    /// 说明:
    /// - 支持断点续传：如果 .part 临时文件存在，从已下载位置继续。
    /// - 每个文件下载完成后做 SHA-256 校验（若提供了校验和）。
    /// - 全部文件下载完成后原子地将临时文件重命名为正式文件。
    /// - 可通过 `cancel_download()` 取消进行中的下载。
    ///
    /// 错误场景:
    /// - 网络不可达。
    /// - 服务器返回非 200。
    /// - SHA-256 校验失败。
    /// - 磁盘写入失败。
    pub async fn download_model(
        &self,
        model_id: &str,
        progress_callback: Option<ProgressCallback>,
    ) -> RamariaResult<()> {
        // 重置状态
        self.cancelled.store(false, Ordering::SeqCst);
        self.downloaded.store(0, Ordering::SeqCst);
        self.total_size.store(0, Ordering::SeqCst);

        let dir = self.model_dir(model_id);

        // 确保模型目录存在
        fs::create_dir_all(&dir).map_err(|e| {
            RamariaError::io(format!("无法创建模型子目录: {}", dir.display()), Some(e))
        })?;

        tracing::info!(model_id, model_dir = %dir.display(), "开始下载嵌入模型");

        let base_url = std::env::var("RAMARIA_MODEL_DOWNLOAD_URL")
            .unwrap_or_else(|_| DEFAULT_DOWNLOAD_BASE.to_string());

        // 下载每个文件
        for (filename, expected_sha256) in MODEL_FILES {
            if self.cancelled.load(Ordering::SeqCst) {
                tracing::info!("下载已被取消");
                return Err(RamariaError::validation("模型下载已取消"));
            }

            let url = format!("{}/{}", base_url, filename);
            let dest_path = dir.join(filename);
            let temp_path = dest_path.with_extension(
                dest_path
                    .extension()
                    .map(|e| {
                        format!(
                            "{}{}",
                            e.to_string_lossy(),
                            TEMP_SUFFIX.trim_start_matches('.')
                        )
                    })
                    .unwrap_or_else(|| TEMP_SUFFIX.trim_start_matches('.').to_string()),
            );

            // 如果正式文件已存在且校验通过，跳过
            if dest_path.exists() && (!expected_sha256.is_empty()) {
                if self.verify_checksum(&dest_path, expected_sha256)? {
                    tracing::debug!(file = %filename, "文件已存在且校验通过，跳过下载");
                    continue;
                }
                tracing::warn!(file = %filename, "文件校验失败，重新下载");
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

            tracing::info!(file = %filename, path = %dest_path.display(), "文件下载完成");
        }

        // 验证模型完整性
        if !self.is_model_ready(model_id) {
            return Err(RamariaError::validation(format!(
                "模型 {} 下载后仍不完整（缺少必需文件）",
                model_id
            )));
        }

        tracing::info!(model_id, "模型下载全部完成");
        Ok(())
    }

    /// 取消当前下载。
    pub fn cancel_download(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        tracing::info!("模型下载取消请求已设置");
    }

    /// 获取当前下载进度。
    pub fn current_progress(&self) -> DownloadProgress {
        let downloaded = self.downloaded.load(Ordering::SeqCst);
        let total = self.total_size.load(Ordering::SeqCst);
        let progress = if total > 0 {
            downloaded as f64 / total as f64
        } else {
            0.0
        };

        DownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: total,
            current_file: String::new(),
            progress,
        }
    }

    /// 验证文件的 SHA-256 校验和。
    ///
    /// 参数:
    /// - `path`: 文件路径。
    /// - `expected_hex`: 预期的十六进制 SHA-256 字符串。
    ///
    /// 返回:
    /// - `Ok(true)`: 校验通过。
    /// - `Ok(false)`: 校验不匹配。
    pub fn verify_checksum(&self, path: &Path, expected_hex: &str) -> RamariaResult<bool> {
        let data = fs::read(path).map_err(|e| {
            RamariaError::io(format!("无法读取文件以校验: {}", path.display()), Some(e))
        })?;

        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = hasher.finalize();
        let hash_hex = format!("{:x}", hash);

        let matches = hash_hex.eq_ignore_ascii_case(expected_hex);
        if !matches {
            tracing::warn!(
                file = %path.display(),
                expected = %expected_hex,
                actual = %hash_hex,
                "SHA-256 校验不匹配"
            );
        }

        Ok(matches)
    }

    /// 删除指定模型的所有文件。
    ///
    /// 参数:
    /// - `model_id`: 模型标识。
    pub fn remove_model(&self, model_id: &str) -> RamariaResult<()> {
        let dir = self.model_dir(model_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| {
                RamariaError::io(format!("无法删除模型目录: {}", dir.display()), Some(e))
            })?;
            tracing::info!(model_id, model_dir = %dir.display(), "模型已删除");
        }
        Ok(())
    }

    /// 获取模型目录占用的磁盘空间（字节）。
    pub fn model_size(&self, model_id: &str) -> u64 {
        let dir = self.model_dir(model_id);
        dir_size(&dir)
    }

    // ---- 内部方法 ----

    /// 下载单个文件（支持断点续传）。
    async fn download_single_file(
        &self,
        url: &str,
        dest: &Path,
        filename: &str,
        cb: Option<&ProgressCallback>,
    ) -> RamariaResult<()> {
        let client = reqwest::Client::new();

        // 检查是否有断点续传的临时文件
        let existing_size = if dest.exists() {
            fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        // 构建请求（支持 Range 头用于断点续传）
        let mut request = client.get(url);
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

        self.total_size.store(total, Ordering::SeqCst);
        let mut downloaded = existing_size;
        self.downloaded.store(downloaded, Ordering::SeqCst);

        // 打开文件（追加模式用于断点续传）
        let mut file = if existing_size > 0 {
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
        use futures::StreamExt;

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

// =========================================================
// 工具函数
// =========================================================

/// 递归计算目录大小。
fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }

    if path.is_file() {
        return fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }

    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            total += dir_size(&entry.path());
        }
    }
    total
}

// =========================================================
// 便捷工厂
// =========================================================

/// 获取默认的模型根目录。
///
/// Windows: `%APPDATA%\Ramaria\models`
/// 可通过 `RAMARIA_DATA_DIR` 环境变量覆盖。
pub fn default_models_root() -> PathBuf {
    if let Ok(dir) = std::env::var("RAMARIA_DATA_DIR") {
        return PathBuf::from(dir).join("models");
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(appdata).join("Ramaria").join("models")
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ramaria").join("models")
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 ModelManager 创建
    #[test]
    fn model_manager_creation() {
        let tmp = std::env::temp_dir().join("ramaria_test_models");
        let _ = fs::remove_dir_all(&tmp);

        let _mgr = ModelManager::new(&tmp).unwrap();
        assert!(tmp.exists());

        // 清理
        let _ = fs::remove_dir_all(&tmp);
    }

    /// 测试模型就绪检查
    #[test]
    fn is_model_ready_returns_false_for_missing_model() {
        let tmp = std::env::temp_dir().join("ramaria_test_models_empty");
        let _ = fs::remove_dir_all(&tmp);

        let mgr = ModelManager::new(&tmp).unwrap();
        assert!(!mgr.is_model_ready("nonexistent"));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 测试列出已安装模型（空目录）
    #[test]
    fn list_installed_models_empty() {
        let tmp = std::env::temp_dir().join("ramaria_test_models_list");
        let _ = fs::remove_dir_all(&tmp);

        let mgr = ModelManager::new(&tmp).unwrap();
        let models = mgr.list_installed_models().unwrap();
        assert!(models.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 测试 SHA-256 校验
    #[test]
    fn verify_checksum_matches() {
        let tmp = std::env::temp_dir().join("ramaria_test_checksum");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let test_file = tmp.join("test.txt");
        fs::write(&test_file, b"hello world").unwrap();

        let mgr = ModelManager::new(&tmp).unwrap();

        // SHA-256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(mgr.verify_checksum(&test_file, expected).unwrap());

        // Wrong hash
        assert!(!mgr.verify_checksum(&test_file, "deadbeef").unwrap());

        let _ = fs::remove_dir_all(&tmp);
    }
}
