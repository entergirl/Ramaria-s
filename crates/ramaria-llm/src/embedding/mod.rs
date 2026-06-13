//! rust/crates/ramaria-llm/src/embedding/mod.rs - Embedding Provider 模块入口
//!
//! 设计特点:
//! - 通过 feature `embedding-onnx` 条件编译 ONNX 实现，避免强制引入 ort 依赖
//! - 无 feature 时，提供空占位类型 `NoopEmbeddingProvider` 供测试和降级使用
//! - 公共 API：`create_onnx_provider()` 工厂函数
//! - `EmbeddingProvider` trait 定义在 `ramaria_core::traits`，本模块仅提供具体实现

#[cfg(feature = "embedding-onnx")]
pub mod onnx;

/// 无 embedding-onnx feature 时的占位 provider（始终返回 is_available() = false）。
///
/// 职责:
/// - 在未编译 ONNX 支持时提供编译期占位，避免上层代码条件编译散落。
/// - `is_available()` 始终返回 false，`embed()` 始终返回 Unsupported 错误。
pub mod noop;
