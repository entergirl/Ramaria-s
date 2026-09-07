//! crates/ramaria-app/src/stages/token_budget.rs - Stage 7: Token 预算管理
//!
//! 设计特点:
//! - 调用 `token_budget::apply_token_budget` 进行字符级 token 估算与截断
//! - 使用 provider capability.context_window 作为预算上限
//! - 预算超出时记录 warn 日志（含 request_id、estimated、window），但不中断管线
//! - 优先级: System Prompt > memory_context > 历史消息（新→旧）> 用户消息（完整保留）
//! - 协调预算差异（`[injection_budget].enabled`）: 本 Stage 仅拿到整条
//!   system_prompt（Stage 6 `build_prompt.rs` 未接线行为/知识/utt/桥接注入，
//!   无法像生产装配路径那样逐层取舍）。协调启用时本 Stage 执行其可达成子集——
//!   对 RAG（memory_context）应用独立上限/总池裁剪，并放开对整条 system_prompt
//!   的二次句截断（交由装配期协调）。生产路径的逐层协调在 `app_chat.rs`
//!   `build_system_prompt_coordinated` 完成。
//! - 纯委托 token_budget 模块，Stage 自身不包含 token 估算逻辑
//! - 输出填充 PipelineData 的 budgeted_* 字段供 Stage 8 使用

use async_trait::async_trait;
use ramaria_core::error::RamariaError;
use ramaria_memory::token_budget::{self, TokenBudgetConfig};

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

// =========================================================
// StageTokenBudget
// =========================================================

/// Stage 7: Token 预算管理。
///
/// 职责:
/// - 从 `PipelineData` 读取 system_prompt、memory_context、history_messages、user_input
/// - 通过 `BackendConfig.capability.context_window` 确定预算上限
/// - 调用 `token_budget::apply_token_budget` 做截断
/// - 将结果写入 `PipelineData.budgeted_*` 字段
///
/// 说明:
/// - 上下文窗口来自 backend_config（Stage 2 产出）的 capability.context_window
/// - 若 system_prompt 未设置（Stage 6 失败或未执行），返回 Fatal 错误
/// - 预算超出上下文窗口时记录 warn 日志，不中断管线（LLM 可能仍能处理）
pub struct StageTokenBudget;

