//! rust/crates/ramaria-llm/src/embedding/mod.rs - Embedding Provider 模块入口
//!
//! 设计特点:
//! - 通过 feature `embedding-native` 条件编译原生 safetensors 实现（推荐）
//! - 通过 feature `embedding-onnx` 条件编译 ONNX Runtime 实现
//! - 无 feature 时，提供空占位类型 `NoopEmbeddingProvider` 供测试和降级使用
//! - 公共 API：`create_native_provider` 工厂函数
//! - `EmbeddingProvider` trait 定义在 `ramaria_core::traits`，本模块仅提供具体实现
//!
//! Feature 推荐:
//! - 生产环境: `embedding-native` — 支持 bge-small-zh-v1.5 和 Qwen3-Embedding-0.6B
//! - 测试/降级: 无 feature — 使用 NoopEmbeddingProvider
//! - 替代方案: `embedding-onnx` — ONNX Runtime 后端

#[cfg(feature = "embedding-native")]
pub mod native;

#[cfg(feature = "embedding-native")]
mod models;

#[cfg(feature = "embedding-onnx")]
pub mod onnx;

/// 无 embedding feature 时的占位 provider（始终返回 `is_available = false`）。
///
/// 职责:
/// - 在未编译嵌入支持时提供编译期占位，避免上层代码条件编译散落。
/// - `is_available` 始终返回 false，`embed` 始终返回 Unsupported 错误。
pub mod noop;
