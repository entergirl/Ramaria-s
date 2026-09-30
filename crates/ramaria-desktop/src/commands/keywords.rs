//! crates/ramaria-desktop/src/commands/keywords.rs - 关键词池只读视图 + 别名确认
//!
//! 设计特点:
//! - 委托服务层关键词用例：展示 keyword_pool 三态（canonical / alias / pending）
//!   与使用次数
//! - pending 别名可 confirm（合并到规范词）/ reject（晋升独立规范词），
//!   语义与 CLI `keyword alias confirm/reject` 一致（桌面口径：非 pending 报错）
//! - 无 seed 入口（前端只读视图，词条由学习管线/CLI 维护）
//! - 日志不记录词条文本：仅计数或 `redact_text_label` 脱敏标签（隐私口径）

use crate::DesktopState;
use ramaria_service::{AliasAction, AliasResolveRequest};
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
// list_keywords — 关键词池全量
// =========================================================

/// 列出关键词池全部词条（按 use_count 降序），含三态计数。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_keywords(state: State<'_, DesktopState>) -> Result<KeywordPoolResponse, String> {
    let pool = state
        .engine
        .keyword_list()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询关键词列表失败"))?;

    let keywords: Vec<KeywordEntryView> = pool
        .keywords
        .into_iter()
        .map(|entry| KeywordEntryView {
            keyword: entry.keyword,
            use_count: entry.use_count,
            status: entry.status,
            canonical_id: entry.canonical_id,
            canonical_keyword: entry.canonical_keyword,
            created_at: entry.created_at,
        })
        .collect();

    tracing::debug!(
        total = keywords.len(),
        canonical = pool.canonical_count,
        alias = pool.alias_count,
        pending = pool.pending_count,
        "list_keywords 完成"
    );

    Ok(KeywordPoolResponse {
        total: pool.total,
        canonical_count: pool.canonical_count,
        alias_count: pool.alias_count,
        pending_count: pool.pending_count,
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
    let pending = state
        .engine
        .keyword_pending_aliases()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询待确认别名失败"))?;

    let aliases: Vec<PendingAliasView> = pending
        .into_iter()
        .map(|p| PendingAliasView {
            alias_id: p.alias_id,
            alias_keyword: p.alias,
            canonical_keyword: p.canonical,
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
#[tracing::instrument(skip(state, alias))]
pub async fn resolve_alias(
    state: State<'_, DesktopState>,
    alias: String,
    action: String,
) -> Result<AliasResolveResponse, String> {
    let action_value = match action.as_str() {
        "confirm" => AliasAction::Confirm,
        "reject" => AliasAction::Reject,
        _ => return Err(format!("无效操作: '{action}'（应为 confirm 或 reject）")),
    };

    // 桌面口径：confirm 且词条已是 alias 时按非 pending 报错（不幂等成功）
    let outcome = state
        .engine
        .keyword_resolve_alias(AliasResolveRequest {
            alias,
            action: action_value,
            already_applied_ok: false,
        })
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "别名处理失败"))?;

    tracing::debug!(
        alias = %crate::path_guard::redact_text_label(&outcome.alias),
        action = action.as_str(),
        status = %outcome.status,
        "别名状态迁移完成"
    );
    Ok(AliasResolveResponse {
        alias: outcome.alias,
        canonical_keyword: outcome.canonical_keyword,
        status: outcome.status,
    })
}
