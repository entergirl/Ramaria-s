//! crates/ramaria-service/src/ingest.rs - 回流写入用例（chat_ingest 的服务层实现）
//!
//! 设计特点:
//! - 会话三态解析：显式外部对话标识（`conversation_id`）> 单流退化（同通道无标识流）>
//!   新建会话；命中他人格占用的外部标识时不续写（另起，避免串人格）
//! - 惰性封存体检：续写前检查该会话最后消息距今是否超过 `[session].l1_idle_minutes`，
//!   超过则先封存（生成 L1）再另起，覆盖"进程刚启动 / 客户端跨天续写"的空档
//! - 重复提交安全（两层，覆盖整段对话、不受读取窗口限制）：
//!   1. 重发前缀跳过——与库内该对话消息尾部做后缀匹配，已入库的前缀不再重复写入；
//!   2. 指纹去重——每条消息带确定性指纹（对话标识 + 角色 + 内容 + 出现序数），
//!      入库前查库跳过（跨会话、跨进程均生效）
//! - 去重依据取该对话**全部**消息键（`list_message_keys_by_channel_ref`，仅两列）：
//!   长对话（数千条）也能精确计算出现序数，避免"窗口截断导致序数失准 → 重复写入"
//! - 写入通道标识：新会话落 `channel` / `external_ref`，桌面可据此标注来源
//! - 边界：空 messages / 空内容 / 越权人格显式报错或跳过（不静默写脏数据）；
//!   惰性封存失败不阻塞写入（与 `finalize` 同口径）

use std::collections::HashMap;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageKey, MessageSource, Session, now_ms};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{CHANNEL_MCP, ChatTurn, DEFAULT_PERSONA_UID, IngestOutcome, IngestRequest};

/// 兜底通道：请求未带通道时归入 MCP 通道。
fn normalize_channel(channel: &str) -> String {
    let trimmed = channel.trim();
    if trimmed.is_empty() {
        CHANNEL_MCP.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 归一化外部对话标识（空白视为未提供，进入单流退化）。
fn normalize_external_ref(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// =========================================================
// 用例入口
// =========================================================

/// 执行回流写入用例。
///
/// 流程:
/// 1. 入参边界校验（messages 非空）与人格白名单校验；
/// 2. 会话解析 + 惰性封存体检（命中超时会话则先封存再另起）；
/// 3. 重发前缀跳过 + 逐条指纹去重 + 落库（`channel` / `external_ref` 已写入会话）；
/// 4. `finalize=true` 时立即封存（触发 L1 与后续加工）；封存失败不回滚写入
///    （记 error + `finalized=false`，摘要留待空闲检查或补扫重试）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 回流请求（消息 / 人格 / 外部对话标识 / 通道 / 是否收尾）。
///
/// 返回:
/// - `session_id`（消息落库目标会话）、`written`、`deduplicated`、`finalized`。
pub(crate) async fn run(engine: &Engine, req: IngestRequest) -> RamariaResult<IngestOutcome> {
    if req.messages.is_empty() {
        return Err(RamariaError::validation("messages 不能为空"));
    }

    let policy = engine.recall_policy();
    let persona = req
        .persona
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_PERSONA_UID)
        .to_string();
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "回流请求的人格不在可写白名单内，拒绝写入");
        // 文案区分读/写语义：白名单同时约束"可见"与"可写"，此处为写入侧拒绝
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可写白名单内（allowed_personas，与可见性共用同一白名单）"
        )));
    }

    // 归属人格必须已存在（messages.persona_uid 有外键约束）：
    // 显式报错优于让写入以底层 FOREIGN KEY 失败收场，也避免留下空会话残留
    if engine
        .storage_ref()
        .get_persona_by_uid(&persona)
        .await?
        .is_none()
    {
        return Err(RamariaError::validation(format!(
            "人格不存在: {persona}（请先在 Ramaria 完成初始化或改用已存在的人格）"
        )));
    }

    let channel = normalize_channel(&req.channel);
    let external_ref = normalize_external_ref(req.conversation_id.as_deref());
    let storage = engine.storage_ref().as_ref();

    // ---- 1. 会话解析（含惰性封存体检） ----
    let session = resolve_session(engine, &channel, external_ref.as_deref(), &persona).await?;

    // ---- 2. 重发前缀跳过 + 指纹去重 + 落库 ----
    // 去重依据 = 该对话**全部**消息键（跨会话、仅 role + trim 正文两列）：
    // 长对话也能精确计算出现序数，不因读取窗口截断而误判重复。
    let history = match storage
        .list_message_keys_by_channel_ref(&channel, external_ref.as_deref())
        .await
    {
        Ok(keys) => keys,
        Err(e) => {
            // 读取失败不阻塞写入：退化为"无历史"（可能重复写入，但不丢数据）
            tracing::warn!(error = %e, "读取该对话历史失败，本次按无历史处理");
            Vec::new()
        }
    };
    let skip = suffix_match_len(&history, &req.messages);
    let mut ordinals = occurrence_counts(&history);
    let fingerprint_key = external_ref.as_deref().unwrap_or(persona.as_str());

    let mut written = 0usize;
    let mut deduplicated = skip;
    let base_ts = now_ms();

    for (index, turn) in req.messages.iter().enumerate().skip(skip) {
        let content = turn.content.trim();
        if content.is_empty() {
            tracing::warn!(index, "回流消息内容为空，跳过该条");
            continue;
        }

        // 序数：同一 (角色, 内容) 在该对话内的第几次出现（跨进程 / 跨会话稳定）
        let key = (turn.role.as_str().to_string(), content.to_string());
        let ordinal = {
            let counter = ordinals.entry(key).or_insert(0);
            *counter += 1;
            *counter
        };
        let fingerprint = ingest_fingerprint(fingerprint_key, turn.role.as_str(), content, ordinal);

        // 指纹去重（防御：跨会话 / 跨进程重复提交）
        match storage.find_message_by_fingerprint(&fingerprint).await {
            Ok(Some(existing)) => {
                tracing::debug!(
                    existing_id = %existing.id,
                    "回流消息指纹已存在，跳过（重复提交）"
                );
                deduplicated += 1;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "指纹查重失败，按未命中处理（可能重复写入）");
            }
        }

        // 逐条落库：created_at 按提交顺序单调递增，保证会话内顺序稳定
        let mut message = Message::new(
            session.id,
            turn.role.into(),
            content.to_string(),
            MessageSource::Local,
        )
        .with_persona_uid(Some(persona.clone()));
        message.created_at = base_ts.saturating_add(index as i64);
        message.fingerprint = Some(fingerprint);

        storage.save_message(&message).await?;
        written += 1;
    }

    // external_ref 可能含可识别信息（客户端自报标识），日志降级为 debug
    tracing::info!(
        session_id = %session.id,
        channel = %channel,
        has_external_ref = external_ref.is_some(),
        written,
        deduplicated,
        "回流消息写入完成"
    );
    tracing::debug!(
        session_id = %session.id,
        external_ref = external_ref.as_deref().unwrap_or("none"),
        "回流写入的对话标识（debug 级，避免 info 日志携带可识别信息）"
    );

    // ---- 3. 收尾封存（finalize） ----
    // 封存失败不改变写入结果：消息已落库（主交付），摘要留待空闲检查或补扫生成；
    // 此处记 error + `finalized=false` 让调用方看到"已写入但未摘要"，而非整调用失败。
    let finalized = if req.finalize {
        match crate::seal::run(engine, session.id).await {
            Ok(outcome) => outcome.sealed,
            Err(e) => {
                tracing::error!(
                    session_id = %session.id,
                    error = %e,
                    "finalize 封存失败（消息已落库，摘要将由空闲检查/补扫重试）"
                );
                false
            }
        }
    } else {
        false
    };

    Ok(IngestOutcome {
        session_id: session.id,
        written,
        deduplicated,
        finalized,
    })
}

