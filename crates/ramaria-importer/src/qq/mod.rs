//! rust/crates/ramaria-importer/src/qq/mod.rs - QQ 聊天记录导入模块
//!
//! 设计特点:
//! - 支持两种格式: shuakami/qq-chat-exporter v5.x JSON 和 PC QQ 经典 `.txt` 导出
//! - JSON 格式优先检测（与 Python 参考实现对齐），`.txt` 作为兼容降级
//! - `QqImporter` 实现 `ImportSource` trait，通过 `detect_format()` 自动判断格式
//! - 快速导入：仅写 messages 表，标记 fingerprint 去重
//! - 深度导入：创建 session → 写入 L0 → 关闭 session → 触发全管线
//! - Persona 归属：导入时用户手动指定 persona_uid，自动创建或关联已有 persona

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
/// - 支持 JSON（qq-chat-exporter）和 `.txt`（PC QQ 导出）两种格式。
/// - 提供 `execute_import()` 方法，执行完整的导入流程。
pub struct QqImporter;

impl QqImporter {
    /// 创建新的 QQ 导入器。
    pub fn new() -> Self {
        Self
    }

    /// 执行快速导入：仅写入 messages 表（L0）。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池。
    /// - `sessions`: 解析后的 session 列表。
    /// - `persona_uid`: 关联的 persona 标识。
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
        persona_uid: &str,
    ) -> RamariaResult<(usize, usize, Vec<uuid::Uuid>)> {
        let mut sessions_written = 0usize;
        let mut messages_written = 0usize;
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();

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

            // 逐条写入消息
            let mut msg_count = 0usize;
            for parsed in &session.messages {
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
                    persona_uid: Some(persona_uid.to_string()),
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
// Persona 辅助函数
// =========================================================

/// 为 QQ 导入准备 persona（查找或创建）。
///
/// 职责:
/// - 如果 persona_uid 对应的 persona 已存在，直接返回。
/// - 如果不存在，自动创建一个 `source="qq"` 的 Char 类型 persona。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `persona_uid`: 期望的 persona 标识（如 `char-0003`）。
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
    let existing = ramaria_storage::repo::personas::get_by_uid(pool, persona_uid).await?;
    if let Some(p) = existing {
        tracing::info!(
            persona_uid = %p.uid,
            persona_name = %p.name,
            "使用已有 persona"
        );
        return Ok(p.uid);
    }

    // 创建新 persona
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
    // 手动设置 ref_id
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
