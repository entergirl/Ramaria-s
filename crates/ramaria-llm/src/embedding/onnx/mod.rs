//! crates/ramaria-llm/src/embedding/onnx/mod.rs - Ramaria ONNX 嵌入模型 Provider 模块
//!
//! 设计特点:
//! - 基于 `ort` (ONNX Runtime v2) 实现推理，支持 BGE/BERT 等嵌入模型
//! - 使用 HuggingFace `tokenizers` 进行 BERT 分词（加载 tokenizer.json）
//! - Mean Pooling + L2 归一化，对齐 standard BERT embedding pipeline
//! - 惰性加载：`Session` 和 `Tokenizer` 仅在首次 `embed` 调用时初始化
//!
//! 停用说明:
//! - 本后端已停用：无 crate 启用 `embedding-onnx` feature，不承诺可编译，
//!   计划后续版本移除；生产嵌入路径使用 `embedding-native`。

mod provider;
mod session;

#[cfg(test)]
mod tests;

pub use provider::{OnnxEmbeddingProvider, create_onnx_provider};
