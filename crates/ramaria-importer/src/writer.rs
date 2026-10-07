//! crates/ramaria-importer/src/writer.rs - 导入源无关的会话/消息写入层
//!
//! 设计特点:
//! - `ImportWriter` 将"解析后的标准化聊天记录写入存储"这一层从导入源中抽离出来，
//!   供 QQ 及未来的微信/Telegram 等人对人导入源复用。
//! - 接收导入源无关的中间数据（`ImportedSession` / `ParsedMessage`）与画像归属参数，
//!   负责：按导入侧过滤消息、创建历史 session、按发送方归属 persona、批量写入、
//!   以及跨文件去重查重（指纹已在库中的消息跳过）。
//! - 画像归属经 `PersonaDispatch` 表达：双画像（私聊）按导出者身份二分，
//!   多画像（群聊）按发送者平台 UID 查成员映射。
//! - 只关心 L0（messages/sessions）写入；L1/L2/L3 深度处理由调用方在拿到
//!   返回的 `session_ids` 后自行触发，writer 不做任何 LLM 相关衔接。
//! - 平台特有逻辑（QQ 号/QQ UID → persona UID、source 归属等）不在此层，
//!   由各导入源在调用前解析好 persona_uid 再传入。
//! - 跨文件去重复用 `ramaria_storage::repo::messages::find_by_fingerprint` 同一路径，
//!   本层不重复实现去重规则。

use std::collections::BTreeMap;

use ramaria_core::error::RamariaResult;
use ramaria_core::privacy::mask_id;
use ramaria_core::types::{MemberRole, SessionMember};
use sqlx::SqlitePool;

use crate::traits::{ImportSide, ImportedSession};

// =========================================================
// 写入结果
// =========================================================

/// L0 写入结果，供调用方展示统计并触发后续深度处理。
///
/// 字段约定:
/// - `session_ids`: 已创建的历史 session UUID 列表，供调用方触发 L1 摘要等深度处理。
/// - `messages_dropped`: 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息），
///   调用方应将其作为"非静默降级"提示给用户，而不是仅存在于日志中。
pub struct WriteOutcome {
    /// 成功写入的 session 数
    pub sessions_written: usize,
    /// 成功写入的消息数
    pub messages_written: usize,
    /// 创建的 session UUID 列表（供调用方触发 L1 摘要等深度处理）
    pub session_ids: Vec<uuid::Uuid>,
    /// 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息）
    pub messages_dropped: usize,
}

// =========================================================
// 多画像派发
// =========================================================

/// 多画像成员派发信息（群聊：一个平台发送者对应一个 persona）。
///
/// 字段约定:
/// - `persona_uid`: 该发送者消息归属的 persona 标识。
/// - `group_nickname`: 群名片；导出未提供时为 None。
/// - `role`: 群内角色；导出未提供时为 None。
#[derive(Debug)]
pub struct MemberDispatch {
    /// 该发送者对应的 persona 标识
    pub persona_uid: String,
    /// 群名片（无则 None）
    pub group_nickname: Option<String>,
    /// 群内角色（无则 None）
    pub role: Option<MemberRole>,
}

/// 消息画像归属派发策略。
///
/// 语义:
/// - `Dual`: 双画像（私聊），发送者为导出者 → self persona，其余 → other persona。
/// - `Multi`: 多画像（群聊），按发送者平台 UID 查成员映射；未命中（含空 UID）
///   的消息丢弃并计入 `messages_dropped`。
#[derive(Debug)]
pub enum PersonaDispatch<'a> {
    /// 双画像（私聊）：发送者为导出者 → self persona，其余 → other persona
    Dual {
        /// 导出者的平台内部 UID（用于与消息的 sender_uid 比较）
        self_uid: &'a str,
        /// 导出者本人的画像标识（导入侧过滤跳过时为 None）
        self_persona_uid: Option<&'a str>,
        /// 对话对方的画像标识（导入侧过滤跳过时为 None）
        other_persona_uid: Option<&'a str>,
    },
    /// 多画像（群聊）：按发送者平台 UID 查成员表
    Multi {
        /// 导出者的平台内部 UID（用于判定我方消息）
        self_uid: &'a str,
        /// 平台 UID → 成员派发信息
        members: &'a BTreeMap<String, MemberDispatch>,
        /// 会话归属 persona（调用方保证；None 时整段会话丢弃并计入 messages_dropped）
        owner_uid: Option<&'a str>,
    },
}

