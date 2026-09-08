//! crates/ramaria-desktop/src/commands/rules.rs - 行为规则管理 Tauri Commands（M7）
//!
//! 设计特点:
//! - 对接既有 `ramaria rule` 同一行为层用例（ramaria_app::commands::behavior），
//!   不新增后端语义，保证 CLI 与 GUI 行为一致。
//! - list / get / edit / enable / disable / evidence，**不提供 delete**（回归红线 7）。
//! - edit/disable 沿用行为层 S1 反馈与 Manual 强锚点语义（与 CLI 一致）。
//! - 返回前端友好视图；仅记录 id/条数等日志，不记录规则文本/原文。
//! - 隐私：evidence 只返回结构化脱敏字段（title/summary/paraphrase/keywords）。

use crate::DesktopState;
use ramaria_core::behavior::BehaviorRule;
use serde::Serialize;
use serde_json::Value as JsonValue;
use tauri::State;

// =========================================================
// 前端展示结构体
// =========================================================

/// 规则列表响应（含按 persona 过滤后总数）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleListResponse {
    pub persona_uid: String,
    pub total: usize,
    /// 规则序列化列表（字段与 BehaviorRule serde 一致，snake_case）
    pub rules: Vec<JsonValue>,
}

/// 规则证据链响应。
#[derive(Debug, Clone, Serialize)]
pub struct RuleEvidenceResponse {
    pub rule_id: i64,
    pub evidence: Vec<JsonValue>,
}

// =========================================================
// 序列化辅助
// =========================================================

/// 将行为规则序列化为前端 JSON（含嵌套 situation/params/evidence）。
fn rule_to_json(rule: &BehaviorRule) -> JsonValue {
    serde_json::to_value(rule).unwrap_or_else(|_| {
        // 仅理论不可达（BehaviorRule 全字段可序列化）；兜底返回含 id 的最小对象
        serde_json::json!({ "id": rule.id })
    })
}

/// 规则证据项序列化（结构字段含 title/summary/paraphrase，均为脱敏视图）。
fn evidence_item_to_json(item: &ramaria_app::commands::behavior::RuleEvidenceItem) -> JsonValue {
    serde_json::json!({
        "event_id": item.event_id,
        "weight": item.weight,
        "title": item.title,
        "summary": item.summary,
        "paraphrase": item.paraphrase,
        "keywords": item.keywords,
    })
}

/// 默认规则所属 persona（与全局默认一致；前端一般显式传 persona）。
const DEFAULT_RULE_PERSONA: &str = "rama-0001";

// =========================================================
// list_rules — 行为规则列表
// =========================================================

/// 列出指定人格的行为规则（含启用/禁用与自动/手工来源标记）。
///
/// 参数:
/// - `persona_uid`: 可选，人格标识（缺省使用默认人格）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_rules(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
) -> Result<RuleListResponse, String> {
    let persona_uid = persona_uid.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());
    let rules = ramaria_app::commands::behavior::behavior_list_rules(&state.app, &persona_uid)
        .await
        .map_err(|e| format!("查询行为规则失败: {e}"))?;

    let rules_json: Vec<JsonValue> = rules.iter().map(rule_to_json).collect();
    tracing::debug!(persona_uid = %persona_uid, total = rules_json.len(), "list_rules 完成");

    Ok(RuleListResponse {
        persona_uid,
        total: rules_json.len(),
        rules: rules_json,
    })
}

// =========================================================
// get_rule — 规则详情
// =========================================================

/// 查看单条行为规则详情。
///
/// 参数:
/// - `rule_id`: 规则 ID。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_rule(state: State<'_, DesktopState>, rule_id: i64) -> Result<JsonValue, String> {
    let rule = ramaria_app::commands::behavior::behavior_get_rule(&state.app, rule_id)
        .await
        .map_err(|e| format!("查询行为规则失败: {e}"))?
        .ok_or_else(|| format!("行为规则 {rule_id} 不存在"))?;

    Ok(rule_to_json(&rule))
}

// =========================================================
// set_rule_enabled — 启用 / 禁用
// =========================================================

/// 启用或禁用行为规则（禁用写 S1 反馈，语义与 CLI `rule disable/enable` 一致）。
///
/// 参数:
/// - `rule_id`: 规则 ID。
/// - `enabled`: true = 启用，false = 禁用。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_rule_enabled(
    state: State<'_, DesktopState>,
    rule_id: i64,
    enabled: bool,
) -> Result<JsonValue, String> {
    ramaria_app::commands::behavior::behavior_set_rule_enabled(&state.app, rule_id, enabled, None)
        .await
        .map_err(|e| format!("切换规则状态失败: {e}"))?;

    tracing::debug!(rule_id, enabled, "规则状态已切换");
    Ok(serde_json::json!({ "id": rule_id, "enabled": enabled }))
}

