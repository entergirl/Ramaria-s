//! crates/ramaria-desktop/src/commands/keywords.rs - 关键词池只读视图 + 别名确认（M7）
//!
//! 设计特点:
//! - 对接 T-V20-3-006 CLI（`ramaria keyword`）的同一 storage repo（repo::keyword），
//!   展示 keyword_pool 三态（canonical / alias / pending）与使用次数。
//! - pending 别名可 confirm（合并到规范词）/ reject（晋升独立规范词），
//!   语义与 CLI `keyword alias confirm/reject` 完全一致。
//! - 无 seed 入口（前端只读视图，词条由学习管线/CLI 维护）。
//! - 日志仅记录计数，不记录词条文本（CR-SEC-104 隐私口径）。

use crate::DesktopState;
use ramaria_core::keyword::KeywordToken;
use ramaria_storage::repo::keyword as kw_repo;
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示结构体
// =========================================================

/// 单条词条展示视图。
#[derive(Debug, Clone, Serialize)]
pub struct KeywordEntryView {
    /// 关键词文本（标准化后）
    pub keyword: String,
    /// 使用次数（自然出现 +1；手工种子为 0）
    pub use_count: i64,
    /// 状态: canonical / alias / pending
    pub status: String,
    /// 指向的规范词 rowid（规范词自身为 null）
    pub canonical_id: Option<i64>,
    /// 指向的规范词文本（规范词自身为 null）
    pub canonical_keyword: Option<String>,
    /// 登记时间（Unix 毫秒）
    pub created_at: i64,
}

/// 关键词池列表响应（三态计数 + 全量词条）。
#[derive(Debug, Clone, Serialize)]
pub struct KeywordPoolResponse {
    pub total: usize,
    pub canonical_count: usize,
    pub alias_count: usize,
    pub pending_count: usize,
    pub keywords: Vec<KeywordEntryView>,
}

/// 待确认别名项。
#[derive(Debug, Clone, Serialize)]
pub struct PendingAliasView {
    pub alias_id: i64,
    pub alias_keyword: String,
    pub canonical_keyword: String,
    pub created_at: i64,
}

/// 待确认别名列表响应。
#[derive(Debug, Clone, Serialize)]
pub struct PendingAliasResponse {
    pub total: usize,
    pub aliases: Vec<PendingAliasView>,
}

/// 别名确认/驳回结果。
#[derive(Debug, Clone, Serialize)]
pub struct AliasResolveResponse {
    /// 被处理的别名文本
    pub alias: String,
    /// 处理后指向的规范词（confirm 后有值；reject 后为 null）
    pub canonical_keyword: Option<String>,
    /// 处理后状态: alias / canonical
    pub status: String,
}

// =========================================================
// 辅助
// =========================================================

/// 词条状态文本（keyword_pool 口径: canonical/alias/pending）。
fn status_of(alias_status: &Option<String>) -> &'static str {
    match alias_status.as_deref() {
        Some("alias") => "alias",
        Some("pending") => "pending",
        _ => "canonical",
    }
}

/// 校验关键词文本为合法标准化 token。
fn parse_keyword(raw: &str) -> Result<KeywordToken, String> {
    KeywordToken::new(raw).ok_or_else(|| format!("无效关键词: '{raw}'（需非空且不超过 256 字符）"))
}

// =========================================================
// list_keywords — 关键词池全量
// =========================================================

/// 列出关键词池全部词条（按 use_count 降序），含三态计数。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_keywords(state: State<'_, DesktopState>) -> Result<KeywordPoolResponse, String> {
    let entries = kw_repo::list_entries(&state.pool)
        .await
        .map_err(|e| format!("查询关键词列表失败: {e}"))?;

    let mut canonical_count = 0usize;
    let mut alias_count = 0usize;
    let mut pending_count = 0usize;
    let mut keywords = Vec::with_capacity(entries.len());

    for e in entries {
        match status_of(&e.alias_status) {
            "canonical" => canonical_count += 1,
            "alias" => alias_count += 1,
            _ => pending_count += 1,
        }
        keywords.push(KeywordEntryView {
            keyword: e.keyword,
            use_count: e.use_count,
            status: status_of(&e.alias_status).to_string(),
            canonical_id: e.canonical_id,
            canonical_keyword: e.canonical_keyword,
            created_at: e.created_at,
        });
    }

    tracing::debug!(
        total = keywords.len(),
        canonical = canonical_count,
        alias = alias_count,
        pending = pending_count,
        "list_keywords 完成"
    );

    Ok(KeywordPoolResponse {
        total: keywords.len(),
        canonical_count,
        alias_count,
        pending_count,
        keywords,
    })
}

