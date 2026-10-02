//! crates/ramaria-service/src/lifecycle/l2_l3/schedule.rs - Ramaria L2/L3 后台定时调度
//!
//! 设计特点:
//! - 首轮检查延迟 `first_delay_seconds` 秒执行（避开宿主启动阶段），之后按 `interval_seconds` 周期检查
//! - 时间线阈值来自配置（`> 0` 才启用）：L1 超龄触发 L2，事件超龄触发 L3
//! - 每轮先消费封存失败遗留的 L1 补扫任务；无主 L1 按时间阈值归属并触发提取
//! - 停止位按 60 秒分片感知，任一片收到停止信号即退出
//! - 日志只记 persona 与计数，不记原文

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ramaria_core::types::now_ms;
use tracing::{debug, error, info, warn};

use crate::engine::Engine;

use super::l2::{run_l2_extraction, shutdown_requested};
use super::l3::run_l3_inference;
use super::unbound::process_unbound_l1_for_l2;

// =========================================================
// 后台定时任务：L2/L3 定时触发
// =========================================================

/// 启动后台 L2/L3 定时检查任务。
///
/// 逻辑:
/// - 首轮检查延迟 `first_delay_seconds` 秒执行（避开宿主启动阶段）；
/// - 之后每 `interval_seconds` 秒检查一轮，遍历所有 persona（时间线阈值来自配置，
///   `> 0` 才启用）：
///   - 最早未吸收 L1 超过 `[thresholds].l2_trigger_days` 天 → 触发 L2 事件提取
///   - 最早未吸收事件超过 `[thresholds].l3_trigger_days` 天 → 触发 L3 性格推断
/// - 停止位置位后退出；等待按 60 秒分片，每片感知一次停止位。
///
/// 参数:
/// - `engine`: 服务层引擎（`Arc` 共享）。
/// - `shutdown`: 宿主停止位（true = 循环应在下一轮退出）。
/// - `first_delay_seconds`: 首轮检查前的延迟秒数。
/// - `interval_seconds`: 两轮检查之间的间隔秒数。
pub(crate) fn spawn_scheduler(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
    first_delay_seconds: u64,
    interval_seconds: u64,
) -> tokio::task::JoinHandle<()> {
    info!(
        interval_seconds,
        first_delay_seconds, "后台 L2/L3 定时检查任务启动"
    );

    tokio::spawn(async move {
        // 首次延迟：避免在宿主启动阶段执行检查
        tokio::time::sleep(Duration::from_secs(first_delay_seconds)).await;

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("L2/L3 定时检查任务收到停止信号，退出");
                return;
            }

            // 执行定时检查
            run_scheduled_check(&engine, Some(&shutdown)).await;

            // 等待下一次检查（可中断，每 60s 检查一次停止信号）
            let mut sleep_secs = interval_seconds;
            while sleep_secs > 0 && !shutdown.load(Ordering::Relaxed) {
                let chunk = sleep_secs.min(60);
                tokio::time::sleep(Duration::from_secs(chunk)).await;
                sleep_secs = sleep_secs.saturating_sub(chunk);
            }
        }
    })
}

