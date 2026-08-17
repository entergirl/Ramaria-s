//! crates/ramaria-importer/src/qq/mod.rs - QQ 聊天记录导入模块
//!
//! 设计特点:
//! - 仅支持 shuakami/qq-chat-exporter v6.x JSON 格式（语义化 type 名称）
//! - `QqImporter` 实现 `ImportSource` trait，通过 `detect_format` 检测 JSON 格式
//! - 快速导入：仅写 messages 表，标记 fingerprint 去重
//! - 深度导入：创建 session → 写入 L0 → 关闭 session → 触发全管线
//! - 双画像支持——按发送者分别关联 persona（self_persona_uid vs other_persona_uid）
//! - `build_persona_uid` 提供 4 级优先级的 UID 生成策略
//! - `ensure_qq_persona` 复用原有逻辑，每次调用创建/查找单个 persona
//! - 完整覆盖 qce v6.x 全部 10 种语义化消息类型（text/reply/audio/json/file/video/forward/type_10/type_19/system）

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
/// - 仅支持 qq-chat-exporter v6.x JSON 格式（语义化 type 名称）。
/// - 提供 `execute_fast_import` 方法，执行完整的 L0 导入流程。
pub struct QqImporter;

impl QqImporter {
    /// 创建新的 QQ 导入器。
    pub fn new() -> Self {
        Self
    }

    /// 执行快速导入：仅写入 messages 表（L0）。
    ///
    /// 双画像支持：
    /// - 根据每条消息的发送者（`sender_uid == self_uid`）区分画像归属。
    /// - 导出者本人的消息关联 `self_persona_uid`，对方消息关联 `other_persona_uid`。
    ///
    /// 导入侧过滤：
    /// - `side` 控制只处理某一侧：`Me` 只写我方消息、`Other` 只写对方消息、
    ///   `Both` 全部写入（默认）。跳过侧消息不入库；该侧 persona 由调用方不创建。
    /// - 单侧模式下，跳过侧的 `persona_uid` 传 `None`（不会在消息中出现）；
    ///   session 归属为处理侧 persona。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池。
    /// - `sessions`: 解析后的 session 列表。
    /// - `self_persona_uid`: 导出者本人的 persona 标识（`side=Other` 时为 None）。
    /// - `other_persona_uid`: 对话对方的 persona 标识（`side=Me` 时为 None）。
    /// - `self_uid`: 导出者的 QQ UID（用于与消息的 sender_uid 比较）。
    /// - `side`: 导入侧过滤（self|other|both）。
    ///
    /// 返回:
    /// - `(sessions_written, messages_written, session_ids)`: 写入统计及创建的 session UUID 列表。
    ///
    /// 说明:
    /// - 每个 session 创建为已关闭的历史 session。
    /// - 消息使用 `save_import` 写入，绕过 session 活跃状态检查。
    /// - 返回的 session_ids 供调用方触发 L1 摘要等后处理。
    pub async fn execute_fast_import(
        pool: &SqlitePool,
        sessions: &[ImportedSession],
        self_persona_uid: Option<&str>,
        other_persona_uid: Option<&str>,
        self_uid: &str,
        side: ImportSide,
    ) -> RamariaResult<(usize, usize, Vec<uuid::Uuid>)> {
        let mut sessions_written = 0usize;
        let mut messages_written = 0usize;
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
        // 分别统计双方消息数，用于日志输出
        let mut self_msg_count = 0usize;
        let mut other_msg_count = 0usize;

        for session in sessions {
            // 过滤本 session 消息（按 side）：跳过侧消息不入库
            let mut kept: Vec<(bool, &crate::traits::ParsedMessage)> = Vec::new();
            for parsed in &session.messages {
                let is_self = parsed.sender_uid == self_uid;
                match (side, is_self) {
                    (ImportSide::Me, false) | (ImportSide::Other, true) => continue,
                    _ => {}
                }
                kept.push((is_self, parsed));
            }

            // 全部消息被过滤（单侧无该侧消息）→ 跳过该 session（不创建空 session）
            if kept.is_empty() {
                continue;
            }

            // 创建历史 session（已关闭）；归属为处理侧 persona
            let owner = match side {
                ImportSide::Me => self_persona_uid,
                _ => other_persona_uid,
            };
            let Some(owner_uid) = owner else {
                // 防御：单侧模式下归属侧 persona 必须已创建（调用方保证）
                tracing::warn!("session 归属 persona 未创建，跳过该 session");
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
            let mut batch: Vec<ramaria_core::types::Message> = Vec::with_capacity(kept.len());
            for (is_self, parsed) in kept {
                let persona_for_msg = if is_self {
                    self_msg_count += 1;
                    self_persona_uid
                } else {
                    other_msg_count += 1;
                    other_persona_uid
                };
                let Some(persona_uid) = persona_for_msg else {
                    // 防御：单侧模式下不应出现跳过侧消息（已过滤），出现则丢弃记 warn
                    tracing::warn!(
                        sender = %parsed.sender_uid,
                        "消息发送侧 persona 未创建，丢弃该消息（导入侧过滤不一致）"
                    );
                    continue;
                };

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
                });
            }

            // 单事务批量写入（替代逐条 INSERT，显著降低大文件导入的 fsync 开销）
            let written = ramaria_storage::repo::messages::save_import_batch(pool, &batch)
                .await
                .map_err(|e| {
                    tracing::error!(session_id = %db_session.id, error = %e, "批量写入导入消息失败");
                    e
                })?;
            let msg_count = written;

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
            self_persona = ?self_persona_uid,
            other_persona = ?other_persona_uid,
            side = ?side,
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
// Persona UID 生成策略
// =========================================================

/// 导入侧（我方/对方），决定 persona UID 前缀与 kind。
///
/// 语义:
/// - `Me`（我方，导出者）: UID 前缀 `user-`，kind=user —— 白名单 kind 过滤
///   （Char/Anim/Oc/Hist）天然排除我方，探针/画像始终面向"对方"。
/// - `Other`（对方）: UID 前缀 `char-`，kind=char —— 与 v1.5 及以前一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaSide {
    /// 我方（导出者本人）
    Me,
    /// 对方（对话另一方）
    Other,
}

impl PersonaSide {
    /// 返回该侧默认的 UID 前缀（含 `-`）。
    fn uid_prefix(self) -> &'static str {
        match self {
            PersonaSide::Me => "user-",
            PersonaSide::Other => "char-",
        }
    }
}

