//! crates/ramaria-desktop/src/commands/style.rs - 说话风格统计只读命令（M7）
//!
//! 设计特点:
//! - 只读展示 `persona_style_stats` 单行记录：样本量、五维统计 JSON、规则文本、
//!   规则来源与统计状态。
//! - 数据为统计参数/规则文本，不含原文消息（隐私红线，v1.7 口径）。
//! - SpeakingStyle 事实（persona_facts field=speaking_style，含样例/手工覆盖）由既有
//!   `get_facts` 命令返回，本命令不做重复查询。

use crate::DesktopState;
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示结构体
// =========================================================

/// 风格统计状态中文标签映射。
fn status_label(status: &str) -> &'static str {
    match status {
        "ready" => "就绪（可注入）",
        "no_significant" => "样本充足但无显著项",
        _ => "数据不足",
    }
}

/// 规则来源中文标签映射。
fn source_label(source: &str) -> &'static str {
    match source {
        "template" => "模板生成",
        "llm" => "LLM 增强",
        _ => "未生成",
    }
}

/// 说话风格统计只读视图。
#[derive(Debug, Clone, Serialize)]
pub struct StyleStatsView {
    /// 人格标识
    pub persona_uid: String,
    /// 统计样本量 n_p（消息条数）
    pub sample_count: u32,
    /// 全局基线池合并版本
    pub baseline_version: u32,
    /// 状态标识: insufficient / ready / no_significant
    pub status: String,
    /// 状态中文标签（供前端直接展示）
    pub status_label: String,
    /// 规则来源: none / template / llm
    pub rule_source: String,
    /// 规则来源中文标签
    pub rule_source_label: String,
    /// 自动风格规则文本（null = 未生成）
    pub rule_text: Option<String>,
    /// 五维统计 JSON 原始文本（前端解析展示）
    pub stats_json: String,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

// =========================================================
// get_style_stats — 说话风格统计
// =========================================================

/// 查询指定人格的说话风格统计（只读）。
///
/// 参数:
/// - `persona_uid`: 目标人格 UID。
///
/// 返回:
/// - 无记录时返回 None（空态，非错误）；记录存在返回完整统计视图。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_style_stats(
    state: State<'_, DesktopState>,
    persona_uid: String,
) -> Result<Option<StyleStatsView>, String> {
    if persona_uid.trim().is_empty() {
        return Err("人格 UID 不能为空".to_string());
    }

    let stats = state
        .app
        .storage()
        .get_style_stats(&persona_uid)
        .await
        .map_err(|e| format!("查询说话风格统计失败: {e}"))?;

    let view = stats.map(|s| {
        let status = s.status.as_str();
        let source = s.rule_source.as_str();
        StyleStatsView {
            persona_uid: s.persona_uid,
            sample_count: s.sample_count,
            baseline_version: s.baseline_version,
            status: status.to_string(),
            status_label: status_label(status).to_string(),
            rule_source: source.to_string(),
            rule_source_label: source_label(source).to_string(),
            rule_text: s.rule_text,
            stats_json: s.stats_json,
            updated_at: s.updated_at,
        }
    });

    tracing::debug!(%persona_uid, present = view.is_some(), "get_style_stats 完成");
    Ok(view)
}