// =========================================================
// 会话解析
// =========================================================

/// 解析目标会话：续写已有活跃会话，或新建会话（含惰性封存体检）。
///
/// 规则:
/// 1. 按 `(channel, external_ref)` 查活跃会话；
/// 2. 命中且归属人格一致（或会话未绑定）→ 检查空闲：
///    - 超 `[session].l1_idle_minutes` → 先封存（生成 L1）再新建；
///    - 未超 → 直接续写；
/// 3. 命中但归属他人格 → 不续写（另起，避免串人格）；
/// 4. 未命中 → 新建带通道标识的会话。
///
/// 用法:
/// - 回流写入（`ingest`）与生成（`chat`）共用同一会话解析口径，保证两个入口
///   对"同一外部标识是否续写"的判断一致。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `channel`: 来源通道。
/// - `external_ref`: 外部对话标识（None = 单流退化）。
/// - `persona`: 归属人格。
pub(crate) async fn resolve_session(
    engine: &Engine,
    channel: &str,
    external_ref: Option<&str>,
    persona: &str,
) -> RamariaResult<Session> {
    let storage = engine.storage_ref().as_ref();

    if let Some(existing) = storage
        .find_active_session_by_channel(channel, external_ref)
        .await?
    {
        let same_persona = existing
            .persona_uid
            .as_deref()
            .map(|uid| uid == persona)
            .unwrap_or(true);
        if same_persona {
            if session_idle(engine, existing.id).await? {
                // 封存门禁（D-V21-009 语义一致化）：关闭时不做惰性封存，也不另起会话，
                // 直接续写原会话；会话边界与摘要留待允许封存的宿主 / 下次允许时处理。
                if !engine.seal_allowed() {
                    tracing::info!(
                        session_id = %existing.id,
                        idle_minutes = engine.config().session.l1_idle_minutes,
                        "会话空闲超阈值但封存已禁用：续写原会话（不另起、不生成摘要）"
                    );
                    return Ok(existing);
                }
                tracing::info!(
                    session_id = %existing.id,
                    idle_minutes = engine.config().session.l1_idle_minutes,
                    "会话空闲超阈值，先封存再另起（惰性封存体检）"
                );
                // 封存失败不阻塞回流（与 finalize 分支同口径）：会话已被抢占关闭，
                // 摘要留给空闲检查 / 补扫重试；此处若用 `?` 上抛会让"LLM 不可用"
                // 直接吞掉本次用户消息（违反"LLM 不可用不阻塞主流程"）。
                if let Err(e) = crate::seal::run(engine, existing.id).await {
                    tracing::error!(
                        session_id = %existing.id,
                        error = %e,
                        "惰性封存失败，继续另起新会话写入（摘要待空闲检查/补扫重试）"
                    );
                }
            } else {
                tracing::debug!(session_id = %existing.id, "续写已有活跃会话");
                return Ok(existing);
            }
        } else {
            tracing::warn!(
                session_id = %existing.id,
                existing_persona = existing.persona_uid.as_deref().unwrap_or("none"),
                requested_persona = %persona,
                "外部对话标识已被其他人格占用，另起新会话"
            );
        }
    }

    let session = storage
        .create_session_in_channel(Some(persona), channel, external_ref)
        .await?;
    tracing::info!(
        session_id = %session.id,
        channel,
        external_ref = external_ref.unwrap_or("none"),
        persona,
        "已创建带来源标识的新会话"
    );
    Ok(session)
}

