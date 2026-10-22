//! crates/ramaria-service/src/chat/steps/feedback.rs - Ramaria 弱反馈检测与主动提示词上下文
//!
//! 设计特点:
//! - 弱反馈检测失败只记 warn，不阻塞生成
//! - 主动模式的请求上下文由选题锚点派生，用户消息位留空

use chrono::Timelike;

use ramaria_core::config::RamariaConfig;
use ramaria_core::time_period::TimePeriod;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, MessageRole, MessageSource};
use ramaria_memory::prompt::builder::ProactivePromptContext;
use uuid::Uuid;

use super::ChatInput;

pub(super) async fn step_detect_feedback(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    session_id: Uuid,
    persona: &str,
    message: &str,
    user_persisted: bool,
) {
    if !config.feedback.enabled {
        return;
    }

    let mut recent = match storage.list_messages(session_id).await {
        Ok(messages) => messages,
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "读取会话消息用于弱反馈检测失败，跳过");
            Vec::new()
        }
    };
    // 本轮消息尚未落库时追加为检测输入最后一条（时间戳取当前，供间隔判定）；
    // 已落库时列表中已含该条，不重复追加
    if !user_persisted {
        recent.push(Message::new(
            session_id,
            MessageRole::User,
            message.to_string(),
            MessageSource::Local,
        ));
    }

    if let Err(e) = crate::feedback::process_feedback_for_new_message(
        storage,
        &config.feedback,
        session_id,
        Some(persona),
        &recent,
    )
    .await
    {
        tracing::warn!(session_id = %session_id, error = %e, "弱反馈信号处理失败（不阻塞对话主流程）");
    }
}

/// 构造主动生成的提示词上下文（时段取本地当前时间）。
pub(super) fn build_proactive_prompt(input: &ChatInput) -> Option<ProactivePromptContext> {
    let proactive = input.proactive.as_ref()?;
    Some(ProactivePromptContext {
        time_period: TimePeriod::from_local_hour(chrono::Local::now().hour()),
        anchor: proactive.anchor.clone(),
        angle: proactive.angle.clone(),
        tone: proactive.tone.clone(),
    })
}
