//! crates/ramaria-service/src/lifecycle/l1/retry.rs - Ramaria L1 补扫与重试观察者
//!
//! 设计特点:
//! - 消费封存失败登记的重试类型任务，按单轮上限补跑
//! - 观察者只记计数与任务标识，不记摘要内容

use async_trait::async_trait;

use ramaria_core::types::MemoryL1;
use ramaria_memory::l1::{L1RetryObserver, L1RetryStats, MAX_L1_RETRY_JOBS_PER_RUN};

use crate::engine::Engine;

// =========================================================
// 补扫入口
// =========================================================

/// 补扫封存失败遗留的 L1 摘要任务，返回本轮成功补跑的任务数。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 本轮成功补跑出 L1 摘要的任务数（检视 / 尝试明细见
///   [`retry_pending_l1_jobs_with_stats`]）。
pub(crate) async fn retry_pending_l1_jobs(engine: &Engine) -> usize {
    retry_pending_l1_jobs_with_stats(engine).await.completed
}

/// 补扫封存失败遗留的 L1 摘要任务，返回本轮全量计数（scanned / attempted / completed）。
///
/// 说明:
/// - 未产出摘要 / LLM 仍不可用时任务保持 pending，下一轮自然收敛；
/// - 供需要完整计数日志的消费方（空闲检查）使用。
pub(crate) async fn retry_pending_l1_jobs_with_stats(engine: &Engine) -> L1RetryStats {
    let storage = engine.storage_ref().as_ref();
    let llm = engine.llm_ref();
    ramaria_memory::l1::retry_pending_l1_jobs(
        storage,
        llm.as_ref(),
        &engine.config().l1.progressive,
        MAX_L1_RETRY_JOBS_PER_RUN,
        &ServiceL1RetryObserver { engine },
        engine.job_manager_config(),
    )
    .await
}

// =========================================================
// L1 补扫宿主钩子
// =========================================================

/// L1 补扫宿主钩子：L1 增量镜像 + 注册的 L2 触发钩子。
///
/// 说明:
/// - 每段补跑成功的 L1 走服务层镜像增量（检索器 + 关键词镜像）；
/// - 级联检查走宿主注册的 `l2_trigger` 钩子，未注册时跳过（不阻塞补扫）。
struct ServiceL1RetryObserver<'a> {
    engine: &'a Engine,
}

#[async_trait]
impl L1RetryObserver for ServiceL1RetryObserver<'_> {
    async fn on_l1(&self, l1: &MemoryL1) {
        crate::index::index_l1_into_mirrors(self.engine, l1).await;
    }

    async fn on_cascade(&self, persona_uid: Option<&str>) {
        let hooks = self.engine.seal_hooks();
        crate::seal::run_hook(
            self.engine,
            &hooks.l2_trigger,
            persona_uid,
            "L2 触发检查（L1 补扫）",
        )
        .await;
    }
}
