//! rust/crates/ramaria-app/src/session_lifecycle/idle.rs - 后台空闲检测线程（Thread A）
//!
//! 设计特点:
//! - 实现 `SessionLifecycle::spawn_idle_checker`，对齐 Python `SessionManager._idle_checker_loop`
//! - 每 60s 轮询活跃 session 的空闲时间，超过阈值自动调用 `save_and_close_session`
//! - 首次跳过一轮（给应用启动留缓冲），随后周期性检查
//! - 内存缓存最后活跃时间优先，缺失时降级到 DB 查询（`get_last_msg_time_from_db`）
//! - shutdown_flag 感知：收到停止信号后立即退出循环

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::now_ms;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::SessionLifecycle;

// =========================================================
// 空闲检测线程（Thread A）
// =========================================================

impl SessionLifecycle {
    /// 启动后台空闲检测线程（Thread A）。
    ///
    /// 对齐 Python `SessionManager._idle_checker_loop`。
    ///
    /// 逻辑:
    /// - 每 `config.session.idle_check_interval_seconds`（默认 60s）轮询
    /// - 若活跃 session 的最后消息时间距今超过 `config.session.l1_idle_minutes`（默认 10min）
    ///   → 自动调用 `save_and_close_session`
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider。
    ///
    /// 返回:
    /// - `tokio::task::JoinHandle<>`，供 shutdown 时等待。
    pub fn spawn_idle_checker(
        self: &Arc<Self>,
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
    ) -> tokio::task::JoinHandle<()> {
        let slf = Arc::clone(self);
        let interval_secs = self.config.session.idle_check_interval_seconds;
        let shutdown_flag = Arc::clone(&self.shutdown_flag);
        let idle_minutes_arc = Arc::clone(&self.idle_minutes);

        info!(
            interval_secs,
            idle_minutes = idle_minutes_arc.load(Ordering::Relaxed),
            "后台空闲检测线程启动（Thread A）"
        );

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs as u64));
            // 跳过首次立即触发（给应用启动留缓冲）
            ticker.tick().await;

            loop {
                ticker.tick().await;

                if shutdown_flag.load(Ordering::Relaxed) {
                    info!("空闲检测线程收到停止信号，退出");
                    return;
                }

                // 每轮 tick 读取最新阈值（T-V14-5-001 热更新：
                // 设置页保存后 set_idle_minutes 即时生效，无需重启）
                let idle_minutes = idle_minutes_arc.load(Ordering::Relaxed);

                let active_sid = match slf.get_active_session_id() {
                    Some(sid) => sid,
                    None => {
                        // 无活跃 session，无需检测
                        continue;
                    }
                };

                // 从内存缓存获取最后活跃时间（Python 从 DB 查）
                let last_active = match slf.last_active(active_sid) {
                    Some(t) => t,
                    None => {
                        // 内存缓存中没有，尝试从 DB 恢复
                        // 对齐 Python `database.get_last_message_time(session_id)`
                        match get_last_msg_time_from_db(storage.as_ref(), active_sid).await {
                            Ok(Some(t)) => {
                                slf.touch_session(active_sid);
                                t
                            }
                            Ok(None) => {
                                // 无消息的空 session，使用创建时间
                                debug!(%active_sid, "session 无消息，跳过空闲检测");
                                continue;
                            }
                            Err(e) => {
                                warn!(%active_sid, %e, "查询最后消息时间失败");
                                continue;
                            }
                        }
                    }
                };

                let now = now_ms();
                let idle_ms = now.saturating_sub(last_active);
                let idle_min = idle_ms as f64 / 60_000.0;

                if idle_min >= idle_minutes as f64 {
                    info!(
                        %active_sid,
                        idle_min = %format!("{:.1}", idle_min),
                        threshold_min = idle_minutes,
                        "session 空闲超时，自动关闭"
                    );

                    // 从活跃 session 读取 persona_uid（不再传 None）
                    let persona_uid = slf.get_active_session_persona_uid(storage.as_ref()).await;

                    if let Err(e) = slf
                        .save_and_close_session(
                            storage.as_ref(),
                            llm.as_ref(),
                            persona_uid.as_deref(),
                        )
                        .await
                    {
                        error!(%active_sid, %e, "自动关闭 session 失败");
                    }

                    // 请求间节流（L1/L2 共用，`[thresholds].cluster_delay_ms`）：
                    // 多个 session 同时超时会被连续封存（连续触发 L1 摘要 LLM 调用），
                    // 保持最小间隔避免触发远程 API 速率限制。
                    ramaria_memory::llm_gate::inter_llm_delay(
                        slf.config.thresholds.cluster_delay_ms,
                        "L1 空闲批量封存",
                    )
                    .await;
                } else {
                    debug!(
                        %active_sid,
                        idle_min = %format!("{:.1}", idle_min),
                        "session 仍在活跃，未触发空闲关闭"
                    );
                }
            }
        })
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// 从 DB 查询 session 最后消息时间（降级路径）。
///
/// 当内存缓存中没有记录时，降级到 DB 查询。
/// 对齐 Python `database.get_last_message_time(session_id)`。
///
/// 实现:
/// - 使用 `StorageBackend::get_last_message_time` — 高效 `SELECT MAX(created_at)` 聚合，
///   不再全量加载消息列表。
/// - 若 trait 实现未覆写（返回 None），回退到 `list_messages` 全量加载。
pub(super) async fn get_last_msg_time_from_db(
    storage: &dyn StorageBackend,
    session_id: Uuid,
) -> RamariaResult<Option<i64>> {
    // 优先使用高效的 MAX 聚合查询
    if let Some(time) = storage.get_last_message_time(session_id).await? {
        return Ok(Some(time));
    }
    // 降级：全量加载消息取最后时间（仅当 trait 未覆写时发生）
    let messages = storage.list_messages(session_id).await?;
    Ok(messages.iter().map(|m| m.created_at).max())
}
