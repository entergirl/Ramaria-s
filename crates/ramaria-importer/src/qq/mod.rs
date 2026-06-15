//! rust/crates/ramaria-importer/src/qq/mod.rs - QQ 聊天记录导入模块
//!
//! 设计特点:
//! - 仅支持 shuakami/qq-chat-exporter v5.x JSON 格式（TXT 已从 v1.1 移除）
//! - `QqImporter` 实现 `ImportSource` trait，通过 `detect_format()` 检测 JSON 格式
//! - 快速导入：仅写 messages 表，标记 fingerprint 去重
//! - 深度导入：创建 session → 写入 L0 → 关闭 session → 触发全管线
//! - Phase 5B: 双画像支持——按发送者分别关联 persona（self_persona_uid vs other_persona_uid）
//! - Phase 5B: `build_persona_uid()` 提供 4 级优先级的 UID 生成策略
//! - Phase 5B: `ensure_qq_persona()` 复用原有逻辑，每次调用创建/查找单个 persona
//! - 完整覆盖 qce v5.x 全部 11 种消息类型（含 v1.1 新增的 type_8/10/19 和 system 过滤）

pub mod parser;

use std::path::Path;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::PersonaKind;
use sqlx::SqlitePool;

use crate::traits::{ImportReport, ImportSource, ImportedSession};

// =========================================================
// QQ 导入器
// =========================================================

/// QQ 聊天记录导入器。
///
/// 职责:
/// - 实现 `ImportSource` trait，提供 QQ 聊天记录的格式检测和解析能力。
/// - 仅支持 qq-chat-exporter v5.x JSON 格式。
/// - 提供 `execute_fast_import()` 方法，执行完整的 L0 导入流程。
pub struct QqImporter;

impl QqImporter {
    /// 创建新的 QQ 导入器。
    pub fn new() -> Self {
        Self
    }

    /// 执行快速导入：仅写入 messages 表（L0）。
    ///
    /// Phase 5B 双画像支持：
    /// - 根据每条消息的发送者（`sender_uid == self_uid`）区分画像归属。
    /// - 导出者本人的消息关联 `self_persona_uid`，对方消息关联 `other_persona_uid`。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池。
    /// - `sessions`: 解析后的 session 列表。
    /// - `self_persona_uid`: 导出者本人的 persona 标识。
    /// - `other_persona_uid`: 对话对方的 persona 标识。
    /// - `self_uid`: 导出者的 QQ UID（用于与消息的 sender_uid 比较）。
    ///
    /// 返回:
    /// - `(sessions_written, messages_written, session_ids)`: 写入统计及创建的 session UUID 列表。
    ///
    /// 说明:
    /// - 每个 session 创建为已关闭的历史 session。
    /// - 消息使用 `save_import()` 写入，绕过 session 活跃状态检查。
    /// - 返回的 session_ids 供调用方触发 L1 摘要等后处理。
    pub async fn execute_fast_import(
        pool: &SqlitePool,
        sessions: &[ImportedSession],
        self_persona_uid: &str,
        other_persona_uid: &str,
        self_uid: &str,
    ) -> RamariaResult<(usize, usize, Vec<uuid::Uuid>)> {
        let mut sessions_written = 0usize;
        let mut messages_written = 0usize;
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
        // 分别统计双方消息数，用于日志输出
        let mut self_msg_count = 0usize;
        let mut other_msg_count = 0usize;

        for session in sessions {
            if session.messages.is_empty() {
                continue;
            }

            // 创建历史 session（已关闭）
            let db_session = ramaria_storage::repo::sessions::create_historical(
                pool,
                session.started_at,
                session.ended_at,
            )
            .await
            .map_err(|e| {
                tracing::error!(session_start = %session.started_at, error = %e, "创建历史 session 失败");
                e
            })?;

            // 逐条写入消息，按发送者分配 persona_uid
            let mut msg_count = 0usize;
            for parsed in &session.messages {
                // Phase 5B: 按发送者决定使用哪个 persona_uid
                let persona_for_msg = if parsed.sender_uid == self_uid {
                    self_msg_count += 1;
                    self_persona_uid
                } else {
                    other_msg_count += 1;
                    other_persona_uid
                };

                let msg = ramaria_core::types::Message {
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
                    persona_uid: Some(persona_for_msg.to_string()),
                };

                ramaria_storage::repo::messages::save_import(pool, &msg)
                    .await
                    .map_err(|e| {
                        tracing::error!(msg_id = %msg.id, error = %e, "写入导入消息失败");
                        e
                    })?;

                msg_count += 1;
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

        tracing::info!(
            self_messages = self_msg_count,
            other_messages = other_msg_count,
            self_persona = %self_persona_uid,
            other_persona = %other_persona_uid,
            "双画像导入统计"
        );

        Ok((sessions_written, messages_written, session_ids))
    }
}

impl Default for QqImporter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ImportSource for QqImporter {
    fn name(&self) -> &'static str {
        "QQ"
    }

    fn detect_format(&self, file_path: &Path) -> RamariaResult<bool> {
        parser::detect_qq_format(file_path)
    }

    fn parse(
        &self,
        file_path: &Path,
        gap_minutes: u32,
    ) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
        parser::parse_qq_export(file_path, gap_minutes)
    }
}

