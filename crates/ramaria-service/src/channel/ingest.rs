//! crates/ramaria-service/src/channel/ingest.rs - Ramaria 社交通道入站写入用例
//!
//! 设计特点:
//! - 幂等单点：平台消息 ID 优先指纹；无平台 ID 时按内容事件指纹；重复事件零重复入库
//! - 会话解析独立口径：`(channel, external_ref)` 单活跃会话；改绑更新归属、
//!   空闲先封存再另起（不复用面向「一会话一人格」的回流解析）
//! - 发言人归属：本机侧（self_id 命中）为绑定人格 + assistant；
//!   人类成员挂 `char-{账号}` 锚点 + user（跨会话归一）
//! - 附件行落库后按插入序返回定位（供理解回填）；成员 upsert 失败不阻塞
//! - 日志经 `mask_id` 脱敏，不携带消息内容

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::types::{
    AttachmentStatus, InboundAttachmentKind, InboundAttachmentRef, InboundMessage, Message,
    MessageAttachment, MessageRole, MessageSource, Persona, PersonaKind, SessionMember, now_ms,
};
use sha2::{Digest, Sha256};

use crate::engine::Engine;
use crate::types::{ChannelIngestOutcome, ChannelIngestRequest, ChannelIngestedImage};

// =========================================================
// 用例入口
// =========================================================

/// 执行社交通道入站写入用例。
///
/// 流程:
/// 1. 参数校验（通道 / 外部标识 / 人格非空；人格白名单与存在性）；
/// 2. 指纹幂等（平台消息 ID 优先；命中直接返回未写入）；
/// 3. 会话解析（续写 / 改绑 / 空闲先封存再另起）；
/// 4. 发言人归属（本机侧 assistant / 人类成员 char 锚点 + user）；
/// 5. 消息落库 + 附件行落库（返回定位）+ 成员 upsert（失败不阻塞）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 入站写入请求（通道 / 外部标识 / 本机侧人格 / 消息）。
///
/// 返回:
/// - `session_id` / `written` / 图片附件定位。
pub(crate) async fn run(
    engine: &Engine,
    req: ChannelIngestRequest,
) -> RamariaResult<ChannelIngestOutcome> {
    // ---- 1. 参数校验 ----
    let channel = validate_required(&req.channel, "通道标识不能为空")?.to_string();
    let external_ref = validate_required(&req.external_ref, "会话外部标识不能为空")?.to_string();
    let persona = validate_required(&req.persona_uid, "人格标识不能为空")?.to_string();

    let policy = engine.recall_policy();
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "社交通道消息的人格不在可写白名单内，拒绝写入");
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可写白名单内（allowed_personas，与可见性共用同一白名单）"
        )));
    }
    if engine
        .storage_ref()
        .get_persona_by_uid(&persona)
        .await?
        .is_none()
    {
        return Err(RamariaError::validation(format!(
            "人格不存在: {persona}（请确认通道绑定人格或通道级默认人格）"
        )));
    }

    let text = req.message.text.trim().to_string();
    if text.is_empty() {
        return Err(RamariaError::validation("消息内容不能为空"));
    }

    let storage = engine.storage_ref().as_ref();

    // ---- 2. 指纹幂等 ----
    let fingerprint = channel_fingerprint(&channel, &external_ref, &req.message);
    match storage.find_message_by_fingerprint(&fingerprint).await {
        Ok(Some(existing)) => {
            tracing::debug!(
                session_id = %existing.session_id,
                "社交通道消息指纹已存在（重复事件），跳过写入"
            );
            return Ok(ChannelIngestOutcome {
                session_id: existing.session_id,
                message_id: existing.id,
                written: false,
                images: Vec::new(),
            });
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "社交通道指纹查重失败，按未命中处理（可能重复写入）");
        }
    }

    // ---- 3. 会话解析 ----
    let session = resolve_session_for_channel(engine, &channel, &external_ref, &persona).await?;

    // ---- 4. 发言人归属 ----
    let sender = &req.message.sender;
    let self_id = req
        .self_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let sender_account = sender
        .uin
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| sender.platform_id.trim());
    let is_self = self_id
        .is_some_and(|self_id| sender_account == self_id || sender.platform_id.trim() == self_id);

    let (msg_persona, role, member) = if is_self {
        // 本机侧（bot 身份）：归属绑定人格，角色为 assistant，不建成员锚点
        (persona.clone(), MessageRole::Assistant, None)
    } else {
        if sender_account.is_empty() {
            return Err(RamariaError::validation("消息缺少发送者标识"));
        }
        let anchor =
            ensure_char_persona(engine, &channel, sender_account, &sender.display_name).await?;
        let member = SessionMember {
            session_id: session.id,
            platform_ref: sender_account.to_string(),
            name: sender.display_name.trim().to_string(),
            group_nickname: sender.group_nickname.clone(),
            role: sender.role,
            first_seen_at: req.message.created_at,
            last_seen_at: req.message.created_at,
        };
        (anchor, MessageRole::User, Some(member))
    };

    // ---- 5. 消息落库 ----
    let mut message = Message::new(session.id, role, text, MessageSource::Local)
        .with_persona_uid(Some(msg_persona));
    message.created_at = req.message.created_at;
    message.fingerprint = Some(fingerprint);
    message.sender_ref = optional_trimmed(&sender.platform_id);
    message.sender_name = optional_trimmed(&sender.display_name);
    message.platform_message_id = req
        .message
        .platform_message_id
        .as_deref()
        .and_then(optional_trimmed);
    storage.save_message(&message).await?;

    // ---- 6. 附件行（图片） ----
    let images = persist_image_attachments(storage, message.id, &req.message.attachments).await?;

    // ---- 7. 成员 upsert（仅人类消息；失败不阻塞） ----
    if let Some(member) = member {
        if let Err(e) = storage.upsert_session_members(&[member]).await {
            tracing::warn!(session_id = %session.id, error = %e, "社交通道成员写入失败（不阻塞）");
        }
    }

    tracing::info!(
        session_id = %session.id,
        channel,
        written = true,
        images = images.len(),
        "社交通道消息写入完成"
    );
    tracing::debug!(
        session_id = %session.id,
        external_ref = %mask_id(&external_ref),
        "社交通道写入的会话标识（debug 级，避免 info 日志携带可识别信息）"
    );

    Ok(ChannelIngestOutcome {
        session_id: session.id,
        message_id: message.id,
        written: true,
        images,
    })
}

