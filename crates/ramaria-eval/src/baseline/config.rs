//! crates/ramaria-eval/src/baseline/config.rs - Ramaria 评测基线模型配置
//!
//! 设计特点:
//! - 配置固定来自当前工作目录的 `.eval/eval.toml`，不接受命令行后端参数
//! - 只读模型类别 / 地址 / 模型标识 / 向量模型目录四项，不读也不写 API key
//! - 本地模型允许空地址（回退产品默认地址），云端模型地址与模型标识必须齐全
//! - 校验失败文案带文件路径与字段名，便于定位缺失项
//! - 产出直接映射为沙盒后端选择，复用同一份解析与应用实现

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use serde::Deserialize;

use crate::sandbox::SandboxBackendChoice;
use crate::sandbox::backend::SandboxModelKind;

/// 评测配置文件（相对当前工作目录）。
pub const EVAL_CONFIG_RELATIVE_PATH: &str = ".eval/eval.toml";

/// 配置组名（TOML 表名）。
const SECTION: &str = "memory";

/// 评测建库使用的模型配置。
///
/// 字段约定:
/// - `kind`: 模型类别（本地 / 云端）；
/// - `base_url`: 模型服务地址，本地模型允许为空（回退产品默认）；
/// - `model`: 模型标识，两类都必须非空；
/// - `embedding_path`: 向量模型目录，必须非空；
/// - `source`: 实际读取的文件路径，只用于错误文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConfig {
    pub kind: SandboxModelKind,
    pub base_url: String,
    pub model: String,
    pub embedding_path: PathBuf,
    pub source: PathBuf,
}

impl MemoryConfig {
    /// 读取并校验评测配置。
    ///
    /// 参数:
    /// - `path`: 配置文件路径。
    ///
    /// 返回:
    /// - 校验通过的模型配置；
    /// - 文件缺失 / 解析失败 / 字段缺失或取值非法返回 `Validation`，读取失败返回 `Io`。
    pub fn load(path: &Path) -> RamariaResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RamariaError::validation(format!(
                    "评测配置缺失: {}（需要 [memory] 组: kind / base_url / model / embedding_path）",
                    path.display()
                ))
            } else {
                RamariaError::io(
                    format!("读取评测配置失败: {}", path.display()),
                    Some(error),
                )
            }
        })?;

        let raw: RawFile = toml::from_str(&text).map_err(|error| {
            RamariaError::validation(format!("评测配置解析失败: {}（{error}）", path.display()))
        })?;
        let memory = raw.memory.ok_or_else(|| {
            RamariaError::validation(format!(
                "评测配置缺少 [{SECTION}] 组（文件: {}）",
                path.display()
            ))
        })?;

        let kind_text = memory.kind.ok_or_else(|| missing(path, "kind"))?;
        let kind = parse_kind(&kind_text).ok_or_else(|| {
            RamariaError::validation(format!(
                "[memory].kind 取值非法: {kind_text}（可选: local / cloud；文件: {}）",
                path.display()
            ))
        })?;

        let model = required_text(path, "model", memory.model)?;
        let base_url = memory.base_url.trim().to_string();
        if kind == SandboxModelKind::Cloud && base_url.is_empty() {
            return Err(RamariaError::validation(format!(
                "[memory].base_url 在 kind = cloud 时必须非空（文件: {}）",
                path.display()
            )));
        }

        let embedding_path = required_text(path, "embedding_path", memory.embedding_path)?;

        Ok(Self {
            kind,
            base_url,
            model,
            embedding_path: PathBuf::from(embedding_path),
            source: path.to_path_buf(),
        })
    }

    /// 模型类别标识（`local` / `cloud`，产物字段使用）。
    pub fn kind_str(&self) -> &'static str {
        self.kind.as_str()
    }

    /// 面向用户的类别标签。
    pub fn kind_label(&self) -> &'static str {
        self.kind.label()
    }

    /// 构造独立后端选择（向量模型目录必带）。
    pub fn backend_choice(&self) -> SandboxBackendChoice {
        SandboxBackendChoice::Independent {
            kind: self.kind,
            base_url: self.base_url.clone(),
            model_id: self.model.clone(),
            embedding_model_path: Some(self.embedding_path.clone()),
        }
    }
}

// =========================================================
// 原始结构与校验
// =========================================================

#[derive(Debug, Deserialize)]
struct RawFile {
    memory: Option<RawMemory>,
}

#[derive(Debug, Deserialize)]
struct RawMemory {
    kind: Option<String>,
    #[serde(default)]
    base_url: String,
    model: Option<String>,
    embedding_path: Option<String>,
}

/// 解析模型类别（大小写不敏感）。
fn parse_kind(raw: &str) -> Option<SandboxModelKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "local" => Some(SandboxModelKind::Local),
        "cloud" => Some(SandboxModelKind::Cloud),
        _ => None,
    }
}

/// 必填文本字段：缺失、空白都按缺失处理。
fn required_text(path: &Path, field: &str, value: Option<String>) -> RamariaResult<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| missing(path, field))
}

/// 构造缺字段错误（文案带路径与字段名）。
fn missing(path: &Path, field: &str) -> RamariaError {
    RamariaError::validation(format!(
        "评测配置缺少 [memory].{field}（文件: {}）",
        path.display()
    ))
}

#[cfg(test)]
mod tests;
