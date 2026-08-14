//! crates/ramaria-app/src/stages/check_state.rs - Stage 1: 应用状态检查
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 1: 状态检查
//! - Ready → 放行（正常对话）
//! - Degraded → 放行（warn 日志，向量通道不可用但 BM25+图谱仍工作）
//! - FatalError → 拒绝（Fatal PipelineError）
//! - 其他状态（NeedsSetup/DownloadingModel/Indexing）→ 拒绝（Fatal PipelineError）
//! - 读取 PipelineData.app_state（由调用方通过 with_app_state 设置）

use async_trait::async_trait;
use ramaria_core::types::AppState;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

/// Stage 1: 应用状态检查。
///
/// 职责:
/// - 验证应用当前状态是否允许对话
/// - 将 app_state 保留在 PipelineData 中供后续 Stage 参考
///
/// 判定规则:
/// - `Ready`: 放行，正常对话
/// - `Degraded`: 放行，记录 warn 日志（向量通道降级，BM25+图谱可用）
/// - `FatalError`: 拒绝，返回 Fatal PipelineError
/// - `NeedsSetup` / `DownloadingModel` / `Indexing`: 拒绝，返回 Fatal PipelineError
pub struct StageCheckState;

impl StageCheckState {
    /// 创建 StageCheckState 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageCheckState {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageCheckState {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "CheckState"
    }

    /// 执行状态检查。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文（本阶段不使用）。
    /// - `input`: 管线数据，须包含 `app_state` 字段。
    ///
    /// 返回:
    /// - `Ok(data)`: 状态检查通过，`data` 原样返回。
    /// - `Err(Fatal)`: 状态不允许对话（FatalError / 未就绪状态）。
    /// - `Err(Fatal)`: `app_state` 为 None（调用方未设置，属于编程错误）。
    async fn execute(
        &self,
        _ctx: &PipelineContext,
        input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        let state = input.app_state.ok_or_else(|| {
            PipelineError::fatal(
                "CheckState",
                ramaria_core::error::RamariaError::validation(
                    "PipelineData.app_state 未设置——调用方必须通过 with_app_state 传入当前状态",
                ),
            )
        })?;

        match state {
            AppState::Ready => {
                tracing::debug!(state = %state, "状态检查通过：Ready");
            }
            AppState::Degraded => {
                tracing::warn!(
                    state = %state,
                    "应用处于降级状态，对话功能可用但向量检索已降级"
                );
            }
            AppState::FatalError => {
                tracing::error!(state = %state, "状态检查失败：应用处于 FatalError 状态");
                return Err(PipelineError::fatal(
                    "CheckState",
                    ramaria_core::error::RamariaError::validation(
                        "应用发生严重错误，请查看日志后重启应用。",
                    ),
                ));
            }
            other => {
                tracing::warn!(state = %other, "状态检查失败：应用尚未就绪");
                return Err(PipelineError::fatal(
                    "CheckState",
                    ramaria_core::error::RamariaError::validation(format!(
                        "应用尚未就绪（当前状态: {other}）。请先完成设置流程。"
                    )),
                ));
            }
        }

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

    fn make_data(state: AppState) -> PipelineData {
        PipelineData::new("test".into(), None, None, uuid::Uuid::new_v4()).with_app_state(state)
    }

    /// Ready / Degraded 状态应通过检查。
    #[tokio::test]
    async fn ready_like_states_pass() {
        for state in [AppState::Ready, AppState::Degraded] {
            let ctx = simple_context();
            let stage = StageCheckState::new();
            let data = make_data(state);
            let result = stage.execute(&ctx, data).await;
            assert!(result.is_ok(), "{state:?} 应通过");
        }
    }

    /// 非 Ready 状态（FatalError / NeedsSetup / DownloadingModel / Indexing）应被拒绝。
    #[tokio::test]
    async fn non_ready_states_rejected() {
        let cases = [
            (AppState::FatalError, "严重错误"),
            (AppState::NeedsSetup, "尚未就绪"),
            (AppState::DownloadingModel, "尚未就绪"),
            (AppState::Indexing, "尚未就绪"),
        ];
        for (state, substr) in cases {
            let ctx = simple_context();
            let stage = StageCheckState::new();
            let data = make_data(state);
            let result = stage.execute(&ctx, data).await;
            let err = match result {
                Ok(_) => panic!("{state:?} should be rejected"),
                Err(e) => e,
            };
            assert!(!err.is_retryable(), "{state:?}");
            assert_eq!(err.stage(), "CheckState", "{state:?}");
            assert!(err.source_error().context().contains(substr), "{state:?}");
        }
    }

    #[tokio::test]
    async fn missing_app_state_returns_fatal() {
        let ctx = simple_context();
        let stage = StageCheckState::new();
        // 故意不调用 with_app_state
        let data = PipelineData::new("test".into(), None, None, uuid::Uuid::new_v4());

        let result = stage.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("missing app_state should fail"),
            Err(e) => e,
        };
        assert!(!err.is_retryable());
        assert_eq!(err.stage(), "CheckState");
        assert!(err.source_error().context().contains("app_state 未设置"));
    }

    #[tokio::test]
    async fn stage_name_is_correct() {
        let stage = StageCheckState::new();
        assert_eq!(stage.name(), "CheckState");
    }
}
