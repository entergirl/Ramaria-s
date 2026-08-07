//! rust/crates/ramaria-llm/src/embedding/models/mod.rs - 嵌入模型架构检测与路由
//!
//! 设计特点:
//! - 从 HuggingFace `config.json` 自动检测模型架构（BERT / LLaMA / Qwen2）
//! - 若 config.json 不够明确，回退检查 safetensors 文件中的 tensor 名称前缀
//!
//! （BERT 键以 `bert.` 开头，LLaMA/Qwen 键以 `model.` 开头）
//! - 每种架构对应一个独立的编码器实现，方便扩展新架构
//! - 池化策略与架构绑定：BERT → mean pooling，LLaMA → last token pooling
//! - 提供统一 `TextEncoder` trait，上层代码无需关心具体架构
//! - 架构检测失败时有明确错误信息，含 config.json 路径和失败原因
//! - `common` 模块提供共享工具（L2 归一化、safetensors header 读取、config JSON 解析）

pub mod bert;
pub mod common;
pub mod llama;
pub mod llama_head_dim;

use ramaria_core::error::RamariaResult;
use serde::Deserialize;
use std::path::Path;

// =========================================================
// 架构枚举
// =========================================================

/// 支持的模型架构。
///
/// 职责:
/// - 决定使用哪个编码器实现
/// - 绑定默认池化策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelArchitecture {
    /// BERT 架构（bge-small-zh-v1.5 等）
    /// - 编码器：`bert.rs`
    /// - 池化：Mean pooling + L2 normalize
    Bert,
    /// LLaMA 架构（标准 head_dim = hidden_size / num_attention_heads）
    /// - 编码器：`llama.rs`
    /// - 池化：Last token pooling + L2 normalize
    Llama,
    /// LLaMA head_dim 变体（config.json 显式指定 head_dim ≠ hidden_size/num_heads）
    /// - 编码器：`llama_head_dim.rs`（基于 candle qwen3 模块 + 内嵌无状态前向）
    /// - 池化：Last token pooling + L2 normalize
    /// - 适用: Qwen3-Embedding 系列及其他显式 head_dim 的 Qwen3 变体
    LlamaHeadDim,
}

impl ModelArchitecture {
    /// 返回此架构的默认向量维度。
    pub fn default_dimension(&self) -> usize {
        match self {
            Self::Bert => 384,          // bge-small-zh-v1.5
            Self::Llama => 1024,        // LLaMA 标准
            Self::LlamaHeadDim => 1024, // head_dim 变体典型值
        }
    }

    /// 返回此架构的人类可读名称。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Bert => "BERT",
            Self::Llama => "LLaMA",
            Self::LlamaHeadDim => "LLaMA-HeadDim",
        }
    }

    /// 返回另一种架构（用于容错回退）。
    pub fn opposite(&self) -> Self {
        match self {
            Self::Bert => Self::Llama,
            Self::Llama => Self::Bert,
            Self::LlamaHeadDim => Self::Llama,
        }
    }
}

// =========================================================
// Config.json 解析（最小结构）
// =========================================================

/// HuggingFace config.json 的最小解析结构。
///
/// 只提取架构检测和维度信息所需的字段，忽略其余。
#[derive(Debug, Deserialize)]
struct ModelConfig {
    /// 架构类型列表（如 `["BertModel"]`、`["Qwen3ForCausalLM"]`）
    architectures: Option<Vec<String>>,
    /// 隐藏层维度（BERT 的 `hidden_size`，LLaMA 的 `hidden_size` 或 `dim`）
    hidden_size: Option<usize>,
    /// 部分 LLaMA/Qwen 模型使用 `dim` 而非 `hidden_size`
    #[serde(rename = "dim")]
    dim: Option<usize>,
    /// 模型类型（如 `"bert"`, `"qwen3"`, `"llama"`）
    model_type: Option<String>,
}

// =========================================================
// 架构检测
// =========================================================

