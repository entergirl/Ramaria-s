//! crates/ramaria-service/src/idle.rs - 空闲检查用例（tick_idle 的服务层实现）
//!
//! 设计特点:
//! - 全库扫描：遍历 `sessions` 中**全部**活跃会话（含切换人格遗留在库的孤儿会话），
//!   而非仅当前活跃会话——与在线管线空闲检测线程同一口径
//! - 阈值口径：最后消息距今 > `[session].l1_idle_minutes`（默认 10 分钟）触发封存
//! - 抢占幂等：逐个走 `seal`（条件更新抢占），多进程同时扫描不会重复生成 L1
//! - 请求间节流：连续封存时按 `[thresholds].cluster_delay_ms` 间隔（避免触发远端 LLM 限流）
//! - 空会话跳过：无消息的会话不触发 LLM（保持既有语义）

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::now_ms;
use uuid::Uuid;

use crate::engine::Engine;

/// 执行一次空闲检查：对超时会话执行封存。
///
/// 流程:
/// 1. 列出全部活跃会话；
/// 2. 逐个读取最后消息时间（无消息 → 跳过）；
/// 3. 超过 `[session].l1_idle_minutes` → 走 `seal` 用例（内部抢占，防止重复摘要）；
/// 4. 每封存一个会话后按 `[thresholds].cluster_delay_ms` 节流。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 本次实际封存的会话数量（未抢到 / 未超时 / 空会话不计入）。
pub(crate) async fn tick(engine: &Engine) -> RamariaResult<usize> {
    let storage = engine.storage_ref().as_ref();
    let config = engine.config();
    let threshold_ms = config.session.l1_idle_minutes as i64 * 60_000;

    let sessions = storage.list_active_sessions().await?;
    if sessions.is_empty() {
        tracing::debug!("空闲检查：无活跃会话");
        return Ok(0);
    }

    let mut sealed = 0usize;
    for session in &sessions {
        // 单会话读取失败不中断整轮扫描（其余会话照常封存，失败者下轮重试）
        let last_active = match last_message_time(storage, session.id).await {
            Ok(time) => time,
            Err(e) => {
                tracing::warn!(
                    session_id = %session.id,
                    error = %e,
                    "空闲检查：读取最后消息时间失败，跳过该会话"
                );
                continue;
            }
        };
        let Some(last_active) = last_active else {
            tracing::debug!(session_id = %session.id, "空闲检查：会话无消息，跳过");
            continue;
        };
        let idle_ms = now_ms().saturating_sub(last_active);
        if idle_ms < threshold_ms {
            tracing::debug!(
                session_id = %session.id,
                idle_minutes = %format!("{:.1}", idle_ms as f64 / 60_000.0),
                "空闲检查：会话仍在活跃，未触发封存"
            );
            continue;
        }

        tracing::info!(
            session_id = %session.id,
            idle_minutes = %format!("{:.1}", idle_ms as f64 / 60_000.0),
            threshold_minutes = config.session.l1_idle_minutes,
            "空闲检查：会话超时，执行封存"
        );

        match crate::seal::run(engine, session.id).await {
            Ok(outcome) => {
                if outcome.sealed {
                    sealed += 1;
                    // 连续封存节流（共享 LLM 速率保护；间隔沿用 [thresholds].cluster_delay_ms）
                    ramaria_memory::llm_gate::inter_llm_delay(
                        config.thresholds.cluster_delay_ms,
                        "L1 空闲批量封存",
                    )
                    .await;
                }
            }
            // 单个会话封存失败不阻塞其余会话（LLM 不可用时下轮继续尝试）
            Err(e) => {
                tracing::error!(session_id = %session.id, error = %e, "空闲检查：封存失败，跳过该会话");
            }
        }
    }

    tracing::info!(
        checked = sessions.len(),
        sealed,
        "空闲检查完成（本轮封存 {sealed} 个会话）"
    );
    Ok(sealed)
}

/// 读取会话最后消息时间。
///
/// 降级:
/// - `get_last_message_time` 未覆写（Unsupported）→ 回退全量加载消息取最大值；
/// - 无消息 → `Ok(None)`。
async fn last_message_time(
    storage: &dyn StorageBackend,
    session_id: Uuid,
) -> RamariaResult<Option<i64>> {
    match storage.get_last_message_time(session_id).await {
        Ok(time) => Ok(time),
        Err(RamariaError::Unsupported { .. }) => {
            let messages = storage.list_messages(session_id).await?;
            Ok(messages.iter().map(|m| m.created_at).max())
        }
        Err(e) => Err(e),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, engine_with_l1_reply, seed_persona, seed_session_with_messages,
    };
    use ramaria_core::traits::StoreCrud;

    /// 3 个会话 2 个超时：只封存超时的 2 个，未超时的保持活跃。
    #[tokio::test]
    async fn tick_seals_only_expired_sessions() {
        let (engine, storage, dir) = engine_with_l1_reply("idle", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;

        // 阈值 10 分钟：20 分钟前 → 超时；刚刚 → 未超时
        let stale_base = now_ms() - 20 * 60_000;
        let stale_a = seed_session_with_messages(&storage, "char-0001", 2, stale_base).await;
        let stale_b =
            seed_session_with_messages(&storage, "char-0001", 2, stale_base + 5_000).await;
        let fresh = seed_session_with_messages(&storage, "char-0001", 2, now_ms()).await;

        let sealed = engine.tick_idle().await.expect("空闲检查应成功");
        assert_eq!(sealed, 2, "应封存 2 个超时会话");

        // 超时会话：已关闭 + 生成 L1
        let stale_session = storage
            .get_session(stale_a)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(stale_session.ended_at.is_some(), "超时会话应被关闭");
        assert_eq!(
            storage
                .list_memory_l1(stale_a)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "超时会话应生成 L1"
        );
        assert!(
            storage
                .get_session(stale_b)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在")
                .ended_at
                .is_some(),
            "第二个超时会话也应被关闭"
        );

        // 未超时会话：保持活跃
        let fresh_session = storage
            .get_session(fresh)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(fresh_session.ended_at.is_none(), "未超时会话不应被关闭");

        // 幂等：再跑一次无超时会话 → 0
        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无消息的空会话不触发封存（不调用 LLM）。
    #[tokio::test]
    async fn tick_skips_empty_sessions() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-empty", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话");

        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);
        let stored = storage
            .get_session(session.id)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(stored.ended_at.is_none(), "空会话应保持活跃");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无活跃会话 → 0（空库不报错）。
    #[tokio::test]
    async fn tick_without_sessions_returns_zero() {
        let (engine, _storage, dir) = engine_with_l1_reply("idle-none", L1_JSON_REPLY).await;
        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