/// 判断会话是否空闲超阈值（最后消息距今 > `[session].l1_idle_minutes`）。
///
/// 返回:
/// - `Ok(true)`: 超阈值（需先封存）。
/// - `Ok(false)`: 未超阈值 / 会话无消息（空会话视为可续写）。
///
/// 降级:
/// - `get_last_message_time` 未覆写（Unsupported）→ 回退全量加载消息取最大值。
pub(crate) async fn session_idle(engine: &Engine, session_id: Uuid) -> RamariaResult<bool> {
    let storage = engine.storage_ref().as_ref();
    let threshold_ms = engine.config().session.l1_idle_minutes as i64 * 60_000;

    let last = match storage.get_last_message_time(session_id).await {
        Ok(time) => time,
        Err(RamariaError::Unsupported { .. }) => {
            let messages = storage.list_messages(session_id).await?;
            messages.iter().map(|m| m.created_at).max()
        }
        Err(e) => return Err(e),
    };

    let Some(last) = last else {
        return Ok(false); // 空会话：无空闲概念，直接续写
    };
    Ok(now_ms().saturating_sub(last) >= threshold_ms)
}

// =========================================================
// 去重辅助
// =========================================================

/// 计算"重发前缀"长度：库内消息尾部与本次提交头部的最长匹配条数。
///
/// 说明:
/// - 客户端重复提交整段对话时，已入库的前缀不再重复写入（省去逐条指纹查询）；
/// - 匹配按 (角色, 内容) 逐条比较（库内键已 TRIM，提交侧同样 trim），
///   最多匹配 `min(库内条数, 提交条数)`；
/// - 这是**正确性无关的优化**：即使不跳过，指纹也会把重复消息拦下。
fn suffix_match_len(history: &[MessageKey], turns: &[ChatTurn]) -> usize {
    let max = history.len().min(turns.len());
    for len in (1..=max).rev() {
        let tail = &history[history.len() - len..];
        let head = &turns[..len];
        let matched = tail.iter().zip(head).all(|(key, turn)| {
            key.role == ramaria_core::types::MessageRole::from(turn.role)
                && key.content == turn.content.trim()
        });
        if matched {
            return len;
        }
    }
    0
}

/// 统计库内消息中各 (角色, 内容) 的出现次数（指纹序数基数）。
///
/// 说明:
/// - 覆盖整段对话（跨会话），序数因此精确：重复内容（如"嗯"）也能各自拿到稳定指纹。
fn occurrence_counts(history: &[MessageKey]) -> HashMap<(String, String), usize> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for key in history {
        let pair = (key.role.as_str().to_string(), key.content.clone());
        *counts.entry(pair).or_insert(0) += 1;
    }
    counts
}

/// 回流消息指纹（SHA-256 前 8 字节 → 16 位 hex，与导入去重同一机制）。
///
/// 组成: `对话标识 | 角色 | 内容 | 出现序数`
///
/// 说明:
/// - 客户端不提供时间戳，故以"对话内出现序数"替代时间参与指纹，保证重复提交
///   整段对话时同一条消息得到相同指纹（幂等），同时允许同一对话内重复内容（如"嗯"）
///   各自拥有不同指纹。
fn ingest_fingerprint(conversation_key: &str, role: &str, content: &str, ordinal: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{conversation_key}|{role}|{content}|{ordinal}").as_bytes());
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