/// 从模型目录的 `config.json` 检测模型架构。
///
/// 参数:
/// - `model_dir`: 包含 config.json 的模型目录路径。
///
/// 返回:
/// - `ModelArchitecture`: 检测到的架构类型。
///
/// 错误场景:
/// - config.json 缺失或不可读。
/// - config.json 格式无效。
/// - 架构类型不在支持列表中。
///
/// 检测策略（按优先级）:
/// 1. `architectures[0]` 包含 "Bert" → BERT
/// 2. `architectures[0]` 包含 "Qwen" 或 "Llama" → LLaMA
/// 3. `model_type` 为 "bert" → BERT
/// 4. `model_type` 为 "qwen2"/"qwen3"/"llama" → LLaMA
pub fn detect_architecture(model_dir: &Path) -> RamariaResult<(ModelArchitecture, usize)> {
    let config_path = model_dir.join("config.json");

    if !config_path.exists() {
        return Err(ramaria_core::error::RamariaError::embedding(format!(
            "config.json 缺失: {}。请确保模型目录包含 HuggingFace 配置文件",
            config_path.display()
        )));
    }

    let config_bytes = std::fs::read(&config_path).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "无法读取 config.json: {} — {}",
            config_path.display(),
            e
        ))
    })?;

    let config: ModelConfig = serde_json::from_slice(&config_bytes).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "config.json 解析失败: {} — {}",
            config_path.display(),
            e
        ))
    })?;

    let arch = detect_from_config(&config, &config_path)?;
    let dim = config
        .hidden_size
        .or(config.dim)
        .unwrap_or_else(|| arch.default_dimension());

    tracing::info!(
        architecture = %arch.name(),
        dimension = dim,
        config_path = %config_path.display(),
        "模型架构已检测"
    );

    Ok((arch, dim))
}

/// 从已解析的 ModelConfig 推断架构。
///
/// 策略优先级:
/// 1. config.json 的 `architectures` 字段
/// 2. config.json 的 `model_type` 字段
/// 3. config.json 的 `head_dim` 字段（显式指定 → LlamaHeadDim）
/// 4. safetensors 文件中的 tensor 键名前缀（BERT 以 `bert.` 开头，LLaMA 以 `model.` 开头）
fn detect_from_config(
    config: &ModelConfig,
    config_path: &Path,
) -> RamariaResult<ModelArchitecture> {
    // 策略 1：检查 architectures 列表
    if let Some(ref archs) = config.architectures {
        for arch_name in archs {
            let lower = arch_name.to_lowercase();
            if lower.contains("bert") {
                return Ok(ModelArchitecture::Bert);
            }
            // Qwen3 或其他显式 head_dim 变体 → LlamaHeadDim
            if lower.contains("qwen3") {
                return Ok(ModelArchitecture::LlamaHeadDim);
            }
            if lower.contains("qwen") || lower.contains("llama") {
                return Ok(ModelArchitecture::Llama);
            }
        }
    }

    // 策略 2：检查 model_type
    if let Some(ref model_type) = config.model_type {
        let lower = model_type.to_lowercase();
        match lower.as_str() {
            "bert" => return Ok(ModelArchitecture::Bert),
            "qwen3" => return Ok(ModelArchitecture::LlamaHeadDim),
            "qwen2" | "llama" | "qwen2_5" => return Ok(ModelArchitecture::Llama),
            _ => {}
        }
    }

    // 策略 3：检查是否显式指定 head_dim（通用检测，不限品牌）
    if config_has_head_dim(config_path) {
        tracing::info!("config.json 含显式 head_dim 字段，使用 LlamaHeadDim 编码器");
        return Ok(ModelArchitecture::LlamaHeadDim);
    }

    // 策略 4：检查 safetensors 文件中的 tensor 键名
    // 这是最可靠的方式，因为 tensor 名称是权重文件的物理事实。
    if let Some(arch) = detect_from_safetensors(config_path) {
        tracing::info!(
            strategy = "safetensors_keys",
            architecture = %arch.name(),
            config_path = %config_path.display(),
            "通过 safetensors 键名推断架构"
        );
        return Ok(arch);
    }

    // 无法检测
    let arch_hint = config
        .architectures
        .as_ref()
        .map(|a| a.join(", "))
        .unwrap_or_else(|| "(无)".to_string());
    let type_hint = config.model_type.as_deref().unwrap_or("(无)");

    Err(ramaria_core::error::RamariaError::embedding(format!(
        "无法识别模型架构: {}。architectures=[{}], model_type={}。\n\
         当前支持: BERT（bge-small-zh-v1.5）、LLaMA/Qwen（Qwen3-Embedding-0.6B）",
        config_path.display(),
        arch_hint,
        type_hint
    )))
}

