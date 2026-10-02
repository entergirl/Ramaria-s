//! crates/ramaria-service/src/recall/entry.rs - 召回用例入口（策略校验与模式分流）
//!
//! 设计特点:
//! - 入口编排：策略校验（人格白名单不通过 → `Privacy` 错误）→ 请求归一化 → 模式分流
//! - 检索输入判定：显式 `query` > 最后一条用户消息 → 检索模式；两者皆空 → 概览模式
//! - 归一化口径与在线管线一致：人格 uid 空串视为缺省、空白 query 视为无输入
//! - 只做分流与参数归一化：检索装配见 `search`，时间线装配见 `overview`

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::engine::Engine;
use crate::types::{DEFAULT_PERSONA_UID, RecallRequest, RecallResult};

use super::overview::overview;
use super::search::search;

// =========================================================
// 召回用例入口
// =========================================================

/// 执行召回用例。
///
/// 流程:
/// 1. 策略校验：人格白名单不通过 → `Privacy` 错误（越权可见性拒绝）；
/// 2. 归一化请求（分层 / 上限 / 预算）；
/// 3. 检索输入判定：显式 `query` > 最后一条用户消息 → 检索模式；
///    两者皆空 → 概览模式（时间线返回最近记忆）；
/// 4. 分层装配（记忆层走共用召回实现，其余层按层读取并渲染）；
/// 5. 预算裁剪（context 字符预算 + items 条数上限）与 stats 汇总。
///
/// 参数:
/// - `engine`: 服务层引擎（存储 / 配置 / 检索槽 / 策略）。
/// - `req`: 召回请求（对话片段 / 人格 / 分层 / 预算）。
///
/// 返回:
/// - 成功时返回 `context`（可直接拼接的段落文本）、`items`（结构化明细）与 `stats`。
pub(crate) async fn run(engine: &Engine, req: RecallRequest) -> RamariaResult<RecallResult> {
    let policy = engine.recall_policy();
    let persona = normalize_persona(req.persona.as_deref());
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "召回请求的人格不在可见白名单内，拒绝");
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可见白名单内（allowed_personas）"
        )));
    }

    let include = req.effective_include();
    let max_items = req.effective_max_items() as usize;
    let max_chars = req.effective_max_chars() as usize;

    let query = resolve_query(&req);
    if query.trim().is_empty() {
        tracing::debug!(persona = %persona, "召回无检索输入，进入概览模式");
        return overview(engine, &persona, &include, max_items, max_chars).await;
    }

    tracing::debug!(
        persona = %persona,
        layers = include.len(),
        max_items,
        max_chars,
        "召回检索模式开始"
    );
    search(
        engine, &policy, &persona, &include, &query, max_items, max_chars,
    )
    .await
}

// =========================================================
// 辅助
// =========================================================

/// 归一化人格 uid（空串视为缺省）。
fn normalize_persona(persona: Option<&str>) -> String {
    persona
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_PERSONA_UID)
        .to_string()
}

/// 解析检索输入：显式 `query` 优先，其次最后一条用户消息（与在线管线口径一致）。
pub(super) fn resolve_query(req: &RecallRequest) -> String {
    if let Some(query) = req.query.as_deref().map(str::trim) {
        if !query.is_empty() {
            return query.to_string();
        }
    }
    req.messages
        .iter()
        .rev()
        .find(|turn| turn.role == crate::types::ChatRole::User)
        .map(|turn| turn.content.trim().to_string())
        .or_else(|| {
            req.messages
                .last()
                .map(|turn| turn.content.trim().to_string())
        })
        .unwrap_or_default()
}
