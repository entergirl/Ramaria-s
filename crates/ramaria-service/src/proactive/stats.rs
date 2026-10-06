//! crates/ramaria-service/src/proactive/stats.rs - Ramaria 主动对话数值基线统计模块
//!
//! 设计特点:
//! - 只读统计：投递（主动消息）/ 回应（窗口内首条本地用户消息）/ 判据裁决计数三组指标
//! - 口径单点：回应判定沿用"本地用户消息"口径（排除导入），窗口由调用方给出（0 = 不限）
//! - 数据来源全部为既有存储（`messages` 表 + 主动状态键），不引入新表
//! - 判据计数为累计值（无时间维度）；投递另按本地日期分桶供日频观测
//! - 报告随取数快照携带 `[proactive]` 配置口径，供数值回归对照
//! - 日志只记人格数与投递 / 判据计数等元数据，不记消息内容

use std::collections::BTreeMap;

use ramaria_core::config::ProactiveConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ProactiveDeliveryPair, StorageBackend};
use ramaria_core::types::now_ms;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;

use super::state;

// =========================================================
// 报告形态
// =========================================================

/// 单日投递计数（本地日期分桶）。
///
/// 字段约定:
/// - `date`: 本地日期（`YYYY-MM-DD`）；
/// - `count`: 该日主动投递条数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProactiveDailyCount {
    pub date: String,
    pub count: u32,
}

/// 单人格数值基线条目。
///
/// 字段约定:
/// - `deliveries` / `responded` / `response_rate` / `median_response_ms`: 投递与回应
///   口径（回应 = 窗口内首条本地用户消息；无投递时比率为 None）；
/// - `daily`: 按本地日期升序的投递计数分桶；
/// - 其余字段为状态键原值（`last_sent_at` / 当日计数 / 退避计数 / 判据 yes-no 累计
///   与最近判据时间 / 宽限基准）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProactivePersonaStats {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub deliveries: u32,
    pub responded: u32,
    pub response_rate: Option<f64>,
    pub median_response_ms: Option<i64>,
    pub daily: Vec<ProactiveDailyCount>,
    pub last_sent_at: Option<i64>,
    pub daily_count: u32,
    pub daily_date: String,
    pub silence_streak: u32,
    pub judge_yes_count: u32,
    pub judge_no_count: u32,
    pub last_judge_at: Option<i64>,
    pub first_seen_at: Option<i64>,
}

/// 全人格合计（投递 / 回应 / 判据计数；中位数按全部回应样本计算）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProactiveStatsTotals {
    pub personas: u32,
    pub deliveries: u32,
    pub responded: u32,
    pub response_rate: Option<f64>,
    pub median_response_ms: Option<i64>,
    pub judge_yes_count: u32,
    pub judge_no_count: u32,
}

/// 全局记账口径（`proactive.state.global` + 日上限配置）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProactiveGlobalStats {
    pub daily_total_limit: u32,
    pub daily_count: u32,
    pub daily_date: String,
}

/// 主动对话数值基线报告。
///
/// 字段约定:
/// - `generated_at`: 取数时间（Unix 毫秒）；
/// - `window_hours`: 回应判定窗口（小时；0 = 不限）；
/// - `config`: 取数时的 `[proactive]` 配置快照（数值回归的对照口径）；
/// - `global` / `personas` / `totals`: 全局记账 + 分人格条目 + 合计。
///
/// 说明:
/// - 配置快照（`ProactiveConfig`）不实现 `PartialEq`，报告整体以字段对照而非相等比较使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveStatsReport {
    pub generated_at: i64,
    pub window_hours: u32,
    pub config: ProactiveConfig,
    pub global: ProactiveGlobalStats,
    pub personas: Vec<ProactivePersonaStats>,
    pub totals: ProactiveStatsTotals,
}

// =========================================================
// 统计入口
// =========================================================