/// 导入侧过滤选项（`import --side self|other|both`）。
///
/// 语义:
/// - `Me`（self）: 只处理我方消息；跳过侧（对方）消息不入库、对方 persona 不创建。
/// - `Other`: 只处理对方消息；我方 persona 不创建。
/// - `Both`: 双方都处理（默认，与 v1.5 行为一致）。
///
/// 用途:
/// - 调用方（CLI `--side` / 桌面导入面板）按选项控制 persona 创建与消息写入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSide {
    /// 只处理我方（self）
    Me,
    /// 只处理对方（other）
    Other,
    /// 双方都处理（默认）
    Both,
}

impl ImportSide {
    /// 解析 CLI/前端字符串（`self`/`other`/`both`，大小写不敏感）。
    ///
    /// 返回:
    /// - `Ok(Some(side))`: 合法值。
    /// - `Ok(None)`: 空/未提供 → 默认 `Both`。
    /// - `Err(msg)`: 非法值（业务校验失败提示）。
    pub fn parse_cli(value: Option<&str>) -> Result<ImportSide, String> {
        match value.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("both") => Ok(ImportSide::Both),
            Some("self") | Some("me") => Ok(ImportSide::Me),
            Some("other") => Ok(ImportSide::Other),
            Some(other) => Err(format!(
                "不支持的导入侧: '{other}'（仅支持 self | other | both）"
            )),
        }
    }

    /// 该侧是否需要创建 persona（Both 时两侧都创建）。
    pub fn needs_persona(self, side: PersonaSide) -> bool {
        match self {
            ImportSide::Both => true,
            ImportSide::Me => side == PersonaSide::Me,
            ImportSide::Other => side == PersonaSide::Other,
        }
    }
}

