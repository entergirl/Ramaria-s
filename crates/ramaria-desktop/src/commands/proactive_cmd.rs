//! crates/ramaria-desktop/src/commands/proactive_cmd.rs - 主动消息名单 Tauri Commands
//!
//! 设计特点:
//! - 一对读写命令：名单读取（活跃人格开关状态与生效结论）与开关保存
//! - 委托服务层名单用例，桌面只做视图映射与错误文案转换
//! - 生效结论由服务层单点计算，前端不重算
//! - 日志只记人格标识与开关值等元数据，不记消息内容

use std::collections::BTreeMap;

use crate::DesktopState;
use ramaria_service::ProactiveStatsReport;
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 主动消息名单条目（前端展示用）。
///
/// 字段约定:
/// - `uid` / `name` / `kind`: 人格身份；
/// - `has_local_dialogue`: 本地用户消息存在性（自动态解锁依据，排除导入）；
/// - `mode`: 三态文本 auto / on / off；
/// - `effective`: 生效结论（服务层单点计算）。
#[derive(Debug, Clone, Serialize)]
pub struct ProactivePersonaRow {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub has_local_dialogue: bool,
    pub mode: String,
    pub effective: bool,
}

/// 由服务层名单视图构造前端条目（字段映射的唯一入口，便于单测锁定）。
///
/// 参数:
/// - `item`: 服务层名单条目。
///
/// 返回:
/// - 前端展示用条目（六字段透传，不做任何重算）。
fn row_view(item: ramaria_service::ProactivePersonaView) -> ProactivePersonaRow {
    ProactivePersonaRow {
        uid: item.uid,
        name: item.name,
        kind: item.kind,
        has_local_dialogue: item.has_local_dialogue,
        mode: item.mode,
        effective: item.effective,
    }
}

// =========================================================
// list_proactive_personas — 读取主动消息名单
// =========================================================

/// 列出活跃人格的主动开关状态与生效结论。
///
/// 返回:
/// - JSON 数组，每项为 ProactivePersonaRow（按人格列表顺序）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_proactive_personas(
    state: State<'_, DesktopState>,
) -> Result<Vec<ProactivePersonaRow>, String> {
    let views = state
        .engine
        .proactive_persona_list()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询主动消息名单失败"))?;

    let rows: Vec<ProactivePersonaRow> = views.into_iter().map(row_view).collect();

    tracing::debug!(count = rows.len(), "list_proactive_personas 完成");
    Ok(rows)
}

// =========================================================
// set_proactive_persona — 保存主动开关
// =========================================================

/// 保存指定人格的主动开关（三态）。
///
/// 参数:
/// - `uid`: 人格业务标识。
/// - `mode`: 开关文本（auto / on / off）。
///
/// 返回:
/// - 成功固定返回 `"ok"`；校验 / 存储失败返回用户可读错误文案。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_proactive_persona(
    state: State<'_, DesktopState>,
    uid: String,
    mode: String,
) -> Result<String, String> {
    state
        .engine
        .proactive_persona_set_mode(&uid, &mode)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "保存主动开关失败"))?;

    tracing::info!(%uid, %mode, "set_proactive_persona 完成");
    Ok("ok".to_string())
}

// =========================================================
// 投递概览视图（设置页「主动对话」页）
// =========================================================

/// 单日投递计数（全人格按本地日期聚合）。
#[derive(Debug, Clone, Serialize)]
pub struct ProactiveDailyRowView {
    pub date: String,
    pub count: u32,
}

/// 投递与回应合计（回应窗口 0 = 不限；比率为空表示无投递）。
#[derive(Debug, Clone, Serialize)]
pub struct ProactiveTotalsView {
    pub personas: u32,
    pub deliveries: u32,
    pub responded: u32,
    pub response_rate: Option<f64>,
    pub median_response_ms: Option<i64>,
}