// =========================================================
// edit_rule — 手工编辑规则
// =========================================================

/// 编辑规则（reaction / avoid；与 CLI `rule edit` 一致：编辑后转 Manual 并写 S1）。
///
/// 参数:
/// - `rule_id`: 规则 ID。
/// - `reaction`: 可选，新的规则文本（不可为空；缺省保留原值）。
/// - `avoid`: 可选，新的禁忌列表（逗号分隔）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn edit_rule(
    state: State<'_, DesktopState>,
    rule_id: i64,
    reaction: Option<String>,
    avoid: Option<String>,
) -> Result<JsonValue, String> {
    if reaction.is_none() && avoid.is_none() {
        return Err("请至少提供 reaction 或 avoid 之一".to_string());
    }

    let mut rule = ramaria_app::commands::behavior::behavior_get_rule(&state.app, rule_id)
        .await
        .map_err(|e| format!("查询行为规则失败: {e}"))?
        .ok_or_else(|| format!("行为规则 {rule_id} 不存在"))?;

    if let Some(r) = reaction {
        let trimmed = r.trim().to_string();
        if trimmed.is_empty() {
            return Err("reaction 不能为空（候选规则请保留原状态，仅可编辑避免项）".to_string());
        }
        rule.reaction = Some(trimmed);
    }
    if let Some(a) = avoid {
        rule.avoid = a
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    ramaria_app::commands::behavior::behavior_edit_rule(&state.app, &mut rule, None)
        .await
        .map_err(|e| format!("编辑行为规则失败: {e}"))?;

    tracing::debug!(rule_id, "规则已编辑（转为 Manual）");
    Ok(rule_to_json(&rule))
}

// =========================================================
// rule_evidence — 规则证据链（只读）
// =========================================================

/// 查看规则证据链（规则 → 事件，仅返回脱敏结构化字段）。
///
/// 参数:
/// - `rule_id`: 规则 ID。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn rule_evidence(
    state: State<'_, DesktopState>,
    rule_id: i64,
) -> Result<RuleEvidenceResponse, String> {
    let items = ramaria_app::commands::behavior::behavior_rule_evidence(&state.app, rule_id)
        .await
        .map_err(|e| format!("查询规则证据失败: {e}"))?;

    let evidence: Vec<JsonValue> = items.iter().map(evidence_item_to_json).collect();
    tracing::debug!(rule_id, count = evidence.len(), "rule_evidence 完成");

    Ok(RuleEvidenceResponse { rule_id, evidence })
}

// =========================================================
// 单元测试（纯逻辑，不触库）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::behavior::{BehaviorParams, BehaviorSituation, RuleSource};

    // 规则 JSON 含核心展示字段（供前端直接消费）
    #[test]
    fn rule_json_contains_core_fields() {
        let rule = BehaviorRule {
            id: 7,
            persona_uid: "rama-0001".to_string(),
            situation: BehaviorSituation {
                keywords: vec!["难过".to_string()],
                ..BehaviorSituation::empty()
            },
            reaction: Some("用轻快的语气回应".to_string()),
            params: BehaviorParams::default(),
            avoid: vec!["说教".to_string()],
            evidence: Vec::new(),
            confidence: 0.9,
            stability: 0.8,
            source: RuleSource::Auto,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };

        let json = rule_to_json(&rule);
        assert_eq!(json["id"], 7);
        assert_eq!(json["persona_uid"], "rama-0001");
        assert_eq!(json["source"], "auto");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["reaction"], "用轻快的语气回应");
        assert_eq!(json["situation"]["keywords"][0], "难过");
        assert!(json["params"]["emotional_intensity"].is_f64());
        assert_eq!(json["avoid"][0], "说教");
    }

    // 候选规则（reaction=None）保持 null，前端据此渲染"仅参数注入"态
    #[test]
    fn rule_json_keeps_candidate_reaction_null() {
        let rule = BehaviorRule {
            id: 1,
            persona_uid: "rama-0001".to_string(),
            situation: BehaviorSituation::empty(),
            reaction: None,
            params: BehaviorParams::default(),
            avoid: Vec::new(),
            evidence: Vec::new(),
            confidence: 0.0,
            stability: 0.0,
            source: RuleSource::Auto,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(rule_to_json(&rule)["reaction"], JsonValue::Null);
    }
}