/// 采集主动对话数值基线（只读）。
///
/// 流程:
/// 1. 读取 `[proactive]` 配置快照与全局状态键（日上限记账）；
/// 2. 列出全部人格，逐人格取"投递 + 窗口内首条本地用户回应"配对与运行时状态键；
/// 3. 汇总分人格条目与全人格合计（比率 / 中位数在样本为空时为 None）。
///
/// 参数:
/// - `engine`: 服务层引擎（配置快照与存储句柄来源）。
/// - `window_hours`: 回应判定窗口（小时；0 = 不设上界，与调度"已回应"判定同口径）。
///
/// 返回:
/// - 按存储层人格列表排序的统计报告；任一查询失败上抛。
pub(crate) async fn collect(
    engine: &Engine,
    window_hours: u32,
) -> RamariaResult<ProactiveStatsReport> {
    let config = engine.config().proactive.clone();
    let daily_total_limit = config.daily_total_limit;
    let storage: &dyn StorageBackend = engine.storage_ref().as_ref();
    let window_ms = i64::from(window_hours).saturating_mul(3_600_000);
    let global_state = state::load_global_state(storage).await?;
    let personas = storage.list_personas().await?;

    let mut rows = Vec::with_capacity(personas.len());
    let mut totals = ProactiveStatsTotals::default();
    let mut all_latencies: Vec<i64> = Vec::new();

    for persona in personas {
        let pairs = storage
            .list_proactive_delivery_pairs(&persona.uid, window_ms)
            .await?;
        let st = state::load_state(storage, &persona.uid).await?;

        let deliveries = pairs.len() as u32;
        let mut latencies: Vec<i64> = Vec::with_capacity(pairs.len());
        for pair in &pairs {
            if let Some(responded_at) = pair.responded_at {
                latencies.push(responded_at.saturating_sub(pair.sent_at));
            }
        }
        let responded = latencies.len() as u32;

        totals.personas = totals.personas.saturating_add(1);
        totals.deliveries = totals.deliveries.saturating_add(deliveries);
        totals.responded = totals.responded.saturating_add(responded);
        totals.judge_yes_count = totals.judge_yes_count.saturating_add(st.judge_yes_count);
        totals.judge_no_count = totals.judge_no_count.saturating_add(st.judge_no_count);
        all_latencies.extend(&latencies);

        rows.push(ProactivePersonaStats {
            uid: persona.uid,
            name: persona.name,
            kind: persona.kind.as_str().to_string(),
            deliveries,
            responded,
            response_rate: ratio(responded, deliveries),
            median_response_ms: median(&latencies),
            daily: daily_counts(&pairs),
            last_sent_at: st.last_sent_at,
            daily_count: st.daily_count,
            daily_date: st.daily_date,
            silence_streak: st.silence_streak,
            judge_yes_count: st.judge_yes_count,
            judge_no_count: st.judge_no_count,
            last_judge_at: st.last_judge_at,
            first_seen_at: st.first_seen_at,
        });
    }

    totals.response_rate = ratio(totals.responded, totals.deliveries);
    totals.median_response_ms = median(&all_latencies);

    tracing::debug!(
        personas = totals.personas,
        deliveries = totals.deliveries,
        responded = totals.responded,
        judge_yes = totals.judge_yes_count,
        judge_no = totals.judge_no_count,
        "主动对话数值基线：采集完成"
    );

    Ok(ProactiveStatsReport {
        generated_at: now_ms(),
        window_hours,
        config,
        global: ProactiveGlobalStats {
            daily_total_limit,
            daily_count: global_state.daily_count,
            daily_date: global_state.daily_date,
        },
        personas: rows,
        totals,
    })
}

// =========================================================
// 汇总辅助
// =========================================================

/// 回应比率（分母为 0 时返回 None，不产出无意义的 0 / 0）。
fn ratio(numerator: u32, denominator: u32) -> Option<f64> {
    (denominator > 0).then(|| f64::from(numerator) / f64::from(denominator))
}

/// 回应延迟中位数（毫秒；空样本为 None；偶数样本取中间两值均值）。
fn median(values: &[i64]) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        return Some(sorted[mid]);
    }
    // i128 中间量：避免两值相加溢出
    let sum = i128::from(sorted[mid - 1]) + i128::from(sorted[mid]);
    Some((sum / 2) as i64)
}

/// 按本地日期分桶投递计数（日期升序；超范围时间戳安全跳过）。
fn daily_counts(pairs: &[ProactiveDeliveryPair]) -> Vec<ProactiveDailyCount> {
    let mut buckets: BTreeMap<String, u32> = BTreeMap::new();
    for pair in pairs {
        let date = state::local_date_str(pair.sent_at);
        if date.is_empty() {
            continue;
        }
        let entry = buckets.entry(date).or_insert(0);
        *entry = entry.saturating_add(1);
    }
    buckets
        .into_iter()
        .map(|(date, count)| ProactiveDailyCount { date, count })
        .collect()
}

#[cfg(test)]
mod tests;
