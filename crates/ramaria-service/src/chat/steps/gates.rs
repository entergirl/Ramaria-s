//! crates/ramaria-service/src/chat/steps/gates.rs - Ramaria 生成前置门禁
//!
//! 设计特点:
//! - 状态门禁与线上 provider 隐私确认仅对交互式入口生效
//! - 通道式入口不受门禁约束，主动模式由生成用例预检后静默处理

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{AppState, BackendConfig};

use crate::engine::Engine;

/// 校验消息与目标人格（未指定人格时取生效默认人格）。
pub(super) async fn step_validate<'a>(
    engine: &Engine,
    message: &'a str,
    persona: Option<&str>,
) -> RamariaResult<(&'a str, String)> {
    let message = message.trim();
    if message.is_empty() {
        return Err(RamariaError::validation("消息不能为空"));
    }

    let persona = match persona.map(str::trim).filter(|value| !value.is_empty()) {
        Some(uid) => uid.to_string(),
        None => engine.default_persona_uid().await,
    };
    let policy = engine.recall_policy();
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "生成请求的人格不在可见白名单内，拒绝");
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可见白名单内（allowed_personas）"
        )));
    }

    Ok((message, persona))
}

/// 步骤 2（仅交互式）：应用状态门禁（`Ready` / `Degraded` 放行，其余拒绝）。
pub(super) fn step_check_state(engine: &Engine) -> RamariaResult<()> {
    match engine.current_state() {
        AppState::Ready => {
            tracing::debug!(state = %AppState::Ready, "状态检查通过：Ready");
            Ok(())
        }
        AppState::Degraded => {
            tracing::warn!(
                state = %AppState::Degraded,
                "应用处于降级状态，对话功能可用但向量检索已降级"
            );
            Ok(())
        }
        AppState::FatalError => {
            tracing::error!(state = %AppState::FatalError, "状态检查失败：应用处于 FatalError 状态");
            Err(RamariaError::validation(
                "应用发生严重错误，请查看日志后重启应用。",
            ))
        }
        other => {
            tracing::warn!(state = %other, "状态检查失败：应用尚未就绪");
            Err(RamariaError::validation(format!(
                "应用尚未就绪（当前状态: {other}）。请先完成设置流程。"
            )))
        }
    }
}

/// 步骤 3a：读取当前 LLM 后端配置快照（Token 预算与请求参数来源）。
pub(super) fn step_backend_config(engine: &Engine) -> BackendConfig {
    engine.llm_ref().config().clone()
}

/// 步骤 3b（仅交互式）：线上 provider 的隐私确认门禁。
pub(super) async fn step_check_privacy(
    engine: &Engine,
    backend: &BackendConfig,
) -> RamariaResult<()> {
    if backend.provider.is_online() {
        crate::privacy::require_privacy(
            engine.storage_ref().as_ref(),
            backend.provider,
            &backend.base_url,
        )
        .await?;
    }
    Ok(())
}