/// 投递概览视图（只含设置页可展示、且后端可观测的字段）。
///
/// 字段约定:
/// - `daily_limit` / `daily_total_limit` / `cooldown_hours`: 取统计时的 `[proactive]` 快照；
/// - `daily_count` / `daily_date`: 全局记账（当日投递数与日期，非当日时前端按 0 处理）；
/// - `daily`: 全人格按日期升序聚合，供前端计算今日 / 本周；
/// - `last_sent_at` / `silence_streak_max` / `judge_*`: 分人格状态取最大值或合计；
/// - `totals`: 投递 / 回应 / 比率 / 中位响应时间（窗口 0 = 不限）。
///
/// 说明:
/// - 调度内部状态（是否等待空闲、下次窗口）不可观测，本视图不含伪字段。
#[derive(Debug, Clone, Serialize)]
pub struct ProactiveOverviewView {
    pub generated_at: i64,
    pub daily_limit: u32,
    pub daily_total_limit: u32,
    pub daily_count: u32,
    pub daily_date: String,
    pub daily: Vec<ProactiveDailyRowView>,
    pub last_sent_at: Option<i64>,
    pub cooldown_hours: u32,
    pub silence_streak_max: u32,
    pub judge_yes_count: u32,
    pub judge_no_count: u32,
    pub last_judge_at: Option<i64>,
    pub totals: ProactiveTotalsView,
}

/// 由数值基线报告聚合投递概览（字段映射的唯一入口，便于单测锁定）。
pub(crate) fn overview_view(report: &ProactiveStatsReport) -> ProactiveOverviewView {
    let mut daily: BTreeMap<String, u32> = BTreeMap::new();
    let mut last_sent_at: Option<i64> = None;
    let mut last_judge_at: Option<i64> = None;
    let mut silence_streak_max = 0_u32;

    for persona in &report.personas {
        for row in &persona.daily {
            *daily.entry(row.date.clone()).or_insert(0) += row.count;
        }
        last_sent_at = last_sent_at.max(persona.last_sent_at);
        last_judge_at = last_judge_at.max(persona.last_judge_at);
        silence_streak_max = silence_streak_max.max(persona.silence_streak);
    }

    ProactiveOverviewView {
        generated_at: report.generated_at,
        daily_limit: report.config.daily_limit,
        daily_total_limit: report.config.daily_total_limit,
        daily_count: report.global.daily_count,
        daily_date: report.global.daily_date.clone(),
        daily: daily
            .into_iter()
            .map(|(date, count)| ProactiveDailyRowView { date, count })
            .collect(),
        last_sent_at,
        cooldown_hours: report.config.cooldown_hours,
        silence_streak_max,
        judge_yes_count: report.totals.judge_yes_count,
        judge_no_count: report.totals.judge_no_count,
        last_judge_at,
        totals: ProactiveTotalsView {
            personas: report.totals.personas,
            deliveries: report.totals.deliveries,
            responded: report.totals.responded,
            response_rate: report.totals.response_rate,
            median_response_ms: report.totals.median_response_ms,
        },
    }
}

// =========================================================
// get_proactive_stats — 投递概览
// =========================================================

