//! crates/ramaria-llm/src/embedding/models/bert/config.rs - BERT config.json 手工解析
//!
//! 设计特点:
//! - 逐字段从原始 JSON 提取后手工构造 candle `BertConfig`，绕开激活函数枚举的解析限制
//! - 未知激活函数映射为 Gelu 并记 warn；缺失字段使用 bge-small-zh-v1.5 的默认值

use candle_transformers::models::bert::Config as BertConfig;
use ramaria_core::error::RamariaResult;
use std::path::Path;

/// 手动解析 config.json 并构造 candle `BertConfig`。
///
/// candle 0.8 的 `BertConfig` 的 `hidden_act` 枚举仅支持 gelu/relu，
/// Qwen3 等模型的 config.json 会包含 `silu`，直接反序列化会失败。
/// 此处从原始 JSON 逐个提取字段后手工构造，将未知激活函数映射为 Gelu。
pub(super) fn parse_config(config_path: &Path) -> RamariaResult<BertConfig> {
    use super::super::common::{config_bool, config_f64, config_usize};

    let file = std::fs::File::open(config_path).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "BERT 配置文件打开失败: {} — {}",
            config_path.display(),
            e
        ))
    })?;

    let raw: serde_json::Value = serde_json::from_reader(file).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "BERT 配置 JSON 解析失败: {} — {}",
            config_path.display(),
            e
        ))
    })?;

    // hidden_act: 将不支持的激活函数映射为 Gelu（如 silu → Gelu 警告）
    let hidden_act = raw
        .get("hidden_act")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "gelu" => candle_transformers::models::bert::HiddenAct::Gelu,
            "gelu_approximate" | "gelu_fast" => {
                candle_transformers::models::bert::HiddenAct::GeluApproximate
            }
            "relu" => candle_transformers::models::bert::HiddenAct::Relu,
            other => {
                tracing::warn!(hidden_act = other, "BERT 不支持此激活函数，使用 Gelu 替代");
                candle_transformers::models::bert::HiddenAct::Gelu
            }
        })
        .unwrap_or(candle_transformers::models::bert::HiddenAct::Gelu);

    Ok(BertConfig {
        vocab_size: config_usize(&raw, "vocab_size", 21128),
        hidden_size: config_usize(&raw, "hidden_size", 512),
        num_hidden_layers: config_usize(&raw, "num_hidden_layers", 4),
        num_attention_heads: config_usize(&raw, "num_attention_heads", 8),
        intermediate_size: config_usize(&raw, "intermediate_size", 2048),
        hidden_act,
        hidden_dropout_prob: config_f64(&raw, "hidden_dropout_prob", 0.1),
        max_position_embeddings: config_usize(&raw, "max_position_embeddings", 512),
        type_vocab_size: config_usize(&raw, "type_vocab_size", 2),
        initializer_range: config_f64(&raw, "initializer_range", 0.02),
        layer_norm_eps: config_f64(&raw, "layer_norm_eps", 1e-12),
        pad_token_id: config_usize(&raw, "pad_token_id", 0),
        position_embedding_type: {
            raw.get("position_embedding_type")
                .and_then(|v| v.as_str())
                .map(|s| match s {
                    "absolute" => {
                        candle_transformers::models::bert::PositionEmbeddingType::Absolute
                    }
                    _ => candle_transformers::models::bert::PositionEmbeddingType::Absolute,
                })
                .unwrap_or(candle_transformers::models::bert::PositionEmbeddingType::Absolute)
        },
        use_cache: config_bool(&raw, "use_cache", true),
        classifier_dropout: raw.get("classifier_dropout").and_then(|v| v.as_f64()),
        model_type: raw
            .get("model_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}
