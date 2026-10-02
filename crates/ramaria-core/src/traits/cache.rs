//! crates/ramaria-core/src/traits/cache.rs - Ramaria LLM 响应缓存抽象模块
//!
//! 设计特点:
//! - 为 ProviderBase 提供调用前查询、成功后写入的精确缓存接口
//! - 只存响应不存原文，key 为模型/模板/提示的哈希
//! - 命中缓存不改变输出语义（同 key 同输出）
//! - 查询或写入失败由调用方降级，不阻塞主流程
//! - 支持条目计数与按策略淘汰最旧条目

use async_trait::async_trait;

use crate::error::RamariaResult;

// =========================================================
// LLM 响应缓存抽象层
// =========================================================

/// LLM 响应精确缓存接口。
///
/// 职责:
/// - 供 `ramaria-llm` 的 `ProviderBase` 在调用 LLM 前查询、成功后写入缓存，
///   覆盖重跑/重试/失败恢复导入与生成管线场景（不重复花费 API 账单）。
/// - 由 `ramaria-storage` 的 `SqliteLlmCache` 实现（`llm_response_cache` 表）。
///
/// 安全约束（隐私红线）:
/// - **只存响应，不存原文输入**：`key` 为 `sha256(model_id + template_version + prompt)`
///   哈希，`put` 只接收响应文本与元数据，不接收 prompt 原文。
/// - 命中缓存不改变输出语义（同 key 同输出）。
///
/// 降级约定:
/// - 查询失败由调用方（ProviderBase）记 warn 后直接走 LLM，不阻塞主流程。
/// - 写入失败由调用方记 warn 后继续。
#[async_trait]
pub trait LlmResponseCache: Send + Sync {
    /// 按 key 查询缓存响应，并更新访问时间与命中计数。
    ///
    /// 参数:
    /// - `key`: 缓存键（SHA-256 hex）。
    ///
    /// 返回:
    /// - `Ok(Some(response))`: 命中。
    /// - `Ok(None)`: 未命中。
    /// - `Err`: 查询失败（调用方应降级直接走 LLM）。
    async fn get(&self, key: &str) -> RamariaResult<Option<String>>;

    /// 写入一条缓存响应。
    ///
    /// 参数:
    /// - `key`: 缓存键（SHA-256 hex）。
    /// - `response`: LLM 响应文本（唯一存储的内容）。
    /// - `model_id`: 后端模型标识（`BackendConfig.capability.model_id`）。
    /// - `template_version`: Prompt 模板版本（`ChatRequest.template_version`）。
    ///
    /// 说明:
    /// - 同 key 已存在时覆盖（INSERT OR REPLACE）。
    async fn put(
        &self,
        key: &str,
        response: &str,
        model_id: &str,
        template_version: &str,
    ) -> RamariaResult<()>;

    /// 返回当前缓存条目数。
    async fn count(&self) -> RamariaResult<u64>;

    /// 按淘汰策略删除最旧条目，使剩余条目数不超过 `keep`。
    ///
    /// 参数:
    /// - `keep`: 保留上限（`[cache].max_entries`）。
    ///
    /// 返回:
    /// - 实际删除的条目数。
    async fn evict_oldest(&self, keep: u64) -> RamariaResult<u64>;
}