/// 通过检查 safetensors 文件中的 tensor 键名前缀来推断模型架构。
///
/// - BERT 模型的 tensor 键以 `bert.` 开头
/// - LLaMA/Qwen 模型的 tensor 键以 `model.` 开头
///
/// 此方法复用 `common::read_safetensors_header` 只读取 header（不加载权重），非常快。
fn detect_from_safetensors(config_path: &Path) -> Option<ModelArchitecture> {
    let model_dir = config_path.parent()?;
    let st_path = model_dir.join("model.safetensors");

    if !st_path.exists() {
        return None;
    }

    // 使用共享的 header 读取函数
    let header_bytes = common::read_safetensors_header(&st_path).ok()?;

    // 只解析足够判断架构的部分——不反序列化整个 header
    let header_str = String::from_utf8_lossy(&header_bytes);

    // 检查第一个 tensor 键的前缀
    // BERT: "bert.embeddings..." → 前缀 "bert."
    // LLaMA: "model.embed_tokens..." → 前缀 "model."
    if header_str.contains("\"bert.") {
        return Some(ModelArchitecture::Bert);
    }
    if header_str.contains("\"model.") {
        return Some(ModelArchitecture::Llama);
    }

    None
}

/// 检查 config.json 是否显式包含 `head_dim` 字段（通用检测，不限模型品牌）。
///
/// 若有此字段且值不等于 hidden_size/num_attention_heads，该模型应使用 LlamaHeadDim 编码器。
fn config_has_head_dim(config_path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(config_path) else {
        return false;
    };
    let Ok(raw): std::result::Result<serde_json::Value, _> = serde_json::from_reader(file) else {
        return false;
    };
    // 检查 head_dim 字段存在且为正整数
    raw.get("head_dim")
        .and_then(|v| v.as_u64())
        .map(|v| v > 0)
        .unwrap_or(false)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 BERT config 检测
    #[test]
    fn detect_bert_from_config() {
        let dir = std::env::temp_dir().join("ramaria_test_bert_config");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config_json = r#"{
            "architectures": ["BertModel"],
            "hidden_size": 512,
            "model_type": "bert"
        }"#;
        std::fs::write(dir.join("config.json"), config_json).unwrap();

        let (arch, dim) = detect_architecture(&dir).unwrap();
        assert_eq!(arch, ModelArchitecture::Bert);
        assert_eq!(dim, 512);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试 Qwen3 config 检测
    #[test]
    fn detect_qwen3_from_config() {
        let dir = std::env::temp_dir().join("ramaria_test_qwen_config");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config_json = r#"{
            "architectures": ["Qwen3ForCausalLM"],
            "hidden_size": 1024,
            "model_type": "qwen3"
        }"#;
        std::fs::write(dir.join("config.json"), config_json).unwrap();

        let (arch, dim) = detect_architecture(&dir).unwrap();
        assert_eq!(arch, ModelArchitecture::LlamaHeadDim);
        assert_eq!(dim, 1024);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试 LLaMA config 使用 `dim` 字段
    #[test]
    fn detect_llama_with_dim_field() {
        let dir = std::env::temp_dir().join("ramaria_test_llama_dim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config_json = r#"{
            "architectures": ["LlamaForCausalLM"],
            "dim": 4096,
            "model_type": "llama"
        }"#;
        std::fs::write(dir.join("config.json"), config_json).unwrap();

        let (arch, dim) = detect_architecture(&dir).unwrap();
        assert_eq!(arch, ModelArchitecture::Llama);
        assert_eq!(dim, 4096);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试缺失 config.json
    #[test]
    fn missing_config_json() {
        let dir = std::env::temp_dir().join("ramaria_test_missing_config");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let result = detect_architecture(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("config.json 缺失"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试不支持的架构
    #[test]
    fn unsupported_architecture() {
        let dir = std::env::temp_dir().join("ramaria_test_unknown_arch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config_json = r#"{
            "architectures": ["GPT2Model"],
            "hidden_size": 768,
            "model_type": "gpt2"
        }"#;
        std::fs::write(dir.join("config.json"), config_json).unwrap();

        let result = detect_architecture(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("无法识别模型架构"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