/// 按优先级生成 persona UID。
///
/// 职责:
/// - 为 QQ 导入的双方画像生成简洁、可辨识的 UID。
/// - 避免使用过于冗长的 QQ 内部 UID（如 `char-u_example_uid`）。
/// - 我方（self）生成 `user-*` 前缀（kind=user），对方（other）仍 `char-*`。
///
/// 4 级优先级（从高到低）:
/// 1. **用户显式指定** — `user_provided_uid` 非空时使用；若未以既有 kind 前缀
///    （rama-/user-/char-/anim-/oc-/hist-）开头则按侧自动补全（self→`user-`、other→`char-`）。
/// 2. **QQ 号（uin）** — 格式 `{prefix}{uin}`，如 `user-123456789` / `char-123456789`。
/// 3. **QQ 内部 UID** — 格式 `{prefix}{uid}`，回退方案。
/// 4. **自动递增序号** — 格式 `{prefix}{seq:04}`，如 `user-0003` / `char-0003`。
///
/// 安全约束:
/// - `user-`/`char-` 前缀作为固定格式不可被修改或截断，所有返回的 UID 均以侧前缀开头。
///
/// 参数:
/// - `side`: 导入侧（self → `user-*`/kind=user；other → `char-*`）。
/// - `user_provided_uid`: 用户显式指定的 UID（来自 CLI 参数或前端输入），可为空。
/// - `uin`: QQ 号，可能为 None。
/// - `uid`: QQ 内部 UID，可能为空。
/// - `fallback_seq`: 自动递增序号的回调，返回下一个可用 seq。
///
/// 返回:
/// - 按优先级选出的 persona UID。
pub fn build_persona_uid(
    side: PersonaSide,
    user_provided_uid: Option<&str>,
    uin: Option<&str>,
    uid: &str,
    fallback_seq: u32,
) -> String {
    let prefix = side.uid_prefix();

    // 级别 1: 用户显式指定（未以既有 kind 前缀开头时按侧补全）
    if let Some(provided) = user_provided_uid
        && !provided.is_empty()
    {
        let has_kind_prefix = ["rama-", "user-", "char-", "anim-", "oc-", "hist-"]
            .iter()
            .any(|p| provided.starts_with(p));
        let uid_str = if has_kind_prefix {
            provided.to_string()
        } else {
            format!("{prefix}{provided}")
        };
        tracing::debug!(uid = %uid_str, "使用用户显式指定的 persona UID");
        return uid_str;
    }

    // 级别 2: QQ 号（如 `user-123456789`）
    if let Some(qq) = uin
        && !qq.is_empty()
    {
        let uid_str = format!("{prefix}{qq}");
        tracing::debug!(uid = %uid_str, "使用 QQ 号生成 persona UID");
        return uid_str;
    }

    // 级别 3: QQ 内部 UID（如 `user-u_example_uid`）
    if !uid.is_empty() {
        let uid_str = format!("{prefix}{uid}");
        tracing::debug!(uid = %uid_str, "使用 QQ UID 生成 persona UID");
        return uid_str;
    }

    // 级别 4: 自动递增序号
    let uid_str = format!("{prefix}{fallback_seq:04}");
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
    // idx_personas_kind_source_ref ON personas(kind, source, ref_id) WHERE ref_id IS NOT NULL
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
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_persona_uid 4 级优先级测试 ──

    /// build_persona_uid 全分支参数化验证（对方 Other 侧，char- 前缀）：
    /// 级别 1 用户显式指定优先；级别 2 QQ 号 uin；级别 3 QQ UID；级别 4 自动序号。
    #[test]
    fn build_persona_uid_cases_other() {
        let cases = [
            // (provided, uin, uid, seq, expected)
            (
                Some("char-my-custom"),
                Some("12345"),
                "u_abc",
                7,
                "char-my-custom",
            ), // L1 显式指定
            (Some("my-custom"), None, "", 7, "char-my-custom"), // L1 自动补前缀
            (Some(""), Some("123456789"), "u_abc", 7, "char-123456789"), // L1 空 → L2
            (
                None,
                Some("123456789"),
                "u_example_uid",
                7,
                "char-123456789",
            ), // L2 QQ 号
            (None, Some(""), "u_example_uid", 7, "char-u_example_uid"), // L2 空 → L3
            (None, None, "u_example_uid", 7, "char-u_example_uid"), // L3 QQ UID
            (None, None, "", 3, "char-0003"),                   // L3 空 → L4
            (None, None, "", 42, "char-0042"),                  // L4 序号
            (None, None, "", 1, "char-0001"),                   // 全部为空
            (
                Some("char-explicit"),
                Some("99999"),
                "u_something",
                10,
                "char-explicit",
            ), // 用户指定覆盖一切
        ];
        for (provided, uin, uid, seq, expected) in cases {
            assert_eq!(
                build_persona_uid(PersonaSide::Other, provided, uin, uid, seq),
                expected,
                "side=Other provided={provided:?} uin={uin:?} uid={uid:?} seq={seq}"
            );
        }
    }

    /// build_persona_uid 我方（Self）分支：
    /// 自动生成路径一律 `user-` 前缀（kind=user），与对方 `char-` 区分。
    #[test]
    fn build_persona_uid_cases_self() {
        let cases = [
            // (provided, uin, uid, seq, expected)
            (
                Some("user-my-self"),
                Some("12345"),
                "u_abc",
                7,
                "user-my-self",
            ), // L1 显式指定（已有 user- 前缀原样保留）
            (Some("my-self"), None, "", 7, "user-my-self"), // L1 自动补 user- 前缀
            (Some(""), Some("123456789"), "u_abc", 7, "user-123456789"), // L1 空 → L2
            (
                None,
                Some("123456789"),
                "u_example_uid",
                7,
                "user-123456789",
            ), // L2 QQ 号
            (None, Some(""), "u_example_uid", 7, "user-u_example_uid"), // L2 空 → L3
            (None, None, "u_example_uid", 7, "user-u_example_uid"), // L3 QQ UID
            (None, None, "", 3, "user-0003"),               // L3 空 → L4
            (None, None, "", 42, "user-0042"),              // L4 序号
            (None, None, "", 1, "user-0001"),               // 全部为空
            (
                Some("char-explicit-self"),
                Some("99999"),
                "u_something",
                10,
                "char-explicit-self",
            ), // 显式 kind 前缀尊重用户指定
        ];
        for (provided, uin, uid, seq, expected) in cases {
            assert_eq!(
                build_persona_uid(PersonaSide::Me, provided, uin, uid, seq),
                expected,
                "side=Self provided={provided:?} uin={uin:?} uid={uid:?} seq={seq}"
            );
        }
    }

    /// ensure_qq_persona 兼容 `user-` 前缀：
    /// `PersonaKind::from_uid("user-xxx")` 必须推导为 User kind（我方）。
    #[test]
    fn user_prefix_resolves_to_user_kind() {
        assert_eq!(
            PersonaKind::from_uid("user-0001"),
            PersonaKind::User,
            "user- 前缀必须推导为 User kind（白名单过滤天然排除我方）"
        );
        assert_eq!(
            PersonaKind::from_uid("char-0001"),
            PersonaKind::Char,
            "char- 前缀保持 Char kind（对方）"
        );
    }

    // ── execute_fast_import 导入侧过滤 ──

    /// 构造一个含 self + other 各 1 条消息的 session。
    fn make_side_session(
        self_content: &str,
        other_content: &str,
    ) -> crate::traits::ImportedSession {
        crate::traits::ImportedSession {
            messages: vec![
                crate::traits::ParsedMessage {
                    role: "user".to_string(),
                    content: self_content.to_string(),
                    created_at: 1100,
                    fingerprint: format!("f-self-{self_content}"),
                    sender_uid: "SELF_UID".to_string(),
                    sender_uin: Some("10001".to_string()),
                    sender_name: "我".to_string(),
                },
                crate::traits::ParsedMessage {
                    role: "assistant".to_string(),
                    content: other_content.to_string(),
                    created_at: 1200,
                    fingerprint: format!("f-other-{other_content}"),
                    sender_uid: "OTHER_UID".to_string(),
                    sender_uin: Some("20002".to_string()),
                    sender_name: "对方".to_string(),
                },
            ],
            started_at: 1000,
            ended_at: 2000,
        }
    }

    /// 创建单连接内存库（max_connections=1 保证 sqlite::memory: 共享同一库）。
    async fn side_test_pool() -> sqlx::SqlitePool {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // 最小 schema（sessions + messages，对应 create_historical / save_import_batch 所需列）
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                persona_uid TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                source TEXT NOT NULL,
                import_fingerprint TEXT,
                persona_uid TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn msg_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn msg_persona_uids(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT persona_uid FROM messages ORDER BY created_at")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn session_owner(pool: &sqlx::SqlitePool) -> Option<String> {
        sqlx::query_scalar("SELECT persona_uid FROM sessions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// side=self（Me）：只写我方消息，跳过侧（对方）零消息零 persona；session 归属我方。
    #[tokio::test]
    async fn execute_fast_import_side_me_filters_other() {
        let pool = side_test_pool().await;
        let sessions = vec![make_side_session("我的发言", "对方发言")];

        let (sessions_written, messages_written, _) = QqImporter::execute_fast_import(
            &pool,
            &sessions,
            Some("user-0001"),
            None, // side=Me：对方 persona 不创建
            "SELF_UID",
            ImportSide::Me,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 1, "跳过侧消息必须不入库");
        assert_eq!(msg_count(&pool).await, 1);
        assert_eq!(msg_persona_uids(&pool).await, vec!["user-0001".to_string()]);
        assert_eq!(session_owner(&pool).await.as_deref(), Some("user-0001"));
    }

    /// side=other：只写对方消息，我方 persona 不创建；session 归属对方。
    #[tokio::test]
    async fn execute_fast_import_side_other_filters_self() {
        let pool = side_test_pool().await;
        let sessions = vec![make_side_session("我的发言", "对方发言")];

        let (sessions_written, messages_written, _) = QqImporter::execute_fast_import(
            &pool,
            &sessions,
            None, // side=Other：我方 persona 不创建
            Some("char-0001"),
            "SELF_UID",
            ImportSide::Other,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 1, "我方消息必须被过滤");
        assert_eq!(msg_count(&pool).await, 1);
        assert_eq!(msg_persona_uids(&pool).await, vec!["char-0001".to_string()]);
        assert_eq!(session_owner(&pool).await.as_deref(), Some("char-0001"));
    }

    /// side=both（默认）：双方消息全部写入（与 v1.5 行为一致）。
    #[tokio::test]
    async fn execute_fast_import_side_both_keeps_all() {
        let pool = side_test_pool().await;
        let sessions = vec![make_side_session("我的发言", "对方发言")];

        let (sessions_written, messages_written, _) = QqImporter::execute_fast_import(
            &pool,
            &sessions,
            Some("user-0001"),
            Some("char-0001"),
            "SELF_UID",
            ImportSide::Both,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 2, "both 模式双方消息全部入库");
        assert_eq!(
            msg_persona_uids(&pool).await,
            vec!["user-0001".to_string(), "char-0001".to_string()]
        );
    }

    /// 单侧模式下 session 内全部为跳过侧消息 → 不创建空 session（零消息零 session）。
    #[tokio::test]
    async fn execute_fast_import_side_skips_empty_session() {
        let pool = side_test_pool().await;
        // 只有 self 消息的 session，side=Other → 全部过滤 → session 不创建
        let sessions = vec![make_side_session("我的发言", "对方发言")];
        let mut only_self = sessions;
        only_self[0].messages.retain(|m| m.sender_uid == "SELF_UID");

        let (sessions_written, messages_written, _) = QqImporter::execute_fast_import(
            &pool,
            &only_self,
            None,
            Some("char-0001"),
            "SELF_UID",
            ImportSide::Other,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 0, "全过滤 session 不应创建");
        assert_eq!(messages_written, 0);
        assert_eq!(msg_count(&pool).await, 0);
        let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(session_count, 0);
    }

    /// ImportSide::parse_cli 解析（self|other|both；非法值报错）。
    #[test]
    fn import_side_parse_cli() {
        assert_eq!(ImportSide::parse_cli(None).unwrap(), ImportSide::Both);
        assert_eq!(
            ImportSide::parse_cli(Some("both")).unwrap(),
            ImportSide::Both
        );
        assert_eq!(ImportSide::parse_cli(Some("SELF")).unwrap(), ImportSide::Me);
        assert_eq!(ImportSide::parse_cli(Some("me")).unwrap(), ImportSide::Me);
        assert_eq!(
            ImportSide::parse_cli(Some("other")).unwrap(),
            ImportSide::Other
        );
        assert!(ImportSide::parse_cli(Some("all")).is_err());
    }
}
