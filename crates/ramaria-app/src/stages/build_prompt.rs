//! rust/crates/ramaria-app/src/stages/build_prompt.rs - Stage 6: 5-Block System Prompt 装配
//!
//! 设计特点:
//! - 从 DB 加载 persona/facts/traits/examples，调用 `assemble_prompt` 组装 5-Block Prompt
//! - 空 traits 时回退 persona.toml（冷启动兜底，不依赖 LLM 结构化拆解）
//! - 无 persona 数据时使用默认 Ramaria 基础 prompt（"具有记忆能力、善解人意的 AI 助手"）
//! - 各数据加载独立调用，单个失败不阻塞整体（warn 日志 + 跳过对应 Block）
//! - 注入跨 session 上下文（recent_summaries / last_active_at），提升对话连续性
//! - 纯函数式数据加载 + 装配，不持有可变状态

use async_trait::async_trait;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Persona, ProfileField};
use ramaria_memory::SHARED_CHAT_STYLE_RULES;
use ramaria_memory::parse_persona_toml;
use ramaria_memory::prompt::builder::{PromptConfig, PromptContext, assemble_prompt};

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

// =========================================================
// StageBuildPrompt
// =========================================================

/// Stage 6: 5-Block System Prompt 装配。
///
/// 职责:
/// - 从 StorageBackend 加载当前 persona 的结构化画像数据
/// - 调用 `assemble_prompt` 组装 Block A-E 五段式 System Prompt
/// - 无 persona 时降级为基础 Ramaria 默认 prompt
///
/// 降级策略（逐层退化）:
/// 1. DB persona 存在 + traits 非空 → 完整 5-Block 装配（最佳路径）
/// 2. DB persona 存在 + facts+traits 均为空 → 尝试 persona.toml 冷启动兜底
/// 3. DB persona 不存在 → 默认 Ramaria 基础 prompt
/// 4. 各数据源加载失败 → warn 日志，跳过对应 Block（不中断管线）
pub struct StageBuildPrompt;

impl StageBuildPrompt {
    /// 创建 StageBuildPrompt 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageBuildPrompt {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageBuildPrompt {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "BuildPrompt"
    }

    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        let persona_uid = input.persona_uid.as_deref().unwrap_or("rama-0001");

        tracing::debug!(
            persona_uid = persona_uid,
            summaries = input.recent_summaries.len(),
            "StageBuildPrompt: 开始加载 persona 数据"
        );

        // 尝试加载 DB persona
        let persona = match ctx.storage.get_persona_by_uid(persona_uid).await {
            Ok(Some(p)) => {
                tracing::debug!(persona_uid = persona_uid, persona_name = %p.name, "persona 已加载");
                Some(p)
            }
            Ok(None) => {
                tracing::debug!(persona_uid = persona_uid, "persona 不存在，使用默认 prompt");
                None
            }
            Err(e) => {
                tracing::warn!(persona_uid = persona_uid, %e, "加载 persona 失败，使用默认 prompt");
                None
            }
        };

        // 有 persona 数据 → 5-Block 装配
        if let Some(ref p) = persona {
            let system_prompt = build_structured_prompt(
                ctx.storage.as_ref(),
                p,
                &input.recent_summaries,
                input.last_active_at.as_deref(),
                // v1.4 M6（T-V14-6-004）：examples.max_examples 经 RamariaConfig 传播
                ctx.config.examples.max_examples as usize,
            )
            .await;

            input.system_prompt = Some(system_prompt);
            tracing::info!(
                persona_uid = persona_uid,
                prompt_chars = input.system_prompt.as_ref().map(|s| s.len()).unwrap_or(0),
                "StageBuildPrompt: 5-Block System Prompt 已装配"
            );
        } else {
            // 降级：默认 Ramaria 基础 prompt
            let fallback = default_ramaria_prompt();
            tracing::info!(
                persona_uid = persona_uid,
                prompt_chars = fallback.len(),
                "StageBuildPrompt: 使用默认 Ramaria System Prompt"
            );
            input.system_prompt = Some(fallback);
        }

        Ok(input)
    }
}