// =========================================================
// list_pending_aliases — 待确认别名
// =========================================================

/// 列出全部待确认别名冲突（pending，别名 → 建议规范词）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_pending_aliases(
    state: State<'_, DesktopState>,
) -> Result<PendingAliasResponse, String> {
    let pending = kw_repo::list_pending_aliases(&state.pool)
        .await
        .map_err(|e| format!("查询待确认别名失败: {e}"))?;

    let aliases: Vec<PendingAliasView> = pending
        .into_iter()
        .map(|p| PendingAliasView {
            alias_id: p.alias_id,
            alias_keyword: p.alias_keyword,
            canonical_keyword: p.canonical_keyword,
            created_at: p.created_at,
        })
        .collect();

    tracing::debug!(count = aliases.len(), "list_pending_aliases 完成");
    Ok(PendingAliasResponse {
        total: aliases.len(),
        aliases,
    })
}

// =========================================================
// resolve_alias — 确认 / 驳回 pending 别名
// =========================================================

/// 确认（合并到规范词）或驳回（晋升独立规范词）单个 pending 别名。
///
/// 参数:
/// - `alias`: 待处理的别名文本（标准化比较）。
/// - `action`: "confirm" 确认合并 / "reject" 驳回。
///
/// 说明:
/// - 词条不存在或非 pending 状态时返回业务校验错误（不写库）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn resolve_alias(
    state: State<'_, DesktopState>,
    alias: String,
    action: String,
) -> Result<AliasResolveResponse, String> {
    let confirm = match action.as_str() {
        "confirm" => true,
        "reject" => false,
        _ => return Err(format!("无效操作: '{action}'（应为 confirm 或 reject）")),
    };

    let token = parse_keyword(&alias)?;
    let rowid = kw_repo::find_rowid(&state.pool, token.as_str())
        .await
        .map_err(|e| format!("查询词条 rowid 失败: {e}"))?
        .ok_or_else(|| format!("关键词 '{alias}' 不存在"))?;

    // 预取词条现状：非 pending 直接报错
    let entries = kw_repo::list_entries(&state.pool)
        .await
        .map_err(|e| format!("查询关键词词条失败: {e}"))?;
    let entry = entries
        .iter()
        .find(|e| e.keyword == token.as_str())
        .ok_or_else(|| format!("关键词 '{alias}' 不存在"))?;

    if status_of(&entry.alias_status) != "pending" {
        let verb = if confirm { "确认合并" } else { "驳回" };
        return Err(format!(
            "关键词 '{alias}' 当前状态为 {}，不是待确认别名（pending），无法{verb}",
            status_of(&entry.alias_status)
        ));
    }
    let canonical_text = entry
        .canonical_keyword
        .as_deref()
        .unwrap_or("（规范词缺失）")
        .to_string();

    // 执行状态机迁移（返回 false 表示现状已变化，未生效）
    let changed = if confirm {
        kw_repo::confirm_alias(&state.pool, rowid)
            .await
            .map_err(|e| format!("确认别名失败: {e}"))?
    } else {
        kw_repo::reject_alias(&state.pool, rowid)
            .await
            .map_err(|e| format!("驳回别名失败: {e}"))?
    };
    if !changed {
        return Err(format!(
            "词条 '{alias}' 状态已变化（非待确认别名），操作未执行，请刷新后重试"
        ));
    }

    let new_status = if confirm { "alias" } else { "canonical" };
    let canonical_json = if confirm { Some(canonical_text) } else { None };

    tracing::debug!(alias = %token.as_str(), action = action.as_str(), "别名状态迁移完成");
    Ok(AliasResolveResponse {
        alias: token.as_str().to_string(),
        canonical_keyword: canonical_json,
        status: new_status.to_string(),
    })
}

// =========================================================
// 单元测试（状态判定纯逻辑）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_covers_three_states() {
        assert_eq!(status_of(&None), "canonical");
        assert_eq!(status_of(&Some("canonical".to_string())), "canonical");
        assert_eq!(status_of(&Some("alias".to_string())), "alias");
        assert_eq!(status_of(&Some("pending".to_string())), "pending");
        // 未知取值兜底为 canonical（与 keyword-design 口径一致）
        assert_eq!(status_of(&Some("weird".to_string())), "canonical");
    }

    #[test]
    fn keyword_token_validation_rejects_empty_and_oversized() {
        assert!(parse_keyword("").is_err());
        assert!(parse_keyword("   ").is_err());
        assert!(parse_keyword(&"x".repeat(300)).is_err());
        assert!(parse_keyword("旅行").is_ok());
    }
}
