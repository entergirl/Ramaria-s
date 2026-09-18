//! crates/ramaria-app/src/app_knowledge.rs - 知识层对话注入用例（在线管线入口）
//!
//! 设计特点:
//! - 对话时从存储读取 persona 的 active 事实，经规则判定器判断是否命中当前用户消息
//! - 命中 → 返回 facts 供 prompt 知识块注入；未命中 → 空（静默降级，不影响主线）
//! - 零新增 LLM 调用（纯规则判定器）
//! - 检索失败记 warn 后置空（不阻塞对话主流程）
//! - 编排实现位于 `ramaria_memory::fact::retriever`（与服务层 recall 用例共用同一份），
//!   本模块只保留在线管线入口的薄封装

use ramaria_core::config::KnowledgeConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::PersonaFact;

/// 从存储加载 persona 的 active 事实并做判定器命中判断。
///
/// 参数:
/// - `storage`: 存储后端（经 trait 访问）。
/// - `config`: [knowledge] 配置（判定器开关、检索参数、渲染预算）。
/// - `persona_uid`: 目标 persona（严格隔离，跨 persona 不可见）。
/// - `user_message`: 用户当前输入（判定器输入）。
///
/// 返回:
/// - 判定器命中且检索有结果 → 匹配的 active facts。
/// - 未命中 / 关闭 / 检索失败 → 空 Vec（不注入）。
///
/// 说明:
/// - 编排实现见 `ramaria_memory::fact::retriever::load_knowledge_facts_for_query`。
pub async fn load_knowledge_facts(
    storage: &dyn StorageBackend,
    config: KnowledgeConfig,
    persona_uid: &str,
    user_message: &str,
) -> Vec<PersonaFact> {
    ramaria_memory::fact::retriever::load_knowledge_facts_for_query(
        storage,
        &config,
        persona_uid,
        user_message,
    )
    .await
}
