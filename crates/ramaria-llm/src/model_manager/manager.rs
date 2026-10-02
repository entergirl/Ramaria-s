//! crates/ramaria-llm/src/model_manager/manager.rs - 模型管理器实现
//!
//! 设计特点:
//! - 模型目录生命周期：创建 / 就绪检查 / 已安装列表 / 删除 / 占用空间
//! - SHA-256 校验：流式分块读取（64KB），大文件不整体加载到内存
//! - 取消与进度状态通过原子量维护，供下载路径轮询
//! - 预置查询与下载基础 URL 构造（不在预置列表时返回 Config 错误）
//! - HTTP 客户端在构造时创建并可复用（连接池 + TLS 会话重用）

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ramaria_core::error::{RamariaError, RamariaResult};
use sha2::{Digest, Sha256};

use super::download::{DownloadProgress, build_http_client};
use super::fs::{dir_size, path_log_label};
use super::presets::{HF_DOMAIN, MODEL_PRESETS, ModelPreset, REQUIRED_FILES};

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
/// manager.download_model("bge-small-zh-v1.5", Some(progress_callback)).await?;
/// }
/// let model_path = manager.model_dir("bge-small-zh-v1.5");
/// ```
pub struct ModelManager {
    /// 模型根目录（如 %APPDATA%\Ramaria\models）
    models_root: PathBuf,
    /// 是否已取消当前下载
    pub(crate) cancelled: AtomicBool,
    /// 当前下载的已下载字节数
    pub(crate) downloaded: AtomicU64,
    /// 当前下载的总字节数
    pub(crate) total_size: AtomicU64,
    /// 可复用的 HTTP 客户端（连接池 + TLS 会话重用 + 超时配置）
    pub(crate) http_client: reqwest::Client,
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

        // 构建可复用的 HTTP 客户端（连接池、TLS 会话重用、超时配置）
        let http_client = build_http_client()?;

        tracing::info!(models_root = %path_log_label(&root), "ModelManager 已初始化");

        Ok(Self {
            models_root: root,
            cancelled: AtomicBool::new(false),
            downloaded: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
            http_client,
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
    /// - `true`: config.json, model.safetensors, tokenizer.json 均存在。
    pub fn is_model_ready(&self, model_id: &str) -> bool {
        let dir = self.model_dir(model_id);
        let all_ok = REQUIRED_FILES.iter().all(|f| dir.join(f).exists());

        tracing::debug!(
            model_id,
            ready = all_ok,
            model_dir = %path_log_label(&dir),
            "模型就绪检查"
        );

        all_ok
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
            if REQUIRED_FILES.iter().all(|f| dir.join(f).exists())
                && let Some(name) = dir.file_name().and_then(|n| n.to_str())
            {
                models.push(name.to_string());
            }
        }

        tracing::debug!(count = models.len(), "已发现已安装模型");
        Ok(models)
    }

    /// 获取模型预置信息。
    ///
    /// 参数:
    /// - `model_id`: 模型标识。
    ///
    /// 返回:
    /// - `Some(&ModelPreset)`: 预置模型信息。
    /// - `None`: 不在预置列表中。
    pub fn get_preset(model_id: &str) -> Option<&'static ModelPreset> {
        MODEL_PRESETS.iter().find(|p| p.model_id == model_id)
    }

    /// 获取模型的 HuggingFace 下载基础 URL。
    ///
    /// 参数:
    /// - `model_id`: 模型标识。
    ///
    /// 返回:
    /// - 下载基础 URL（如 `https://huggingface.co/BAAI/bge-small-zh-v1.5/resolve/main`）。
    ///   如果不在预置列表中，返回错误。
    pub fn download_base_url(model_id: &str) -> RamariaResult<String> {
        let preset = Self::get_preset(model_id).ok_or_else(|| {
            RamariaError::config(format!(
                "不支持的模型标识: {}。支持列表: {:?}",
                model_id,
                MODEL_PRESETS.iter().map(|p| p.model_id).collect::<Vec<_>>()
            ))
        })?;

        Ok(format!("{}/{}/resolve/main", HF_DOMAIN, preset.hf_repo))
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

    /// 验证文件的 SHA-256 校验和（流式读取，支持大文件）。
    ///
    /// 参数:
    /// - `path`: 文件路径。
    /// - `expected_hex`: 预期的十六进制 SHA-256 字符串。
    ///
    /// 返回:
    /// - `Ok(true)`: 校验通过。
    /// - `Ok(false)`: 校验不匹配。
    ///
    /// 说明:
    /// - 使用 `BufReader` 分块读取，每块 64KB，避免将整个文件加载到内存。
    /// - 对于 Qwen3-Embedding-0.6B（~1.2GB）等大文件安全无害。
    pub fn verify_checksum(&self, path: &Path, expected_hex: &str) -> RamariaResult<bool> {
        use std::io::Read;

        let file = fs::File::open(path).map_err(|e| {
            RamariaError::io(format!("无法打开文件以校验: {}", path.display()), Some(e))
        })?;

        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];

        loop {
            let bytes_read = reader.read(&mut buf).map_err(|e| {
                RamariaError::io(format!("校验文件时读取失败: {}", path.display()), Some(e))
            })?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buf[..bytes_read]);
        }

        let hash = hasher.finalize();
        let hash_hex = format!("{:x}", hash);

        let matches = hash_hex.eq_ignore_ascii_case(expected_hex);
        if !matches {
            tracing::warn!(
                file = %path_log_label(path),
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
            tracing::info!(model_id, model_dir = %path_log_label(&dir), "模型已删除");
        }
        Ok(())
    }

    /// 获取模型目录占用的磁盘空间（字节）。
    pub fn model_size(&self, model_id: &str) -> u64 {
        let dir = self.model_dir(model_id);
        dir_size(&dir)
    }
}
