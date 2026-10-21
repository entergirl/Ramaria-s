//! crates/ramaria-service/src/local_image.rs - Ramaria 本地会话发图用例
//!
//! 设计特点:
//! - 仅本地会话：社交通道会话走通道发图链路，本用例拒绝
//! - 图片字节 best-effort 落客户端缓存（数据库同级 `image_cache/`），消息与附件行一并落库
//! - 正文口径与通道图片一致：说明文字与占位符直接拼接；描述留待理解回填
//! - 边界校验：空 / 超限 / 非图片魔数拒绝，不做任何落盘
//! - 隐私：日志只记会话 id 与消息 id，不记说明文字

use md5::{Digest, Md5};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{
    AttachmentStatus, InboundAttachmentKind, Message, MessageAttachment, MessageRole,
    MessageSource, image_placeholder_hash, is_social_channel, now_ms,
};

use crate::engine::Engine;
use crate::types::{LocalImageSendOutcome, LocalImageSendRequest};

/// 单张图片的字节上限（20 MB，与通道图片与表情资产口径一致）。
///
/// 公开给宿主做读取前预检（避免把超大文件整体读入内存后才拒绝）。
pub const MAX_LOCAL_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 本地会话发图（消息与附件行一并落库）。
///
/// 流程:
/// 1. 会话校验（不存在 / 社交通道会话 / 已关闭 → 校验错误）；
/// 2. 字节边界（空 / 超限 / 非图片魔数 → 校验错误）；
/// 3. best-effort 落盘客户端图片缓存；
/// 4. 落库用户消息（正文 = 说明文字 + 图片占位符）；
/// 5. 落库图片附件行（pending，描述留待理解回填）并读回附件 id。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 发图请求（会话 / 文件名 / 字节 / 说明）。
///
/// 返回:
/// - 落库消息与附件行定位、占位符与最终正文。
pub(crate) async fn send(
    engine: &Engine,
    req: LocalImageSendRequest,
) -> RamariaResult<LocalImageSendOutcome> {
    let storage = engine.storage_ref().as_ref();

    // ---- 1. 会话校验 ----
    let Some(session) = storage.get_session(req.session_id).await? else {
        return Err(RamariaError::validation("会话不存在"));
    };
    if is_social_channel(&session.channel) {
        return Err(RamariaError::validation("社交平台会话不支持本地发送图片"));
    }
    if session.ended_at.is_some() {
        return Err(RamariaError::validation("会话已关闭"));
    }

    // ---- 2. 字节边界 ----
    if req.bytes.is_empty() {
        return Err(RamariaError::validation("图片字节不能为空"));
    }
    if req.bytes.len() as u64 > MAX_LOCAL_IMAGE_BYTES {
        return Err(RamariaError::validation("图片超过 20 MB 上限"));
    }
    if crate::vision::image_mime_checked(&req.bytes).is_none() {
        return Err(RamariaError::validation("不是可识别的图片格式"));
    }
    let md5 = md5_hex(&req.bytes);

    // ---- 3. best-effort 落盘客户端图片缓存（命名以 md5 为准）----
    if let Some(parent) = engine.db_path().parent() {
        let cache_dir = parent.join("image_cache");
        crate::attachment_cache::store(&cache_dir, 0, Some(&md5), &req.bytes);
    }

    // ---- 4. 用户消息落库 ----
    let placeholder = format!("[图片#{}]", image_placeholder_hash(&md5));
    let caption = req.caption.as_deref().map(str::trim).unwrap_or("");
    let content = if caption.is_empty() {
        placeholder.clone()
    } else {
        format!("{caption}{placeholder}")
    };
    let mut message = Message::new(
        req.session_id,
        MessageRole::User,
        content.clone(),
        MessageSource::Local,
    );
    if let Some(persona_uid) = session.persona_uid.clone() {
        message = message.with_persona_uid(Some(persona_uid));
    }
    storage.save_message(&message).await?;

    // ---- 5. 附件行落库（pending，待理解回填描述）----
    let now = now_ms();
    let attachment = MessageAttachment {
        id: 0,
        message_id: message.id,
        kind: InboundAttachmentKind::Image,
        source_ref: req.file_name.clone(),
        md5: Some(md5.clone()),
        size: Some(req.bytes.len() as u64),
        width: None,
        height: None,
        sub_type: None,
        status: AttachmentStatus::Pending,
        description: None,
        description_model: None,
        created_at: now,
        updated_at: now,
    };
    storage.insert_message_attachments(&[attachment]).await?;
    let rows = storage.list_attachments_by_messages(&[message.id]).await?;
    let attachment_id = rows
        .first()
        .map(|row| row.id)
        .ok_or_else(|| RamariaError::storage("图片附件写入后读取失败"))?;

    tracing::info!(
        session_id = %req.session_id,
        message_id = %message.id,
        bytes_len = req.bytes.len(),
        "本地会话图片已发送"
    );
    Ok(LocalImageSendOutcome {
        message_id: message.id,
        attachment_id,
        md5,
        placeholder,
        content,
    })
}

/// 计算字节的 md5（小写 hex）。
fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