// =========================================================
// 会话解析
// =========================================================

/// 解析目标会话：续写已有活跃会话（含改绑与空闲封存体检），或新建。
///
/// 规则:
/// 1. 按 `(channel, external_ref)` 查活跃会话；
/// 2. 命中且空闲超阈值：允许封存时先封存（生成摘要）再另起；禁用封存时续写原会话；
/// 3. 命中且未空闲：会话归属与绑定人格不一致时改绑（群聊多人共用一个会话语义）；
/// 4. 未命中 → 新建带通道标识的会话。
async fn resolve_session_for_channel(
    engine: &Engine,
    channel: &str,
    external_ref: &str,
    persona: &str,
) -> RamariaResult<ramaria_core::types::Session> {
    let storage = engine.storage_ref().as_ref();

    if let Some(mut existing) = storage
        .find_active_session_by_channel(channel, Some(external_ref))
        .await?
    {
        if crate::ingest::session_idle(engine, existing.id).await? {
            if !engine.seal_allowed() {
                tracing::info!(
                    session_id = %existing.id,
                    "社交通道会话空闲超阈值但封存已禁用：续写原会话（不另起）"
                );
                return Ok(existing);
            }
            tracing::info!(
                session_id = %existing.id,
                "社交通道会话空闲超阈值，先封存再另起"
            );
            if let Err(e) = crate::seal::run(engine, existing.id).await {
                tracing::error!(
                    session_id = %existing.id,
                    error = %e,
                    "社交通道惰性封存失败，继续另起新会话写入（摘要待空闲检查/补扫重试）"
                );
            }
        } else {
            let needs_rebind = existing
                .persona_uid
                .as_deref()
                .is_some_and(|uid| uid != persona);
            if needs_rebind {
                match storage.bind_session_persona_uid(existing.id, persona).await {
                    Ok(()) => {
                        tracing::info!(
                            session_id = %existing.id,
                            persona = %mask_id(persona),
                            "改绑后会话归属已更新"
                        );
                        existing.persona_uid = Some(persona.to_string());
                    }
                    Err(e) => {
                        tracing::warn!(
                            session_id = %existing.id,
                            error = %e,
                            "会话归属改绑失败（不阻塞写入）"
                        );
                    }
                }
            }
            tracing::debug!(session_id = %existing.id, "续写已有社交通道会话");
            return Ok(existing);
        }
    }

    let session = storage
        .create_session_in_channel(Some(persona), channel, Some(external_ref))
        .await?;
    tracing::info!(
        session_id = %session.id,
        channel,
        external_ref = %mask_id(external_ref),
        persona = %mask_id(persona),
        "已创建社交通道会话"
    );
    Ok(session)
}