/// 执行一次性 L2/L3 定时检查。
///
/// 流程:
/// 1. L1 补扫：消费封存失败遗留的摘要任务；
/// 2. 遍历所有 persona（时间线阈值来自配置，`> 0` 才启用）：
///    - 最早未吸收 L1 超过 `[thresholds].l2_trigger_days` 天 → 触发 L2 事件提取（时间触发路径）；
///    - 最早未吸收事件超过 `[thresholds].l3_trigger_days` 天 → 触发 L3 性格推断（时间触发路径）；
/// 3. 无主 L1 按时间阈值（`[thresholds].l2_trigger_days`）归属并触发提取。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `shutdown`: 宿主停止位；`None` 表示调用方没有宿主循环（不中断）。
pub(crate) async fn run_scheduled_check(engine: &Engine, shutdown: Option<&AtomicBool>) {
    let storage = engine.storage_ref().as_ref();
    debug!("L2/L3 定时检查开始");

    // L1 补扫：消费封存时 L1 生成失败遗留的 pending 任务（LLM 恢复后自动补跑摘要），
    // 使"失败登记"不再空转（启动补扫之外的周期性消费点）。
    let l1_retried = crate::lifecycle::l1::retry_pending_l1_jobs(engine).await;
    if l1_retried > 0 {
        info!(l1_retried, "L2/L3 定时检查：L1 补扫完成");
    }

    let personas = match storage.list_personas().await {
        Ok(p) => p,
        Err(e) => {
            error!(%e, "L2/L3 定时检查：无法列出 persona");
            return;
        }
    };

    let now = now_ms();
    let ms_per_day: i64 = 86_400_000;
    // 时间线阈值来自配置：`> 0` 才启用该时间线（`0` = 不按时间触发；计数线不受影响）
    let l2_trigger_days = engine.config().thresholds.l2_trigger_days as f64;
    let l3_trigger_days = engine.config().thresholds.l3_trigger_days as f64;

    for persona in &personas {
        if shutdown_requested(shutdown) {
            return;
        }

        // ---- L2 时间触发 ----
        // 最早未吸收 L1 超过配置阈值天数则触发（阈值 > 0 才启用）
        match storage.list_unabsorbed_l1(&persona.uid).await {
            Ok(l1_list) => {
                if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
                    let age_days = (now - oldest) as f64 / ms_per_day as f64;
                    if l2_trigger_days > 0.0 && age_days >= l2_trigger_days {
                        info!(
                            persona_uid = %persona.uid,
                            %age_days,
                            l1_count = l1_list.len(),
                            trigger_days = l2_trigger_days,
                            "L2 定时触发（路径 B：最早未吸收 L1 超过阈值天数）"
                        );
                        // 定时路径也确定对话另一方
                        let other_name = if personas.len() == 2 {
                            personas
                                .iter()
                                .find(|p| p.uid != persona.uid)
                                .map(|p| p.name.clone())
                        } else {
                            None
                        };
                        run_l2_extraction(engine, shutdown, &persona.uid, other_name).await;
                    }
                }
            }
            Err(e) => {
                warn!(persona_uid = %persona.uid, %e, "L2 定时检查：查询 L1 失败");
            }
        }

        // ---- L3 时间触发 ----
        // 最早未吸收事件超过配置阈值天数则触发（阈值 > 0 才启用）
        match storage.list_unabsorbed_events(&persona.uid).await {
            Ok(events) => {
                if let Some(oldest) = events.iter().map(|e| e.start).min() {
                    let age_days = (now - oldest) as f64 / ms_per_day as f64;
                    if l3_trigger_days > 0.0 && age_days >= l3_trigger_days {
                        info!(
                            persona_uid = %persona.uid,
                            %age_days,
                            event_count = events.len(),
                            trigger_days = l3_trigger_days,
                            "L3 定时触发（路径 B：最早未吸收事件超过阈值天数）"
                        );
                        run_l3_inference(engine, &persona.uid).await;
                    }
                }
            }
            Err(e) => {
                warn!(persona_uid = %persona.uid, %e, "L3 定时检查：查询事件失败");
            }
        }
    }

    // ---- 无主 L1 时间触发（数据断层修复）----
    // 定时路径：最早无主 L1 年龄 ≥ 阈值时触发 L2 提取（导入数据同样适用），
    // 与 persona 循环的时间触发（路径 B）语义一致。
    let unbound_stats = process_unbound_l1_for_l2(
        engine,
        shutdown,
        0,
        engine.config().thresholds.l2_trigger_days as f64,
    )
    .await;
    info!(
        ?unbound_stats,
        "L2/L3 定时检查：无主 L1 处理完成（归属 {} / 无法归属 {} / 触发组 {} / 待下次组 {}）",
        unbound_stats.attributed,
        unbound_stats.unattributable,
        unbound_stats.triggered_personas,
        unbound_stats.pending_groups
    );

    debug!("L2/L3 定时检查完成");
}