// =========================================================
// Persona UID 生成策略（Phase 5B）
// =========================================================

/// 按优先级生成 persona UID。
///
/// 职责:
/// - 为 QQ 导入的双方画像生成简洁、可辨识的 UID。
/// - 避免使用过于冗长的 QQ 内部 UID（如 `char-u_RSOI7gG2LaRiP64W8ayLDA`）。
///
/// 4 级优先级（从高到低）:
/// 1. **用户显式指定** — `user_provided_uid` 非空时使用；若不以 `char-` 开头则自动补全。
/// 2. **QQ 号（uin）** — 格式 `char-{uin}`，如 `char-123456789`。简洁且对用户可读。
/// 3. **QQ 内部 UID** — 格式 `char-{uid}`，回退方案。
/// 4. **自动递增序号** — 格式 `char-{seq:04}`，如 `char-0003`。通过 `next_seq()` 查询已有 QQ persona 的最大 seq+1。
///
/// 安全约束:
/// - `char-` 前缀作为固定格式不可被修改或截断，所有返回的 UID 均以 `char-` 开头。
///
/// 参数:
/// - `user_provided_uid`: 用户显式指定的 UID（来自 CLI 参数或前端输入），可为空。
/// - `uin`: QQ 号，可能为 None。
/// - `uid`: QQ 内部 UID，可能为空。
/// - `fallback_seq`: 自动递增序号的回调，返回下一个可用 seq。
///
/// 返回:
/// - 按优先级选出的 persona UID。
pub fn build_persona_uid(
    user_provided_uid: Option<&str>,
    uin: Option<&str>,
    uid: &str,
    fallback_seq: u32,
) -> String {
    // 级别 1: 用户显式指定（确保始终以 "char-" 开头）
    if let Some(provided) = user_provided_uid
        && !provided.is_empty()
    {
        let uid_str = if provided.starts_with("char-") {
            provided.to_string()
        } else {
            format!("char-{provided}")
        };
        tracing::debug!(uid = %uid_str, "使用用户显式指定的 persona UID");
        return uid_str;
    }

    // 级别 2: QQ 号（如 `char-123456789`）
    if let Some(qq) = uin
        && !qq.is_empty()
    {
        let uid_str = format!("char-{qq}");
        tracing::debug!(uid = %uid_str, "使用 QQ 号生成 persona UID");
        return uid_str;
    }

    // 级别 3: QQ 内部 UID（如 `char-u_RSOI7gG2LaRiP64W8ayLDA`）
    if !uid.is_empty() {
        let uid_str = format!("char-{uid}");
        tracing::debug!(uid = %uid_str, "使用 QQ UID 生成 persona UID");
        return uid_str;
    }

    // 级别 4: 自动递增序号
    let uid_str = format!("char-{fallback_seq:04}");
    tracing::debug!(uid = %uid_str, seq = fallback_seq, "使用自动递增序号生成 persona UID");
    uid_str
}

// =========================================================
// Persona 辅助函数
// =========================================================