// =========================================================
// 核心逻辑：从 DB 加载结构化画像 → 5-Block 装配
// =========================================================

/// 使用 DB 中的 persona 结构化数据构建 System Prompt。
///
/// 流程:
/// 1. 并行加载 facts / traits / examples（各独立降级）
/// 2. facts+traits 均为空时，尝试 persona.toml 冷启动兜底
/// 3. 构建 PromptContext，调用 `assemble_prompt`
///
/// 参数:
/// - `storage`: 存储后端。
/// - `persona`: 当前 persona 记录。
/// - `recent_summaries`: 近期 L1 摘要列表（跨 session 上下文）。
/// - `last_active_at`: 最后活跃时间（YYYY-MM-DD HH:MM 格式）。
/// - `max_examples`: `[examples].max_examples` 配置（v1.4 M6 配置传播）。
async fn build_structured_prompt(
    storage: &dyn StorageBackend,
    persona: &Persona,
    recent_summaries: &[String],
    last_active_at: Option<&str>,
    max_examples: usize,
) -> String {
    // 并行加载关联数据（各独立调用，失败单独降级）
    let facts = storage
        .list_facts_by_persona(&persona.uid, ProfileField::BasicInfo)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(persona_uid = %persona.uid, %e, "加载 facts 失败，跳过");
            Vec::new()
        });

    let traits = storage
        .list_traits_by_persona(&persona.uid)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(persona_uid = %persona.uid, %e, "加载 traits 失败，跳过");
            Vec::new()
        });

    let examples = storage
        .list_selected_examples(&persona.uid)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(persona_uid = %persona.uid, %e, "加载 examples 失败，跳过");
            Vec::new()
        });

    // 冷启动兜底：facts+traits 均为空时，尝试加载 persona.toml
    // 优先从 DB persona.config 读取，其次回退到文件系统
    if facts.is_empty()
        && traits.is_empty()
        && let Some(prompt) = load_persona_toml_fallback(persona.config.as_deref())
    {
        tracing::info!(
            persona_uid = %persona.uid,
            "使用 persona.toml 加载的系统 prompt（无结构化画像，冷启动兜底）"
        );
        return prompt;
    }

    // 正常路径：CRISPE 装配
    let rules = resolve_chat_style_rules(persona);
    let ctx = PromptContext {
        persona: Some(persona.clone()),
        facts,
        traits,
        examples,
        memory_context: None, // memory_context 在 ChatRequest 中单独注入
        recent_session_summaries: recent_summaries.to_vec(),
        last_active_at: last_active_at.map(|s| s.to_string()),
        knowledge_boundary: None,
        current_time_str: Some(crate::now_timestamp_str()),
        weather: None,
        chat_style_rules: Some(rules), // v2.0: 回复规则作为 Experiment 块注入
        utt_context: None,             // v1.4: 原文片段由活跃路径（app_chat）注入，本 Stage 未接线
        bridge_context: None, // v1.4 M5: 桥接内容由活跃路径（app_chat）注入，本 Stage 未接线
        behavior_decision: None, // v1.5 M6: 行为路由由活跃路径（app_chat）注入，本 Stage 未接线
    };

    let config = PromptConfig {
        // v1.4 M6（T-V14-6-004）：[examples].max_examples 经 RamariaConfig 传播
        max_examples,
        ..Default::default()
    };
    tracing::debug!(
        persona_uid = %persona.uid,
        facts = ctx.facts.len(),
        traits = ctx.traits.len(),
        examples = ctx.examples.len(),
        summaries = ctx.recent_session_summaries.len(),
        "CRISPE System Prompt 已装配"
    );

    assemble_prompt(&ctx, &config)
}

