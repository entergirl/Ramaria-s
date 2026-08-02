//! rust/crates/ramaria-app/src/stages/check_privacy.rs - Stage 2: 隐私确认检查
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 2: 隐私确认
//! - 线上 provider（DeepSeek/OpenAI）→ 强制检查隐私确认状态
//! - 本地 provider（LM Studio）→ 跳过检查
//! - 未确认时返回 Retryable PipelineError（用户可确认后重试）
//! - 将 BackendConfig 写入 PipelineData 供后续 Stage 使用

use async_trait::async_trait;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};
use crate::privacy;

/// Stage 2: 隐私确认检查。
///
/// 职责:
/// - 读取 LLM provider 配置，判断是否为线上 provider
/// - 线上 provider 强制查询隐私确认状态
/// - 未确认时返回 Retryable 错误（用户完成确认后可重试管线）
/// - 将 BackendConfig 写入 PipelineData 供 TokenBudget / BuildRequest 等后续 Stage 使用
///
/// 降级策略:
/// - 本地 provider（LM Studio）→ 跳过隐私检查，直接通过
/// - 线上 provider 已确认 → 通过
/// - 线上 provider 未确认 → Retryable 错误
pub struct StageCheckPrivacy;

impl StageCheckPrivacy {
    /// 创建 StageCheckPrivacy 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageCheckPrivacy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageCheckPrivacy {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "CheckPrivacy"
    }

    /// 执行隐私确认检查。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文（读取 `ctx.llm.config()` 获取 provider 信息）。
    /// - `input`: 管线数据。
    ///
    /// 返回:
    /// - `Ok(data)`: 隐私检查通过（本地 provider 或已确认），`data.backend_config` 已填充。
    /// - `Err(Retryable)`: 线上 provider 未完成隐私确认。
    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        let cfg = ctx.llm.config().clone();

        if cfg.provider.is_online() {
            tracing::debug!(
                provider = %cfg.provider,
                base_url = %cfg.base_url,
                "线上 provider，检查隐私确认状态"
            );

            privacy::require_privacy(ctx.storage.as_ref(), cfg.provider, &cfg.base_url)
                .await
                .map_err(|e| {
                    tracing::warn!(
                        provider = %cfg.provider,
                        %e,
                        "隐私确认未完成，管线中止（Retryable）"
                    );
                    PipelineError::retryable("CheckPrivacy", e)
                })?;
        } else {
            tracing::debug!("本地 provider，跳过隐私确认");
        }

        // 将后端配置写入 PipelineData 供后续 Stage 使用
        input.backend_config = Some(cfg);

        Ok(input)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockLlm, MockStorage, test_context};
    use ramaria_core::types::{LlmProvider, PrivacyConsent};
    use std::sync::Arc;

    fn make_data() -> PipelineData {
        PipelineData::new("test".into(), None, None, uuid::Uuid::new_v4())
            .with_app_state(AppState::Ready)
    }

    use ramaria_core::types::AppState;

    /// 本地 provider 跳过隐私检查，且 backend_config 字段被正确填充。
    #[tokio::test]
    async fn local_provider_skips_privacy() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageCheckPrivacy::new();
        let data = make_data();

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("local provider should pass");
        assert!(output.backend_config.is_some());
        assert_eq!(
            output.backend_config.as_ref().unwrap().provider,
            LlmProvider::LmStudio
        );
        let cfg = output.backend_config.expect("backend_config should be set");
        assert_eq!(cfg.temperature, 0.3);
        assert_eq!(cfg.max_tokens, 1024);
    }

    /// 线上 provider：无 consent → Retryable；有 consent → 通过。
    #[tokio::test]
    async fn online_provider_consent_cases() {
        for has_consent in [false, true] {
            let storage = Arc::new(MockStorage::new());
            if has_consent {
                storage.add_privacy_consent(PrivacyConsent::new(
                    LlmProvider::DeepSeek,
                    "https://api.deepseek.com/v1".to_string(),
                    true,
                ));
            }

            let ctx = test_context(storage, Arc::new(MockLlm::online_deepseek()), None);
            let stage = StageCheckPrivacy::new();
            let data = make_data();

            let result = stage.execute(&ctx, data).await;
            if has_consent {
                let output = result.expect("online provider with consent should pass");
                assert_eq!(
                    output.backend_config.as_ref().unwrap().provider,
                    LlmProvider::DeepSeek
                );
            } else {
                let err = match result {
                    Ok(_) => panic!("online provider without consent should fail"),
                    Err(e) => e,
                };
                assert!(err.is_retryable());
                assert_eq!(err.stage(), "CheckPrivacy");
            }
        }
    }

    #[tokio::test]
    async fn stage_name_is_correct() {
        let stage = StageCheckPrivacy::new();
        assert_eq!(stage.name(), "CheckPrivacy");
    }
}
