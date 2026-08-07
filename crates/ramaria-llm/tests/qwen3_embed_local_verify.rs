//! rust/crates/ramaria-llm/tests/qwen3_embed_local_verify.rs - Qwen3-Embedding 本地回归验证
//!
//! 修复回归：Qwen3-Embedding-0.6B 导入校验失败
//! （config.json `"sliding_window": null` 无法被 candle qwen2::Config 解析 +
//!   head_dim=128 与 qwen2 隐式 head_dim 不匹配）。
//! 修复后 `validate()` 与 `embed()` 均应通过。
//!
//! 运行方式（需要本机模型，默认路径 F:/9700/model/Qwen3-Embedding-0.6B）:
//!   QWEN3_EMBED_DIR=<模型目录> cargo test -p ramaria-llm \
//!     --features embedding-native --test qwen3_embed_local_verify -- --ignored
#![cfg(feature = "embedding-native")]

use ramaria_core::traits::EmbeddingProvider;
use ramaria_llm::embedding::native::NativeEmbeddingProvider;

/// 模型目录：优先取环境变量，回退默认路径。
fn model_dir() -> std::path::PathBuf {
    std::env::var("QWEN3_EMBED_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("F:/9700/model/Qwen3-Embedding-0.6B"))
}

/// 与桌面端 `validate_embedding_model` 相同的异步校验路径。
#[test]
#[ignore = "需要本机 Qwen3-Embedding 模型文件（CI 不下载模型）"]
fn verify_qwen3_embedding_06b_validate() {
    let dir = model_dir();
    assert!(dir.exists(), "模型目录应存在: {}", dir.display());

    let provider = NativeEmbeddingProvider::new(&dir).expect("provider 创建成功");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        provider.validate().await.expect("validate 应成功");
    });
    println!("OK: validate 通过（含测试推理）");
}

/// 直接嵌入一条文本，验证维度与 L2 归一化。
#[test]
#[ignore = "需要本机 Qwen3-Embedding 模型文件（CI 不下载模型）"]
fn verify_qwen3_embedding_06b_loads_and_embeds() {
    let dir = model_dir();
    assert!(dir.exists(), "模型目录应存在: {}", dir.display());

    let provider = NativeEmbeddingProvider::new(&dir).expect("provider 创建成功");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let vec = rt
        .block_on(provider.embed("测试一下嵌入效果"))
        .expect("嵌入成功");
    assert_eq!(vec.len(), 1024, "Qwen3-Embedding-0.6B 维度应为 1024");
    // L2 归一化校验（模块自身已做，这里确认数值有效）
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "向量应已 L2 归一化, norm={norm}");
    println!("OK: 维度={}, 前3维={:?}", vec.len(), &vec[..3]);
}
