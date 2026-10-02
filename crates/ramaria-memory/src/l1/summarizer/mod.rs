//! crates/ramaria-memory/src/l1/summarizer/mod.rs - L0→L1 摘要生成管线
//!
//! 设计特点:
//! - 依赖注入: 通过 `&dyn LlmProvider` + `&dyn StorageBackend` 解耦具体实现
//! - 完整流程: 取消息 → 格式化 → 选Prompt → 调LLM → 解析JSON → 校验 → 存L1 + 写关键词
//! - types: LLM 响应反序列化目标与 evidence_notes 宽容解析
//! - pipeline: 编排入口（整会话按块生成 + 渐进式按段生成）
//! - generate: 单块生成（对话格式化 / LLM 调用 / JSON 解析 / 字段校验 / 关键词写回）
//! - helpers: 纯函数辅助（格式化 / 触发判断 / 上文构建 / 字段校验 / 关键词切分）
//! - 本模块对外逐项 re-export 子模块公开项，公共 API 路径与原单文件模块一致

mod config;
mod generate;
mod helpers;
mod pipeline;
mod types;

#[cfg(test)]
mod tests;

pub use config::L1SummarizerConfig;
pub use pipeline::L1Summarizer;

// 子模块辅助函数的 crate 内可见引用（pipeline/generate 经 `super::` 消费）
use helpers::*;
