//! crates/ramaria-memory/src/chat/examples.rs - Few-shot 示例预选
//!
//! 设计特点:
//! - 关闭评分轮换时回退静态 selected 查询（`list_selected_examples`）
//! - 记忆检索命中时不注入（避免与记忆内容重复）
//! - 记忆未命中时按话题/情绪/长度评分轮换选择，风格兜底
//! - 安全约束：日志只记录数量，不记录示例内容

use ramaria_core::config::ExamplesConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::PersonaExample;

// =========================================================
// 示例预选（few-shot 注入素材）
// =========================================================

/// 预选 Few-shot 示例。
///
/// 选择策略:
/// - `examples.enabled=false` → 回退：静态 `selected=1` 查询（`list_selected_examples`）。
/// - `examples.enabled=true`：
///   - 记忆检索命中（`memory_hit=true`）→ 不注入（避免与记忆内容重复）；
///   - 记忆未命中 → 从候选池按话题/情绪/长度评分轮换选择，风格兜底。
///
/// 降级:
/// - 候选池为空 / 存储失败 → 空列表（不注入）。
/// - 评分选择不满足最低条数 → 空列表（example_selector 语义，不强制凑数）。
///
/// 安全约束:
/// - 日志只记录数量，不记录示例内容。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `examples_cfg`: 示例配置（`[examples]`）。
/// - `persona_uid`: 人格 UID（None 表示 rama 自身，回退 "rama-0001"）。
/// - `user_input`: 用户当前输入（话题匹配关键词来源）。
/// - `memory_hit`: 记忆检索是否命中（RAG 上下文非空）。
///
/// 返回:
/// - 注入用示例列表（最多 `[examples].max_examples` 条）。
pub async fn load_examples_for_input(
    storage: &dyn StorageBackend,
    examples_cfg: &ExamplesConfig,
    persona_uid: Option<&str>,
    user_input: &str,
    memory_hit: bool,
) -> Vec<PersonaExample> {
    use crate::prompt::example_selector::{ExampleSelector, ExampleSelectorConfig};

    let uid = persona_uid.unwrap_or("rama-0001");

    // 关闭评分轮换时的兼容路径：静态 selected 注入（无条件）
    if !examples_cfg.enabled {
        return storage
            .list_selected_examples(uid)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(persona_uid = %uid, %e, "加载 selected examples 失败，跳过");
                Vec::new()
            });
    }

    // 评分轮换路径：记忆命中不重复注入（兜底语义）
    if memory_hit {
        tracing::debug!(persona_uid = %uid, "记忆检索命中，跳过 examples 兜底注入");
        return Vec::new();
    }

    // 记忆未命中 → 候选池评分轮换（风格兜底）
    let candidates = storage.list_all_examples(uid).await.unwrap_or_else(|e| {
        tracing::warn!(persona_uid = %uid, %e, "加载 examples 候选池失败，跳过");
        Vec::new()
    });
    if candidates.is_empty() {
        tracing::debug!(persona_uid = %uid, "examples 候选池为空，跳过注入");
        return Vec::new();
    }

    let keywords = crate::prompt::example_selector::extract_keywords(user_input);
    let keyword_refs: Vec<&str> = keywords.iter().map(|s| s.as_str()).collect();
    let selector_config = ExampleSelectorConfig {
        max_examples: examples_cfg.max_examples as usize,
        ..ExampleSelectorConfig::default()
    };

    let selected = ExampleSelector::select(&candidates, &keyword_refs, 0.0, &selector_config);

    tracing::debug!(
        persona_uid = %uid,
        candidates = candidates.len(),
        selected = selected.len(),
        "examples 评分轮换完成（记忆未命中兜底注入）"
    );
    selected
}
