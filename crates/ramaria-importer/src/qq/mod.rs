//! crates/ramaria-importer/src/qq/mod.rs - QQ 聊天记录导入模块
//!
//! 设计特点:
//! - 仅支持 shuakami/qq-chat-exporter v6.x JSON 格式（语义化 type 名称）
//! - `QqImporter` 实现 `ImportSource` trait，通过 `detect_format` 检测 JSON 格式
//! - 快速导入：仅写 messages 表，标记 fingerprint 去重
//! - 深度导入：创建 session → 写入 L0 → 关闭 session → 触发全管线
//! - L0 会话/消息写入由通用写入层 `ImportWriter`（`crate::writer`）承担；
//!   本模块负责 QQ 特有的解析与画像 UID 生成，不重复实现写入逻辑
//! - `build_persona_uid` 提供 4 级优先级的 UID 生成策略（含 QQ 号级别 2/3）
//! - `ensure_qq_persona` 每次调用创建/查找单个 source="qq" 的 persona
//! - 完整覆盖 qce v6.x 全部 10 种语义化消息类型（text/reply/audio/json/file/video/forward/type_10/type_19/system）

pub mod parser;

use std::path::Path;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::PersonaKind;
use sqlx::SqlitePool;

use crate::traits::{ImportReport, ImportSource, ImportedSession};

// 兼容历史引用：双画像"双方/导入侧"模型已上移至通用层，此处对外再导出。
// 同时把 PersonaSide / ImportSide 带入本模块作用域（含测试）。
pub use crate::traits::{ImportSide, PersonaSide};

// =========================================================
// QQ 导入器
// =========================================================

/// QQ 聊天记录导入器。
///
/// 职责:
/// - 实现 `ImportSource` trait，提供 QQ 聊天记录的格式检测和解析能力。
/// - 仅支持 qq-chat-exporter v6.x JSON 格式（语义化 type 名称）。
///
/// 说明:
/// - L0 会话/消息写入不在此类型上（已上移至通用写入层 `crate::writer::ImportWriter`），
///   调用方解析完成后以 `ImportWriter::write_l0` 写入。
pub struct QqImporter;

impl QqImporter {
    /// 创建新的 QQ 导入器。
    pub fn new() -> Self {
        Self
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

/// 返回某侧默认的 UID 前缀（含 `-`）。
///
/// QQ 约定: 我方（导出者）`user-`（kind=user，白名单 kind 过滤天然排除我方），
/// 对方 `char-`（kind=char）。该映射是 QQ/项目 UID 命名约定，归属 QQ 侧，
/// 不放在通用双画像模型（`PersonaSide`）中。
fn side_uid_prefix(side: PersonaSide) -> &'static str {
    match side {
        PersonaSide::Me => "user-",
        PersonaSide::Other => "char-",
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
    let prefix = side_uid_prefix(side);

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
}