/// 读取主动对话投递概览（只读，不触发调度）。
///
/// 返回:
/// - 聚合后的概览视图；任一分人格统计失败上抛用户可读文案。
///
/// 说明:
/// - 回应统计窗口取 0（不限），与调度「已回应」判定同口径；
/// - 统计基于既有 `messages` 表与主动状态键，不新增表、不写库。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_proactive_stats(
    state: State<'_, DesktopState>,
) -> Result<ProactiveOverviewView, String> {
    let report = state
        .engine
        .proactive_stats(0)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "读取主动对话统计失败"))?;

    tracing::debug!(personas = report.personas.len(), "get_proactive_stats 完成");
    Ok(overview_view(&report))
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 映射：服务层视图六字段逐项透传（生效结论由服务层计算，前端不重算）。
    #[test]
    fn row_view_maps_fields() {
        let view = ramaria_service::ProactivePersonaView {
            uid: "char-0001".to_string(),
            name: "测试人格".to_string(),
            kind: "char".to_string(),
            has_local_dialogue: true,
            mode: "on".to_string(),
            effective: true,
        };

        let row = row_view(view);
        assert_eq!(row.uid, "char-0001");
        assert_eq!(row.name, "测试人格");
        assert_eq!(row.kind, "char");
        assert!(row.has_local_dialogue, "对话解锁状态应透传");
        assert_eq!(row.mode, "on");
        assert!(row.effective, "生效结论应透传");
    }

    /// 构造样例报告：两名人格的日投递、最近投递与判据状态可区分。
    fn sample_report() -> ProactiveStatsReport {
        let config = serde_json::to_value(ramaria_core::config::ProactiveConfig::default())
            .expect("配置应可序列化");
        serde_json::from_value(serde_json::json!({
            "generated_at": 1_700_000_000_000_i64,
            "window_hours": 0,
            "config": config,
            "global": { "daily_total_limit": 6, "daily_count": 2, "daily_date": "2026-10-26" },
            "personas": [
                {
                    "uid": "char-0001",
                    "name": "甲",
                    "kind": "char",
                    "deliveries": 2,
                    "responded": 1,
                    "response_rate": 0.5,
                    "median_response_ms": 1000,
                    "daily": [
                        { "date": "2026-10-25", "count": 1 },
                        { "date": "2026-10-26", "count": 1 }
                    ],
                    "last_sent_at": 100,
                    "daily_count": 1,
                    "daily_date": "2026-10-26",
                    "silence_streak": 1,
                    "judge_yes_count": 2,
                    "judge_no_count": 1,
                    "last_judge_at": 200,
                    "first_seen_at": 50
                },
                {
                    "uid": "char-0002",
                    "name": "乙",
                    "kind": "char",
                    "deliveries": 1,
                    "responded": 0,
                    "response_rate": null,
                    "median_response_ms": null,
                    "daily": [{ "date": "2026-10-26", "count": 3 }],
                    "last_sent_at": 300,
                    "daily_count": 3,
                    "daily_date": "2026-10-26",
                    "silence_streak": 4,
                    "judge_yes_count": 0,
                    "judge_no_count": 2,
                    "last_judge_at": null,
                    "first_seen_at": null
                }
            ],
            "totals": {
                "personas": 2,
                "deliveries": 3,
                "responded": 1,
                "response_rate": 0.333,
                "median_response_ms": 1000,
                "judge_yes_count": 2,
                "judge_no_count": 3
            }
        }))
        .expect("样例报告应可反序列化")
    }

    /// 聚合：日投递按日期求和升序，最近投递 / 审慎状态取最大，合计透传。
    #[test]
    fn overview_aggregates_daily_and_latest() {
        let report = sample_report();
        let view = overview_view(&report);

        let daily: Vec<(&str, u32)> = view
            .daily
            .iter()
            .map(|row| (row.date.as_str(), row.count))
            .collect();
        assert_eq!(
            daily,
            vec![("2026-10-25", 1), ("2026-10-26", 4)],
            "日投递应按日期升序聚合"
        );
        assert_eq!(view.last_sent_at, Some(300), "最近投递取最大值");
        assert_eq!(view.last_judge_at, Some(200), "最近判据时间取最大值");
        assert_eq!(view.silence_streak_max, 4, "沉默计数取最大值");
        assert_eq!(view.daily_count, 2, "全局当日计数透传");
        assert_eq!(view.daily_date, "2026-10-26");
        assert_eq!(view.totals.deliveries, 3);
        assert_eq!(view.totals.responded, 1);
        assert_eq!(view.totals.response_rate, Some(0.333));
        assert_eq!(view.totals.median_response_ms, Some(1000));
        assert_eq!(view.judge_yes_count, 2);
        assert_eq!(view.judge_no_count, 3);
        assert_eq!(view.generated_at, 1_700_000_000_000);
    }

    /// 概览字段取配置快照：日上限 / 合计上限 / 冷却与 `[proactive]` 默认一致。
    #[test]
    fn overview_reads_limits_from_config_snapshot() {
        let report = sample_report();
        let view = overview_view(&report);
        let config = ramaria_core::config::ProactiveConfig::default();

        assert_eq!(view.daily_limit, config.daily_limit);
        assert_eq!(view.daily_total_limit, config.daily_total_limit);
        assert_eq!(view.cooldown_hours, config.cooldown_hours);
    }

    /// 序列化口径：snake_case 字段名（前端 DTO 手写对齐）。
    #[test]
    fn overview_serializes_snake_case() {
        let view = overview_view(&sample_report());
        let json = serde_json::to_value(&view).expect("概览应可序列化");
        assert!(json.get("daily_total_limit").is_some(), "{json}");
        assert!(json.get("silence_streak_max").is_some(), "{json}");
        assert!(json.get("dailyTotalLimit").is_none(), "不应出现驼峰字段");
    }
}