/// 人类成员人格锚点：`char-{账号}` 命中复用；未命中创建（`source` = 通道）。
///
/// 返回:
/// - 锚点人格 uid；创建失败上抛（可见错误，不做静默降级）。
async fn ensure_char_persona(
    engine: &Engine,
    channel: &str,
    account: &str,
    display_name: &str,
) -> RamariaResult<String> {
    let storage = engine.storage_ref().as_ref();
    let anchor_uid = format!("char-{account}");
    if storage.get_persona_by_uid(&anchor_uid).await?.is_some() {
        return Ok(anchor_uid);
    }

    let name = {
        let trimmed = display_name.trim();
        if trimmed.is_empty() {
            account.to_string()
        } else {
            trimmed.to_string()
        }
    };
    let seq = storage
        .list_personas()
        .await?
        .iter()
        .filter(|persona| persona.source == channel)
        .map(|persona| persona.seq)
        .max()
        .unwrap_or(0)
        + 1;
    let mut persona = Persona::new(
        anchor_uid.clone(),
        name,
        PersonaKind::Char,
        seq,
        channel.to_string(),
    );
    persona.ref_id = Some(account.to_string());
    storage.create_persona(&persona).await?;
    tracing::info!(persona_uid = %mask_id(&anchor_uid), channel, "已创建社交通道成员人格锚点");
    Ok(anchor_uid)
}

// =========================================================
// 附件与指纹
// =========================================================

/// 落库图片附件行并按插入序返回定位（返回序与附件引用序一致）。
async fn persist_image_attachments(
    storage: &dyn ramaria_core::traits::StorageBackend,
    message_id: uuid::Uuid,
    references: &[InboundAttachmentRef],
) -> RamariaResult<Vec<ChannelIngestedImage>> {
    let image_refs: Vec<&InboundAttachmentRef> = references
        .iter()
        .filter(|reference| reference.kind == InboundAttachmentKind::Image)
        .collect();
    if image_refs.is_empty() {
        return Ok(Vec::new());
    }

    let now = now_ms();
    let rows: Vec<MessageAttachment> = image_refs
        .iter()
        .map(|reference| MessageAttachment {
            id: 0,
            message_id,
            kind: reference.kind,
            source_ref: reference.source_ref.clone(),
            md5: reference.md5.clone(),
            size: reference.size,
            width: reference.width,
            height: reference.height,
            sub_type: reference.sub_type.clone(),
            status: AttachmentStatus::Pending,
            description: None,
            description_model: None,
            created_at: now,
            updated_at: now,
        })
        .collect();
    storage.insert_message_attachments(&rows).await?;

    let stored = storage.list_attachments_by_messages(&[message_id]).await?;
    Ok(stored
        .into_iter()
        .map(|row| ChannelIngestedImage {
            attachment_id: row.id,
            source_ref: row.source_ref,
        })
        .collect())
}

/// 事件指纹（SHA-256 前 8 字节 → 16 位 hex）。
///
/// 组成:
/// - 平台消息 ID 存在：`platform:{channel}:{message_id}`（同平台消息跨会话稳定）；
/// - 缺失：`platform:{channel}:{external_ref}:{created_at}:{发送账号}:{正文}`（事件级兜底）。
pub(crate) fn channel_fingerprint(
    channel: &str,
    external_ref: &str,
    message: &InboundMessage,
) -> String {
    let platform_id = message
        .platform_message_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let material = match platform_id {
        Some(message_id) => format!("platform:{channel}:{message_id}"),
        None => {
            let sender = message
                .sender
                .uin
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(message.sender.platform_id.trim());
            format!(
                "platform:{channel}:{external_ref}:{}:{sender}:{}",
                message.created_at,
                message.text.trim()
            )
        }
    };
    let mut hasher = Sha256::new();
    hasher.update(material.as_bytes());
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// =========================================================
// 校验辅助
// =========================================================

/// 校验必填文本（trim 后须非空）；返回 trim 后的切片。
fn validate_required<'a>(value: &'a str, message: &str) -> RamariaResult<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RamariaError::validation(message));
    }
    Ok(trimmed)
}

/// trim 后非空转 String，否则 None。
fn optional_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
