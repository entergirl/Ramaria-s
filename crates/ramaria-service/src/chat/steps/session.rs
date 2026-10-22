//! crates/ramaria-service/src/chat/steps/session.rs - Ramaria 会话定位
//!
//! 设计特点:
//! - 交互式按显式会话 > 新建（含新会话桥接注入）
//! - 通道式按显式会话 > 通道 + 外部标识续写；主动模式新建且不注入桥接
//! - 社交会话只读：显式指定社交平台会话的本地生成一律拒绝
//! - 会话 NULL 归属回写失败只记 warn，不阻塞生成

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::is_social_channel;

use crate::engine::Engine;

use super::super::social::readonly_error;
use super::{ChatInput, ChatMode, ResolvedSession, persona_explicit};

pub(in crate::chat) async fn step_resolve_session(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    input: &ChatInput,
    persona: &str,
) -> RamariaResult<ResolvedSession> {
    match input.mode {
        ChatMode::Interactive => match input.session_id {
            Some(sid) => {
                let mut session = storage
                    .get_session(sid)
                    .await?
                    .ok_or_else(|| RamariaError::validation(format!("会话不存在: {sid}")))?;
                // 社交平台会话在客户端本地只读（先于关闭检查）：显式指定即拒绝
                if is_social_channel(&session.channel) {
                    tracing::warn!(
                        session_id = %session.id,
                        channel = %session.channel,
                        "社交平台会话不允许客户端本地生成，拒绝"
                    );
                    return Err(readonly_error(&session.channel));
                }
                if session.ended_at.is_some() {
                    return Err(RamariaError::validation(format!(
                        "会话已关闭（session {sid}），请开启新对话。"
                    )));
                }

                // 存量 NULL 会话归属回写（仅当调用方显式提供人格；失败只 warn 不阻塞）
                if session.persona_uid.is_none() && persona_explicit(input.persona.as_deref()) {
                    match storage.bind_session_persona_uid(session.id, persona).await {
                        Ok(()) => {
                            session.persona_uid = Some(persona.to_string());
                            tracing::info!(
                                session_id = %session.id,
                                persona = %persona,
                                "会话 NULL 归属已回写绑定"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                session_id = %session.id,
                                error = %e,
                                "会话 NULL 归属回写失败（不阻塞本轮生成）"
                            );
                        }
                    }
                }

                // 会话归属与请求人格不一致时只记日志（uid 级，不含消息内容）
                if let Some(bound) = session.persona_uid.as_deref() {
                    if bound != persona {
                        tracing::warn!(
                            session_id = %session.id,
                            bound_persona = %bound,
                            requested_persona = %persona,
                            "会话归属与请求人格不一致，按会话绑定人格继续生成"
                        );
                    }
                }

                // 会话已绑定人格 → 覆盖本轮生效人格（避免跨人格上下文串用）
                let effective = session
                    .persona_uid
                    .clone()
                    .unwrap_or_else(|| persona.to_string());
                Ok(ResolvedSession {
                    session,
                    persona: effective,
                    bridge_context: None,
                })
            }
            None => {
                let session = storage
                    .create_session(Some(persona))
                    .await
                    .map_err(|e| RamariaError::storage_with_source("创建 session 失败", e))?;
                tracing::info!(session_id = %session.id, "交互式生成新建会话");

                // 新会话桥接：最近一个已关闭会话的尾部原文（两级降级，失败跳过）
                let bridge_context = if config.injection.bridge {
                    let bridge = crate::bridge::load_bridge_context(
                        storage,
                        &config.bridge,
                        &config.utt,
                        Some(persona),
                    )
                    .await;
                    bridge.content
                } else {
                    tracing::debug!("桥接注入闸门关闭，跳过桥接加载");
                    None
                };

                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context,
                })
            }
        },
        ChatMode::Channel => {
            if let Some(id) = input.session_id {
                let session = storage
                    .get_session(id)
                    .await?
                    .ok_or_else(|| RamariaError::validation(format!("会话不存在：{id}")))?;
                // 通道入站回复路径（message_in_history=true，入站消息已落库）是唯一允许写
                // 社交会话的本地链路；其余显式指定社交会话的调用（如 MCP chat_send）一律拒绝
                if is_social_channel(&session.channel) && !input.message_in_history {
                    tracing::warn!(
                        session_id = %session.id,
                        channel = %session.channel,
                        "社交平台会话不允许绕过入站回复路径生成，拒绝"
                    );
                    return Err(readonly_error(&session.channel));
                }
                if session.ended_at.is_some() {
                    return Err(RamariaError::validation(format!(
                        "会话已关闭：{id}（请省略 session_id 由服务端续写或另起）"
                    )));
                }
                if let Some(uid) = session.persona_uid.as_deref() {
                    if uid != persona {
                        return Err(RamariaError::validation(format!(
                            "会话归属人格为 {uid}，与请求人格 {persona} 不一致"
                        )));
                    }
                }
                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context: None,
                })
            } else {
                let session = crate::ingest::resolve_session(
                    engine,
                    &input.channel,
                    input.conversation_id.as_deref(),
                    persona,
                )
                .await?;
                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context: None,
                })
            }
        }
        ChatMode::Proactive => match input.session_id {
            Some(sid) => {
                let session = storage
                    .get_session(sid)
                    .await?
                    .ok_or_else(|| RamariaError::validation(format!("会话不存在: {sid}")))?;
                if session.ended_at.is_some() {
                    return Err(RamariaError::validation(format!(
                        "会话已关闭（session {sid}）"
                    )));
                }
                // 主动生成不改写人格归属：会话已绑定他人格时拒绝（调用方预检应已剔除）
                if let Some(uid) = session.persona_uid.as_deref() {
                    if uid != persona {
                        return Err(RamariaError::validation(format!(
                            "会话归属人格为 {uid}，与主动生成人格 {persona} 不一致"
                        )));
                    }
                }
                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context: None,
                })
            }
            None => {
                // 主动开口的话题由选题锚点承载：新建会话不注入上一会话桥接
                let session = storage
                    .create_session(Some(persona))
                    .await
                    .map_err(|e| RamariaError::storage_with_source("创建 session 失败", e))?;
                tracing::info!(session_id = %session.id, "主动生成新建会话");
                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context: None,
                })
            }
        },
    }
}