/// 为 QQ 导入准备 persona（查找或创建）。
///
/// 职责:
/// - 如果 persona_uid 对应的 persona 已存在，直接返回。
/// - 如果 (kind, source, ref_id) 已存在（同一来源方的旧导入），
///   复用已有 persona（防止 `idx_personas_kind_source_ref` UNIQUE 冲突）。
/// - 如果都不存在，自动创建一个 `source="qq"` 的 Char 类型 persona。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `persona_uid`: 期望的 persona 标识（如 `char-0003`），可能不被使用。
/// - `persona_name`: persona 显示名称。
/// - `ref_id`: 来源方原始 ID（如 QQ UID），用于跨渠道去重。
///
/// 返回:
/// - 已存在或新创建的 persona 的 `persona_uid`。
pub async fn ensure_qq_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    persona_name: &str,
    ref_id: Option<&str>,
) -> RamariaResult<String> {
    // 优先级 1: 按 uid 精确查找
    let existing = ramaria_storage::repo::personas::get_by_uid(pool, persona_uid).await?;
    if let Some(p) = existing {
        tracing::info!(
            persona_uid = %p.uid,
            persona_name = %p.name,
            "使用已有 persona（按 uid 匹配）"
        );
        return Ok(p.uid);
    }

    // 优先级 2: 按 (kind, source, ref_id) 查找，防止 UNIQUE 索引冲突
    //   idx_personas_kind_source_ref ON personas(kind, source, ref_id) WHERE ref_id IS NOT NULL
    if let Some(rid) = ref_id {
        let kind = PersonaKind::from_uid(persona_uid);
        let by_ref =
            ramaria_storage::repo::personas::get_by_kind_source_ref(pool, kind.as_str(), "qq", rid)
                .await?;
        if let Some(p) = by_ref {
            tracing::info!(
                existing_uid = %p.uid,
                requested_uid = %persona_uid,
                ref_id = %rid,
                "复用已有 persona（按 ref_id 匹配，uid 不同）"
            );
            return Ok(p.uid);
        }
    }

    // 创建新 persona（此时能安全 INSERT，不会触发 UNIQUE 冲突）
    let kind = PersonaKind::from_uid(persona_uid);
    // seq 需要从现有 QQ persona 中取最大值 + 1
    let all_personas = ramaria_storage::repo::personas::list_all(pool).await?;
    let max_seq = all_personas
        .iter()
        .filter(|p| p.source == "qq")
        .map(|p| p.seq)
        .max()
        .unwrap_or(0);

    let persona = ramaria_core::types::Persona::new(
        persona_uid.to_string(),
        persona_name.to_string(),
        kind,
        max_seq + 1,
        "qq".to_string(),
    );
    let persona = ramaria_core::types::Persona {
        ref_id: ref_id.map(|s| s.to_string()),
        ..persona
    };

    let id = ramaria_storage::repo::personas::create(pool, &persona).await?;
    tracing::info!(
        persona_uid = %persona.uid,
        persona_name = %persona.name,
        persona_id = id,
        "已创建 QQ 导入 persona"
    );

    Ok(persona.uid)
}

// =========================================================
// 单元测试（Phase 5B: T-V11-5B-013）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_persona_uid() 4 级优先级测试 ──

    #[test]
    fn build_persona_uid_level_1_user_provided() {
        // 级别 1: 用户显式指定优先
        let uid = build_persona_uid(Some("char-my-custom"), Some("12345"), "u_abc", 7);
        assert_eq!(uid, "char-my-custom");
    }

    #[test]
    fn build_persona_uid_level_1_without_prefix_auto_adds_char() {
        // 用户提供不含 "char-" 前缀的值 → 自动补全
        let uid = build_persona_uid(Some("my-custom"), None, "", 7);
        assert_eq!(uid, "char-my-custom");
    }

    #[test]
    fn build_persona_uid_level_1_empty_is_ignored() {
        // 用户提供空字符串 → 降级到下一级
        let uid = build_persona_uid(Some(""), Some("123456789"), "u_abc", 7);
        assert_eq!(uid, "char-123456789");
    }

    #[test]
    fn build_persona_uid_level_2_qq_number() {
        // 级别 2: QQ 号 uin
        let uid = build_persona_uid(None, Some("123456789"), "u_example_uid", 7);
        assert_eq!(uid, "char-123456789");
    }

    #[test]
    fn build_persona_uid_level_2_uin_empty_falls_to_uid() {
        // uin 为空 → 降级到 UID
        let uid = build_persona_uid(None, Some(""), "u_example_uid", 7);
        assert_eq!(uid, "char-u_example_uid");
    }

    #[test]
    fn build_persona_uid_level_3_qq_uid() {
        // 级别 3: QQ UID（无 uin）
        let uid = build_persona_uid(None, None, "u_example_uid", 7);
        assert_eq!(uid, "char-u_example_uid");
    }

    #[test]
    fn build_persona_uid_level_3_empty_uid_falls_to_seq() {
        // uid 也为空 → 降级到 seq
        let uid = build_persona_uid(None, None, "", 3);
        assert_eq!(uid, "char-0003");
    }

    #[test]
    fn build_persona_uid_level_4_sequential() {
        // 级别 4: 自动递增序号
        let uid = build_persona_uid(None, None, "", 42);
        assert_eq!(uid, "char-0042");
    }

    #[test]
    fn build_persona_uid_all_none() {
        // 极端：所有字段为 None/空
        let uid = build_persona_uid(None, None, "", 1);
        assert_eq!(uid, "char-0001");
    }

    #[test]
    fn build_persona_uid_user_provided_overrides_all() {
        // 用户显式指定时，即使 uin/uid 都有值也使用用户指定的
        let uid = build_persona_uid(Some("char-explicit"), Some("99999"), "u_something", 10);
        assert_eq!(uid, "char-explicit");
    }
}
