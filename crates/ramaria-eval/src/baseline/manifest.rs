//! crates/ramaria-eval/src/baseline/manifest.rs - Ramaria 评测基线清单
//!
//! 设计特点:
//! - 清单只记录可比口径：语料指纹、档位窗口、流水线版本、模型与计数
//! - 不写绝对路径、不写服务地址、不写密钥；产物目录由调用方另行输出
//! - 代码指纹由四段版本口径拼接后取 SHA-256，任一段变化都可追溯
//! - 写出先落临时文件再改名，避免中断留下半份 JSON
//! - 清单为纯数据，不读取引擎与文件系统

use std::path::Path;

use ramaria_core::error::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 清单文件名。
pub const MANIFEST_FILE: &str = "manifest.json";

/// 标记产物类型的固定字符串。
pub const ARTIFACT_KIND: &str = "ramaria_eval_baseline";

/// 清单自身的 schema 版本。
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;

/// 基线清单。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物类型与清单版本；
/// - `baseline_id`: `<corpus>-<version>-<language>-<tier>`；
/// - `created_at`: UTC 时间（秒级，Z 结尾）；
/// - `warnings`: 不阻断构建的降级与数据口径提示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineManifest {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub baseline_id: String,
    pub created_at: String,
    pub corpus: ManifestCorpus,
    pub tier: ManifestTier,
    pub qa: ManifestQa,
    pub pipeline: ManifestPipeline,
    pub memory_model: ManifestMemoryModel,
    pub embedding: ManifestEmbedding,
    pub index: ManifestIndex,
    pub counts: Counts,
    pub timing: ManifestTiming,
    pub warnings: Vec<String>,
}

/// 语料来源口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestCorpus {
    pub name: String,
    pub version: String,
    pub language: String,
    pub file_name: String,
    pub sha256: String,
    pub size: u64,
    pub qa_total: usize,
    pub sample_ids: Vec<String>,
}

/// 档位口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestTier {
    pub id: String,
    pub users: usize,
    pub memory_start: String,
    pub memory_end: String,
    pub query_end: String,
    pub span_days: i64,
    pub sample_ids: Vec<String>,
}

/// 题集口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestQa {
    pub total: usize,
    pub in_window: usize,
}

/// 流水线口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestPipeline {
    pub crate_version: String,
    pub schema_version: i32,
    pub prompt_template_version: String,
    pub pipeline_version: String,
    pub code_fingerprint: String,
}

/// 生成模型口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestMemoryModel {
    pub kind: String,
    pub model: String,
    pub temperature: f64,
    pub max_tokens: u32,
}

/// 向量模型口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEmbedding {
    pub fingerprint: String,
    pub dimension: Option<usize>,
}

/// 检索索引口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestIndex {
    pub status: String,
    pub loaded: usize,
    pub total: usize,
}

/// 建库计数。
///
/// 字段约定:
/// - `personas`: 库内实际人格行数（检查点可能多于活跃用户数）；
/// - `l1`: 各会话 L1 摘要条数之和；
/// - `unabsorbed_l1`: 尚未被 L2 消费的 L1 条数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub personas: usize,
    pub sessions: usize,
    pub messages: usize,
    pub l1: usize,
    pub l2: usize,
    pub l3: usize,
    pub unabsorbed_l1: usize,
}

/// 构建耗时。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTiming {
    pub build_elapsed_ms: u64,
}

/// 代码指纹：四段版本口径按固定顺序拼接后取 SHA-256。
pub fn code_fingerprint(
    crate_version: &str,
    schema_version: i32,
    prompt_template_version: &str,
    pipeline_version: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        format!("{crate_version}|{schema_version}|{prompt_template_version}|{pipeline_version}")
            .as_bytes(),
    );
    format!("{:x}", hasher.finalize())
}

/// 写出清单（pretty JSON；先写临时文件再改名）。
///
/// 参数:
/// - `dir`: 档位产物目录；
/// - `manifest`: 清单数据。
pub fn write(dir: &Path, manifest: &BaselineManifest) -> RamariaResult<()> {
    let text =
        serde_json::to_string_pretty(manifest).map_err(|error| RamariaError::Serialization {
            context: format!("序列化基线清单失败: {}", manifest.baseline_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;

    let path = dir.join(MANIFEST_FILE);
    let temp = dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&temp, text).map_err(|error| {
        RamariaError::io(
            format!("写入基线清单临时文件失败: {}", temp.display()),
            Some(error),
        )
    })?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        RamariaError::io(format!("替换基线清单失败: {}", path.display()), Some(error))
    })
}

#[cfg(test)]
mod tests;