/// 解析当前 persona 的聊天回复风格规则。
///
/// 优先级:
/// 1. 若 persona.config 中包含 `E_rules` 块 → 使用自定义规则。
/// 2. 否则 → 使用共享社交平台口吻模板 `SHARED_CHAT_STYLE_RULES`。
fn resolve_chat_style_rules(persona: &Persona) -> String {
    // 尝试从 persona.config 中提取自定义 E_rules
    if let Some(ref cfg) = persona.config
        && let Ok(parsed) = ramaria_memory::parse_persona_toml(cfg)
        && let Some(rules) = parsed
            .blocks
            .iter()
            .find(|(k, _)| k == "E_rules")
            .map(|(_, v)| v.clone())
        && !rules.trim().is_empty()
    {
        tracing::debug!(
            persona_uid = %persona.uid,
            "使用 persona.config 中的自定义 E_rules"
        );
        return rules;
    }

    // 默认使用共享社交平台口吻
    tracing::debug!(
        persona_uid = %persona.uid,
        "使用共享社交平台聊天口吻（无自定义 E_rules）"
    );
    SHARED_CHAT_STYLE_RULES.to_string()
}

// =========================================================
// 降级路径：persona.toml 冷启动兜底
// =========================================================

/// 尝试加载 persona.toml 并构建基础 system prompt。
///
/// 数据来源优先级:
/// 1. `db_config`: DB persona.config 中存储的 TOML 内容（setup 时写入）
/// 2. 文件系统回退: `../config/personas/rama-0001.toml` → `../config/persona.toml`
///
/// 成功时返回由 A_persona + E_rules 组装的基础 prompt。
/// 失败时返回 `None`，由上层降级到通用 prompt。
fn load_persona_toml_fallback(db_config: Option<&str>) -> Option<String> {
    let content = if let Some(cfg) = db_config {
        // 优先使用 DB 中的 persona.toml 内容
        if cfg.contains("[identity]") || cfg.contains("[blocks]") {
            tracing::debug!("从 DB persona.config 加载 persona.toml");
            cfg.to_string()
        } else {
            // config 字段是其他 JSON 格式，回退到文件系统
            fallback_read_persona_toml()?
        }
    } else {
        fallback_read_persona_toml()?
    };

    let parsed = match parse_persona_toml(&content) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e, "persona.toml 解析失败");
            return None;
        }
    };

    let persona_block = parsed
        .blocks
        .iter()
        .find(|(k, _)| k == "A_persona")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let rules_block = parsed
        .blocks
        .iter()
        .find(|(k, _)| k == "E_rules")
        .map(|(_, v)| v.as_str())
        .filter(|s| !s.trim().is_empty())
        // 无自定义 E_rules 时使用共享社交平台口吻
        .unwrap_or(SHARED_CHAT_STYLE_RULES);

    let name = &parsed.assistant_name;
    let time_str = crate::now_timestamp_str();

    Some(format!(
        "你的名字是{name}。\n\n{persona_block}\n\n回复规则:\n{rules_block}\n\n\
         当前时间：{time_str}\n\n\
         你可以记住与用户的对话历史。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。"
    ))
}

/// 文件系统回退: 优先新路径 `../config/personas/rama-0001.toml`，其次旧路径。
fn fallback_read_persona_toml() -> Option<String> {
    // 优先尝试新路径（目录扫描模式，每文件 = 一个 persona）
    let new_path = "../config/personas/rama-0001.toml";
    if let Ok(c) = std::fs::read_to_string(new_path) {
        tracing::debug!(%new_path, "从文件系统加载 persona.toml (新路径)");
        return Some(c);
    }

    // 回退到旧路径
    let old_path = "../config/persona.toml";
    match std::fs::read_to_string(old_path) {
        Ok(c) => {
            tracing::debug!(%old_path, "从文件系统加载 persona.toml (旧路径兼容)");
            Some(c)
        }
        Err(e) => {
            tracing::debug!(%old_path, %e, "persona.toml 文件系统回退失败");
            None
        }
    }
}

