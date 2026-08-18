//! crates/ramaria-app/src/app_knowledge.rs - 知识层对话注入用例
//!
//! 设计特点:
//! - 对话时从存储读取 persona 的 active 事实，经规则判定器判断是否命中当前用户消息
//! - 命中 → 返回 facts 供 prompt 知识块注入；未命中 → 空（静默降级，不影响主线）
//! - 零新增 LLM 调用（纯规则判定器，见 `ramaria_memory::fact::retriever`）
//! - 检索失败记 warn 后置空（不阻塞对话主流程）
//! - 只取 status=active 事实（版本链中仅当前生效参与注入）；严格按 persona_uid 隔离

use ramaria_core::config::KnowledgeConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::PersonaFact;
use ramaria_memory::fact::retriever::{KnowledgeQuery, judge_knowledge_query, retrieve_knowledge};

/// 从存储加载 persona 的 active 事实并做判定器命中判断。
///
/// 参数:
/// - `storage`: 存储后端（经 trait 访问）。
/// - `config`: [knowledge] 配置（判定器开关、预算）。
/// - `persona_uid`: 目标 persona（严格隔离，跨 persona 不可见）。
/// - `user_message`: 用户当前输入（判定器输入）。
///
/// 返回:
/// - 判定器命中且检索有结果 → 匹配的 active facts。
/// - 未命中 / 关闭 / 检索失败 → 空 Vec（不注入）。
pub async fn load_knowledge_facts(
    storage: &dyn StorageBackend,
    config: KnowledgeConfig,
    persona_uid: &str,
    user_message: &str,
) -> Vec<PersonaFact> {
    // 判定器关闭 → 不检索注入
    if !config.detector_enabled {
        return Vec::new();
    }

    // 读取活性事实（失败 → 降级为空，不阻塞）
    let active = match storage.list_active_facts_by_persona(persona_uid).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(persona_uid, error = %e, "知识层检索：读取 active 事实失败，本次不注入");
            return Vec::new();
        }
    };
    if active.is_empty() {
        return Vec::new();
    }

    // 判定器命中判断
    let level = judge_knowledge_query(user_message, &active);
    if level == ramaria_memory::fact::retriever::MatchLevel::None {
        return Vec::new();
    }

    // 命中 → 召回（全量 active 按时效排序；渲染预算由 prompt 层裁剪）
    let query = KnowledgeQuery {
        user_message: user_message.to_string(),
        facts: active,
        budget_chars: config.injection_budget_chars,
    };
    let now = ramaria_core::types::now_ms();
    let retrieval = retrieve_knowledge(&query, now, config.volatile_halflife_days);
    if retrieval.matched.is_empty() {
        Vec::new()
    } else {
        retrieval.matched
    }
}