// =========================================================
// 通用写入器
// =========================================================

///
/// 导入源无关的 L0 会话/消息写入器。
///
/// 职责:
/// - 把一组已解析、已做画像归属的 session 批量写入存储（仅 L0）。
/// - 处理导入侧过滤（`ImportSide`）、会话归属画像、跨文件指纹去重。
///
/// 说明:
/// - 通过关联方法 `write_l0` 调用，不持有跨调用状态。
pub struct ImportWriter;

impl ImportWriter {
    /// 写入已解析的会话与消息（仅 L0，即 messages/sessions 表）。
    ///
    /// 画像归属（`dispatch`）:
    /// - `Dual`（私聊双画像）：发送者 `sender_uid == self_uid` → `self_persona_uid`，
    ///   其余 → `other_persona_uid`；会话按导入侧归属（`Both` 归属对方）。
    /// - `Multi`（群聊多画像）：按发送者平台 UID 查成员映射得到 persona；
    ///   未命中（含空 UID）的消息丢弃并计入 `messages_dropped`；
    ///   会话归属 `owner_uid`，为 None 时整段会话丢弃并计入 `messages_dropped`。
    ///
    /// 导入侧过滤:
    /// - `side` 控制只处理某一侧：`Me` 只写我方消息、`Other` 只写对方消息、
    ///   `Both` 全部写入（默认）。跳过侧消息不入库；该侧 persona 由调用方不创建。
    /// - 单侧模式下，跳过侧的 `persona_uid` 传 `None`（不会在消息中出现）。
    ///
    /// 去重:
    /// - 复用存储层指纹查重，指纹已在库中的消息跨文件去重跳过。
    /// - 本批（write_l0 调用内）已见指纹集合去重：同一批次内指纹相同的消息
    ///   （如同一通话记录被导出两次）跳过，避免撞 messages.import_fingerprint 全局 UNIQUE。
    /// - 只记计数与指纹尾段，不记消息内容/昵称/QQ 号。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池。
    /// - `sessions`: 解析后的 session 列表。
    /// - `dispatch`: 画像归属派发策略（双画像 / 多画像）。
    /// - `side`: 导入侧过滤（self|other|both）。
    ///
    /// 返回:
    /// - `WriteOutcome`: 写入统计、创建的 session UUID 列表与画像缺失丢弃计数。
    ///
    /// 说明:
    /// - 每个 session 创建为已关闭的历史 session。
    /// - 消息使用 `save_import_batch` 批量写入，绕过 session 活跃状态检查。
    /// - 返回的 session_ids 供调用方触发 L1 摘要等深度处理。
    /// - 查重或批量写入失败时中止本批导入，并补偿删除刚创建的 session（不留半成品会话）。
    pub async fn write_l0(
        pool: &SqlitePool,
        sessions: &[ImportedSession],
        dispatch: PersonaDispatch<'_>,
        side: ImportSide,
    ) -> RamariaResult<WriteOutcome> {
        let mut sessions_written = 0usize;
        let mut messages_written = 0usize;
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
        // 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息）
        let mut messages_dropped = 0usize;
        // 分别统计双方消息数，用于日志输出
        let mut self_msg_count = 0usize;
        let mut other_msg_count = 0usize;
        // 跨文件去重统计：指纹已在库中被跳过的消息数（不记内容）
        let mut dedup_skipped = 0usize;
        // 本批（write_l0 调用内、跨全部 session）已见的指纹集合：
        // 数据库查重只覆盖"已提交历史"，同一批次内两条指纹相同的消息（如同一通话记录被
        // 导出两次）若不在此拦截，会在 save_import_batch 撞 messages.import_fingerprint 的
        // 全局 UNIQUE。此处与跨文件去重同语义，仅把去重范围扩到"本批已见"。
        let mut seen_fingerprints: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // 解构派发策略：导出者 UID 用于消息侧判定；模式标识用于日志排查
        let (self_uid, mode) = match &dispatch {
            PersonaDispatch::Dual { self_uid, .. } => (*self_uid, "dual"),
            PersonaDispatch::Multi { self_uid, .. } => (*self_uid, "multi"),
        };

        for session in sessions {
            // 过滤本 session 消息（按 side）：跳过侧消息不入库
            let mut kept: Vec<(bool, &crate::traits::ParsedMessage)> = Vec::new();
            for parsed in &session.messages {
                let is_self = parsed.sender_uid == self_uid;
                match (side, is_self) {
                    (ImportSide::Me, false) | (ImportSide::Other, true) => continue,
                    _ => {}
                }
                // 本批已见指纹去重：重复 → 跳过（不记内容）
                if !parsed.fingerprint.is_empty()
                    && !seen_fingerprints.insert(parsed.fingerprint.clone())
                {
                    dedup_skipped += 1;
                    tracing::debug!(
                        fp_tail = %&parsed.fingerprint[parsed.fingerprint.len().saturating_sub(4)..],
                        "消息在本批内指纹重复，跳过"
                    );
                    continue;
                }
                kept.push((is_self, parsed));
            }

            // 全部消息被过滤（单侧无该侧消息）→ 跳过该 session（不创建空 session）
            if kept.is_empty() {
                continue;
            }

            // 创建历史 session（已关闭）；Dual 归属处理侧画像（Both 模式归属对方），
            // Multi 归属会话 owner。
            let owner = match &dispatch {
                PersonaDispatch::Dual {
                    self_persona_uid,
                    other_persona_uid,
                    ..
                } => match side {
                    ImportSide::Me => *self_persona_uid,
                    ImportSide::Other | ImportSide::Both => *other_persona_uid,
                },
                PersonaDispatch::Multi { owner_uid, .. } => *owner_uid,
            };
            let Some(owner_uid) = owner else {
                // 防御：处理侧归属画像必须已创建（调用方保证）
                messages_dropped += kept.len();
                match &dispatch {
                    PersonaDispatch::Dual { .. } => tracing::warn!(
                        kept = kept.len(),
                        side = ?side,
                        "session 归属画像未创建，跳过该 session 并将其消息计入丢弃数（导入侧过滤不一致）"
                    ),
                    PersonaDispatch::Multi { .. } => tracing::warn!(
                        kept = kept.len(),
                        side = ?side,
                        "群聊会话归属 persona 缺失，跳过该 session 并将其消息计入丢弃数"
                    ),
                }
                continue;
            };

            let db_session = ramaria_storage::repo::sessions::create_historical(
                pool,
                session.started_at,
                session.ended_at,
                owner_uid,
            )
            .await
            .map_err(|e| {
                tracing::error!(session_start = %session.started_at, error = %e, "创建历史 session 失败");
                e
            })?;

            // 构造消息（按发送者分配 persona_uid；单侧模式下跳过侧不会出现）
            // 写入前按指纹查重：已入库的消息跨文件去重跳过，避免 UNIQUE 冲突与重复入库。
            let mut batch: Vec<ramaria_core::types::Message> = Vec::with_capacity(kept.len());
            for (is_self, parsed) in kept {
                // 跨文件去重：指纹已在库中 → 跳过（只记计数与指纹尾段，不记内容/昵称/QQ 号）
                if !parsed.fingerprint.is_empty() {
                    match ramaria_storage::repo::messages::find_by_fingerprint(
                        pool,
                        &parsed.fingerprint,
                    )
                    .await
                    {
                        Ok(Some(existing)) => {
                            dedup_skipped += 1;
                            tracing::debug!(
                                existing_id = %existing.id,
                                fp_tail = %&parsed.fingerprint[parsed.fingerprint.len().saturating_sub(4)..],
                                "消息已在库中，跨文件去重跳过"
                            );
                            continue;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            // 查重失败若继续导入，后续 UNIQUE 冲突会留下语义不明的错误；
                            // 明确中止本批并补偿删除刚建 session（不留半成品会话）。
                            tracing::error!(
                                session_id = %db_session.id,
                                error = %e,
                                "跨文件指纹查重失败，中止本批导入"
                            );
                            rollback_created_session(pool, db_session.id).await;
                            return Err(e);
                        }
                    }
                }

                // 消息画像分配：Dual 按发送侧取二画像；Multi 按发送者 UID 查成员映射
                let persona_for_msg = match &dispatch {
                    PersonaDispatch::Dual {
                        self_persona_uid,
                        other_persona_uid,
                        ..
                    } => {
                        if is_self {
                            self_msg_count += 1;
                            *self_persona_uid
                        } else {
                            other_msg_count += 1;
                            *other_persona_uid
                        }
                    }
                    PersonaDispatch::Multi { members, .. } => members
                        .get(&parsed.sender_uid)
                        .map(|member| member.persona_uid.as_str()),
                };
                let Some(persona_uid) = persona_for_msg else {
                    // 防御：Dual 单侧模式下不应出现跳过侧消息（已过滤），出现则丢弃记 warn；
                    // Multi 下发送者不在成员映射中（含空 UID）同样丢弃。
                    // sender 为个人标识，日志只记掩码。
                    messages_dropped += 1;
                    match &dispatch {
                        PersonaDispatch::Dual { .. } => tracing::warn!(
                            sender = %mask_id(&parsed.sender_uid),
                            "消息发送侧画像未创建，丢弃该消息（导入侧过滤不一致）"
                        ),
                        PersonaDispatch::Multi { .. } => tracing::warn!(
                            sender = %mask_id(&parsed.sender_uid),
                            "消息发送者不在成员映射中，丢弃该消息（成员画像未准备）"
                        ),
                    }
                    continue;
                };

                // 入站规范投影：sender 身份两列取平台 ID 与发送时显示名（空串视为缺失）
                let inbound = parsed.to_inbound();
                batch.push(ramaria_core::types::Message {
                    id: ramaria_core::types::new_id(),
                    session_id: db_session.id,
                    role: if parsed.role == "user" {
                        ramaria_core::types::MessageRole::User
                    } else {
                        ramaria_core::types::MessageRole::Assistant
                    },
                    content: parsed.content.clone(),
                    created_at: parsed.created_at,
                    source: ramaria_core::types::MessageSource::Local,
                    fingerprint: Some(parsed.fingerprint.clone()),
                    persona_uid: Some(persona_uid.to_string()),
                    // 导入消息均为历史常规消息，不属于主动生成
                    is_proactive: false,
                    sender_ref: non_empty_opt(&inbound.sender.platform_id),
                    sender_name: non_empty_opt(&inbound.sender.display_name),
                });
            }

            // 单事务批量写入（替代逐条 INSERT，显著降低大文件导入的 fsync 开销）；
            // 失败时事务整体回滚，并补偿删除刚建 session（不留半成品会话）。
            let msg_count =
                match ramaria_storage::repo::messages::save_import_batch(pool, &batch).await {
                    Ok(written) => written,
                    Err(e) => {
                        tracing::error!(
                            session_id = %db_session.id,
                            error = %e,
                            "批量写入导入消息失败，中止本批导入"
                        );
                        rollback_created_session(pool, db_session.id).await;
                        return Err(e);
                    }
                };

            // 会话成员聚合：按 platform_ref 归并首末见时间与最近显示名（空 ID 不生成成员行）
            let mut member_aggs: std::collections::BTreeMap<String, SessionMember> =
                std::collections::BTreeMap::new();
            for msg in &batch {
                let Some(platform_ref) = msg.sender_ref.as_deref() else {
                    continue;
                };
                let entry = member_aggs
                    .entry(platform_ref.to_string())
                    .or_insert_with(|| {
                        SessionMember::new(
                            db_session.id,
                            platform_ref,
                            String::new(),
                            msg.created_at,
                            msg.created_at,
                        )
                    });
                entry.first_seen_at = entry.first_seen_at.min(msg.created_at);
                entry.last_seen_at = entry.last_seen_at.max(msg.created_at);
                if let Some(name) = &msg.sender_name {
                    entry.name = name.clone();
                }
                // Multi：从成员映射补齐群名片与角色（查不到保持 None；
                // 与非空合并语义由存储层 upsert 负责）
                if let PersonaDispatch::Multi { members, .. } = &dispatch {
                    if let Some(member) = members.get(platform_ref) {
                        entry.group_nickname = member.group_nickname.clone();
                        entry.role = member.role;
                    }
                }
            }
            if !member_aggs.is_empty() {
                let members: Vec<SessionMember> = member_aggs.into_values().collect();
                if let Err(e) =
                    ramaria_storage::repo::session_members::upsert_batch(pool, &members).await
                {
                    tracing::error!(
                        session_id = %db_session.id,
                        error = %e,
                        "会话成员写入失败（不阻塞导入；成员信息可在后续导入时补齐）"
                    );
                }
            }

            session_ids.push(db_session.id);
            sessions_written += 1;
            messages_written += msg_count;

            if sessions_written.is_multiple_of(10) {
                tracing::info!(
                    sessions_written = sessions_written,
                    total_sessions = sessions.len(),
                    "快速导入进度"
                );
            }
        }

        // 双画像标识仅用于日志展示（Multi 下无此概念，记 None）
        let (self_persona_log, other_persona_log) = match &dispatch {
            PersonaDispatch::Dual {
                self_persona_uid,
                other_persona_uid,
                ..
            } => (*self_persona_uid, *other_persona_uid),
            PersonaDispatch::Multi { .. } => (None, None),
        };

        tracing::info!(
            mode = mode,
            self_messages = self_msg_count,
            other_messages = other_msg_count,
            dedup_skipped = dedup_skipped,
            messages_dropped = messages_dropped,
            self_persona = ?self_persona_log,
            other_persona = ?other_persona_log,
            side = ?side,
            "导入消息归属统计"
        );

        if messages_dropped > 0 {
            tracing::warn!(
                messages_dropped = messages_dropped,
                "本次导入存在因画像缺失被丢弃的消息（详见上方逐条警告，不记录消息内容）"
            );
        }

        Ok(WriteOutcome {
            sessions_written,
            messages_written,
            session_ids,
            messages_dropped,
        })
    }
}

/// 补偿删除本批新建的 session（写入失败回滚）。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `session_id`: 需要删除的 session UUID。
///
/// 说明:
/// - 供 `write_l0` 在查重或批量写入失败时调用，保持"失败不留半成品会话"。
/// - 删除失败仅记录 error 日志（提示重跑导入可能重复创建该会话），不覆盖调用方的原始错误。
async fn rollback_created_session(pool: &sqlx::SqlitePool, session_id: uuid::Uuid) {
    if let Err(del_err) = ramaria_storage::repo::sessions::delete(pool, session_id).await {
        tracing::error!(
            session_id = %session_id,
            error = %del_err,
            "补偿删除已创建 session 失败，重跑导入可能重复创建该会话"
        );
    }
}

/// 空串视为缺失，统一转为 None（身份列 NULL 口径）。
fn non_empty_opt(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