// =========================================================
// 最终降级：默认 Ramaria 基础 prompt
// =========================================================

/// 构建默认 Ramaria System Prompt（无任何 persona 数据时的最终降级）。
///
/// 语义:
/// - 身份: "具有记忆能力、善解人意的 AI 助手"
/// - 能力: 可记住对话历史，在后续对话中引用记忆
/// - 语气: 自然、友好
/// - 包含当前时间（供 LLM 感知时间上下文）
fn default_ramaria_prompt() -> String {
    format!(
        "你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
         你可以记住与用户的对话历史，并在后续对话中引用这些记忆。\n\
         请用自然、友好的语气回复用户。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。\n\
         当前时间：{}",
        crate::now_timestamp_str()
    )
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::simple_context;
    use uuid::Uuid;

    /// 构造最简 PipelineData，含 persona_uid。
    fn base_data(persona_uid: Option<&str>) -> PipelineData {
        PipelineData::new(
            "你好".to_string(),
            persona_uid.map(|s| s.to_string()),
            None,
            Uuid::new_v4(),
        )
    }

    // =========================================================
    // 测试: name
    // =========================================================

    #[test]
    fn stage_name() {
        let stage = StageBuildPrompt::new();
        assert_eq!(stage.name(), "BuildPrompt");
    }

    // =========================================================
    // 测试: 无 persona → 降级默认 prompt
    // =========================================================

    #[tokio::test]
    async fn no_persona_uses_default_prompt() {
        let ctx = simple_context();
        let stage = StageBuildPrompt::new();
        let data = base_data(None);

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed with default prompt");
        let prompt = output.system_prompt.expect("system_prompt should be set");
        assert!(
            prompt.contains("Ramaria"),
            "default prompt must mention Ramaria"
        );
        assert!(
            prompt.contains("记忆能力"),
            "default prompt must mention memory"
        );
    }

    // =========================================================
    // 测试: persona_uid 缺失时使用 "rama-0001" 兜底
    // （与 no_persona_uses_default_prompt 场景重复，已覆盖）
    // =========================================================

    // =========================================================
    // 测试: system_prompt 字段在 Stage 后确实被设置
    // =========================================================

    #[tokio::test]
    async fn system_prompt_is_populated() {
        let ctx = simple_context();
        let stage = StageBuildPrompt::new();
        let data = base_data(Some("rama-0001"));

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        let prompt = output.system_prompt.as_deref().unwrap();
        // 默认 prompt 包含当前时间
        assert!(!prompt.is_empty(), "system_prompt must be non-empty");
        assert!(
            prompt.contains("当前时间"),
            "default prompt must contain current time"
        );
    }

    // =========================================================
    // 测试: recent_summaries 参数在 Stage 间保持
    // =========================================================

    #[tokio::test]
    async fn preserves_recent_summaries() {
        let ctx = simple_context();
        let stage = StageBuildPrompt::new();
        let mut data = base_data(Some("rama-0001"));
        data.recent_summaries = vec![
            "上周讨论了出行计划，气氛轻松".to_string(),
            "昨天聊了工作安排".to_string(),
        ];

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());
        let output = result.expect("should succeed");
        assert_eq!(output.recent_summaries.len(), 2);
    }

    // =========================================================
    // 测试: default_ramaria_prompt 包含必要元素
    // =========================================================

    #[test]
    fn default_prompt_contains_key_elements() {
        let prompt = default_ramaria_prompt();
        assert!(prompt.contains("Ramaria"));
        assert!(prompt.contains("记忆能力"));
        assert!(prompt.contains("善解人意"));
        assert!(prompt.contains("AI 助手"));
        assert!(prompt.contains("当前时间"));
    }

    // =========================================================
    // 测试: load_persona_toml_fallback 失败返回 None
    // （原 no_db_no_file / invalid_content 无断言冒烟测试已删除）
    // =========================================================
}
