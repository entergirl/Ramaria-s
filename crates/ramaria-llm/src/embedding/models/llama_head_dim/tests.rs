//! crates/ramaria-llm/src/embedding/models/llama_head_dim/tests.rs - LLaMA head_dim 变体编码器单元测试
//!
//! 设计特点:
//! - 覆盖 Qwen3 config.json 的 null 字段（sliding_window）解析兼容性
//! - 覆盖缺失 head_dim 字段时的解析失败（架构检测归入 LlamaHeadDim 的必要条件）
//! - 编码器端到端验证需要真实模型文件，由应用层命令在真实模型目录执行

use super::*;

// 说明:
// - 编码器需要真实模型文件（config.json + safetensors + tokenizer.json），
//   无法在 CI 构造，端到端验证由 `validate_embedding_model` 命令在真实
//   模型目录上执行（见 ramaria-desktop commands/setup.rs）。
// - Qwen3-Embedding config.json 的 `"sliding_window": null`
//   解析由 candle `qwen3::Config` 的 `Option<usize>` 字段天然兼容，
//   反序列化行为由 candle 单测覆盖；此处保留空模块占位。

/// 防御断言：Qwen3-Embedding 官方 config 的 null 字段在 qwen3::Config 中为 Option。
#[test]
fn qwen3_config_accepts_null_sliding_window() {
    // 与 Qwen3-Embedding-0.6B config.json 结构一致的最小样例
    let json = r#"{
        "vocab_size": 151669,
        "hidden_size": 1024,
        "intermediate_size": 3072,
        "num_hidden_layers": 28,
        "num_attention_heads": 16,
        "head_dim": 128,
        "attention_bias": false,
        "num_key_value_heads": 8,
        "max_position_embeddings": 32768,
        "sliding_window": null,
        "max_window_layers": 28,
        "tie_word_embeddings": true,
        "rope_theta": 1000000,
        "rms_norm_eps": 1e-06,
        "use_sliding_window": false,
        "hidden_act": "silu"
    }"#;
    let cfg: Qwen3Config = serde_json::from_str(json).expect("qwen3::Config 应接受 null 字段");
    assert_eq!(cfg.head_dim, 128, "head_dim 应正确解析");
    assert_eq!(cfg.hidden_size, 1024);
    assert!(cfg.sliding_window.is_none(), "sliding_window: null → None");
    assert!(!cfg.use_sliding_window);
}

/// 防御断言：缺失 head_dim 字段时给出明确解析错误（提示架构不匹配）。
#[test]
fn qwen3_config_requires_head_dim() {
    // qwen3::Config 的 head_dim 为必填 usize 字段（无 serde default），
    // 缺失时报错——这正是架构检测将其归入 LlamaHeadDim 的必要条件。
    let json = r#"{
        "vocab_size": 151669,
        "hidden_size": 1024,
        "intermediate_size": 3072,
        "num_hidden_layers": 28,
        "num_attention_heads": 16,
        "attention_bias": false,
        "num_key_value_heads": 8,
        "max_position_embeddings": 32768,
        "sliding_window": null,
        "max_window_layers": 28,
        "tie_word_embeddings": true,
        "rope_theta": 1000000,
        "rms_norm_eps": 1e-06,
        "use_sliding_window": false,
        "hidden_act": "silu"
    }"#;
    let result: Result<Qwen3Config, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "缺失 head_dim 应解析失败（非本编码器适用模型）"
    );
}
