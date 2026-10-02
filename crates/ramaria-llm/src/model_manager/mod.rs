//! crates/ramaria-llm/src/model_manager/mod.rs - Ramaria 嵌入模型下载与校验管理模块
//!
//! 设计特点:
//! - 管理嵌入模型的下载、SHA-256 校验、断点续传和目录管理
//! - 支持 BERT 架构（bge-small-zh-v1.5）和 LLaMA/Qwen3 架构（Qwen3-Embedding-0.6B）
//! - 下载进度通过回调函数实时推送；下载写入临时文件后原子重命名
//! - 支持用户自行放置模型文件（跳过下载），自动检测必需文件是否齐全
//! - 所有 I/O 错误有明确日志，包含文件路径和具体原因
//!
//! 模型目录约定:
//! - Windows: `%APPDATA%\Ramaria\models\{model_id}\`
//! - 开发模式: 通过 `RAMARIA_DATA_DIR` 覆盖
//!
//! 下载源:
//! - 默认从 HuggingFace 下载（可通过 `RAMARIA_MODEL_DOWNLOAD_URL` 覆盖）
//! - 下载 URL 格式: {base_url}/resolve/main/{file}

mod download;
mod fs;
mod manager;
mod presets;

#[cfg(test)]
mod tests;

pub use download::{DownloadProgress, ProgressCallback};
pub use fs::default_models_root;
pub use manager::ModelManager;
pub use presets::{DEFAULT_MODEL_ID, MODEL_PRESETS, ModelPreset};

#[cfg(test)]
pub(crate) use download::{parse_content_range_start, resume_append_allowed};