impl StageTokenBudget {
    /// 创建 StageTokenBudget 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageTokenBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageTokenBudget {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "TokenBudget"
    }

    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        // 读取前序 Stage 产出
        let system_prompt = input.system_prompt.as_deref().ok_or_else(|| {
            PipelineError::fatal(
                "TokenBudget",
                RamariaError::validation("system_prompt 未设置——Stage 6 (BuildPrompt) 必须在前"),
            )
        })?;

        let backend_config = input.backend_config.as_ref().ok_or_else(|| {
            PipelineError::fatal(
                "TokenBudget",
                RamariaError::validation("backend_config 未设置——Stage 2 (CheckPrivacy) 必须在前"),
            )
        })?;

        // 协调预算（默认关闭）。差异说明见文件头：本 Stage 只执行协调可达成子集
        // （RAG 独立上限/总池裁剪），逐层取舍由生产装配路径（app_chat）完成。
        let coordinated = &ctx.config.injection_budget;
        let mut memory_context = input.memory_context.clone();
        if coordinated.enabled && memory_context.is_some() {
            let alloc = token_budget::allocate_injection_budget(
                &[],
                memory_context.as_deref(),
                coordinated,
            );
            if alloc.memory_context != memory_context {
                tracing::debug!(
                    request_id = %input.request_id,
                    rag_capped = alloc.memory_context.is_some(),
                    "StageTokenBudget: 协调预算已应用 RAG 上限/裁剪"
                );
            }
            memory_context = alloc.memory_context;
        }

        // 上下文窗口来自 provider capability，max_tokens 来自 backend_config
        let context_window = backend_config.capability.context_window as usize;
        let max_output_tokens = backend_config.max_tokens;
        let mut budget_config = TokenBudgetConfig::new(context_window, max_output_tokens);
        if coordinated.enabled {
            // 装配期协调负责 system_prompt 注入总量（本 Stage 无逐层部件），
            // 放开整条句截断，避免对协调结果二次无差别裁剪。
            budget_config.system_prompt_reserve = context_window;
        }

        // 调用 token_budget 模块做截断
        let budgeted = token_budget::apply_token_budget(
            system_prompt,
            memory_context.as_deref(),
            &input.history_messages,
            &input.user_input,
            &budget_config,
        );

        // 超出窗口时 warn（不中断——LLM 服务端可能有不同的 tokenizer 处理）
        if budgeted.estimated_tokens > context_window {
            tracing::warn!(
                request_id = %input.request_id,
                estimated = budgeted.estimated_tokens,
                window = context_window,
                "token 预算超出上下文窗口，可能发生截断"
            );
        }

        tracing::debug!(
            request_id = %input.request_id,
            estimated_tokens = budgeted.estimated_tokens,
            context_window = context_window,
            system_prompt_tokens = token_budget::estimate_tokens(&budgeted.system_prompt),
            history_kept = budgeted.history.len(),
            history_original = input.history_messages.len(),
            has_memory_context = budgeted.memory_context.is_some(),
            "StageTokenBudget: token 预算已应用"
        );

        // 写入 PipelineData
        input.budgeted_system_prompt = Some(budgeted.system_prompt);
        input.budgeted_memory_context = budgeted.memory_context;
        input.budgeted_history = budgeted.history;
        input.estimated_tokens = budgeted.estimated_tokens;

        Ok(input)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::simple_context;
    use ramaria_core::traits::ChatMessage;
    use ramaria_core::types::{
        BackendConfig, LlmProvider as LlmProviderKind, MessageRole, ModelCapability,
    };
    use uuid::Uuid;

    /// 构造含完整前序数据的 PipelineData。
    fn full_data() -> PipelineData {
        let mut data = PipelineData::new(
            "今天天气真好".to_string(),
            Some("rama-0001".to_string()),
            None,
            Uuid::new_v4(),
        );
        // Stage 2 产出
        data.backend_config = Some(BackendConfig {
            provider: LlmProviderKind::LmStudio,
            base_url: "http://localhost:1234/v1".into(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.7,
            max_tokens: 1024,
            capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "test-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
        });
        // Stage 6 产出
        data.system_prompt =
            Some("你是 Ramaria，一个善解人意的 AI 助手。\n请用友好的语气回复。".into());
        // Stage 4 产出
        data.history_messages = vec![
            ChatMessage {
                role: MessageRole::User,
                content: "你好".into(),
            },
            ChatMessage {
                role: MessageRole::Assistant,
                content: "你好！有什么可以帮你的吗？".into(),
            },
        ];
        // Stage 5 产出
        data.memory_context = Some("用户之前讨论过天气相关的话题".into());
        data
    }

    // =========================================================
    // 测试: name
    // =========================================================

    #[test]
    fn stage_name() {
        let stage = StageTokenBudget::new();
        assert_eq!(stage.name(), "TokenBudget");
    }

    // =========================================================
    // 测试: 正常路径——小对话在窗口内
    // =========================================================

    #[tokio::test]
    async fn small_conversation_within_window() {
        let ctx = simple_context();
        let stage = StageTokenBudget::new();
        let data = full_data();

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert!(output.budgeted_system_prompt.is_some());
        assert_eq!(output.budgeted_history.len(), 2, "小对话应全部保留");
        assert!(output.estimated_tokens > 0);
        // token 预算应远小于 4096 窗口
        assert!(
            output.estimated_tokens < 4096,
            "小对话不应超出窗口: estimated={}",
            output.estimated_tokens
        );
    }

    // =========================================================
    // 测试: 关键字段未设置 → Fatal 错误
    // =========================================================

    #[tokio::test]
    async fn missing_field_returns_fatal() {
        let cases: Vec<(&str, fn(&mut PipelineData))> = vec![
            ("system_prompt", |d| d.system_prompt = None), // 未设置 Stage 6 产出
            ("backend_config", |d| d.backend_config = None), // 未设置 Stage 2 产出
        ];
        for (label, mutate) in cases {
            let ctx = simple_context();
            let stage = StageTokenBudget::new();
            let mut data = full_data();
            mutate(&mut data);

            let result = stage.execute(&ctx, data).await;
            match result {
                Ok(_) => panic!("should fail with missing {label}"),
                Err(err) => {
                    assert!(!err.is_retryable(), "missing {label} should be Fatal");
                    assert_eq!(err.stage(), "TokenBudget");
                }
            }
        }
    }

    // =========================================================
    // 测试: memory_context 为 None 时正常处理
    // =========================================================

    #[tokio::test]
    async fn no_memory_context_ok() {
        let ctx = simple_context();
        let stage = StageTokenBudget::new();
        let mut data = full_data();
        data.memory_context = None;

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert!(output.budgeted_memory_context.is_none());
    }

    // =========================================================
    // 测试: 空历史正常处理
    // =========================================================

    #[tokio::test]
    async fn empty_history_ok() {
        let ctx = simple_context();
        let stage = StageTokenBudget::new();
        let mut data = full_data();
        data.history_messages = vec![];

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert!(output.budgeted_history.is_empty());
    }

    // =========================================================
    // 测试: 超长对话历史被截断（验证 budgeted_history < history_messages）
    // =========================================================

    #[tokio::test]
    async fn long_history_truncated() {
        let ctx = simple_context();
        let stage = StageTokenBudget::new();
        let mut data = full_data();

        // 构造极长历史（每条 ~100 中文 chars ≈ 50 tokens，100 条 ≈ 5000 tokens）
        let long_msg = "这是一条非常长的测试消息用于验证token预算管理的截断逻辑。".repeat(10);
        data.history_messages = (0..100)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                content: format!("[{i}] {long_msg}"),
            })
            .collect();
        // 使用小上下文窗口 + 小输出预留强制截断
        let cfg = data.backend_config.as_mut().unwrap();
        cfg.max_tokens = 256; // 留出 (2048-256) = 1792 给 system prompt + 历史
        cfg.capability.context_window = 2048;

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        // 100 条历史应被截断到小于 100 条
        assert!(
            output.budgeted_history.len() < 100,
            "长历史应被截断: kept={}",
            output.budgeted_history.len()
        );
    }

    // =========================================================
    // 测试: estimated_tokens 字段被正确设置
    // （已在 small_conversation_within_window 中断言 estimated_tokens > 0）
    // =========================================================

    // =========================================================
    // 测试: 注入协调预算（[injection_budget]）——Stage 可达子集
    // =========================================================

    /// 构造超长 RAG memory_context（30 个中文字 ≈ 15 token）。
    fn long_memory() -> String {
        "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯".to_string()
    }

    /// 协调启用 + RAG 独立上限：memory_context 被裁剪到 cap 内。
    #[tokio::test]
    async fn coordinated_enabled_caps_memory_context() {
        let mut ctx = simple_context();
        ctx.config.injection_budget.enabled = true;
        ctx.config.injection_budget.max_rag_tokens = 6;
        let stage = StageTokenBudget::new();
        let mut data = full_data();
        data.memory_context = Some(long_memory());

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());
        let output = result.expect("should succeed");

        let rag = output.budgeted_memory_context.expect("RAG 应被保留但截断");
        assert!(
            token_budget::estimate_tokens(&rag) <= 6,
            "RAG 上限裁剪: {}",
            token_budget::estimate_tokens(&rag)
        );
        // 协调启用 → 不再对整条 system_prompt 二次句截断（小 prompt 保持原样）
        assert_eq!(
            output.budgeted_system_prompt.as_deref(),
            output.system_prompt.as_deref(),
            "协调启用时 system_prompt 不做整条二次裁剪"
        );
    }

    /// 协调启用 + 总池小于 RAG：memory_context 被裁剪/丢弃，不超预算。
    #[tokio::test]
    async fn coordinated_enabled_total_pool_constrains_rag() {
        let mut ctx = simple_context();
        ctx.config.injection_budget.enabled = true;
        ctx.config.injection_budget.max_injection_tokens = 5;
        let stage = StageTokenBudget::new();
        let mut data = full_data();
        data.memory_context = Some(long_memory());

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());
        let output = result.expect("should succeed");

        match output.budgeted_memory_context {
            Some(rag) => assert!(
                token_budget::estimate_tokens(&rag) <= 5,
                "总池约束 RAG: {}",
                token_budget::estimate_tokens(&rag)
            ),
            None => {} // RAG 被丢弃也满足"不超预算"
        }
    }

    /// 协调关闭（默认）：memory_context 不做独立 cap，回退既有行为。
    #[tokio::test]
    async fn coordinated_disabled_matches_legacy() {
        let ctx = simple_context(); // injection_budget.enabled 默认 false
        let stage = StageTokenBudget::new();
        let mut data = full_data();
        let memory = long_memory();
        data.memory_context = Some(memory);

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());
        let output = result.expect("should succeed");
        // 大窗口 + 关闭协调 → memory_context 完整保留（既有行为）
        assert_eq!(
            output.budgeted_memory_context.as_deref(),
            Some(long_memory().as_str())
        );
    }
}
