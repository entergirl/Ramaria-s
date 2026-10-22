//! crates/ramaria-service/src/lifecycle/l1/generate.rs - Ramaria 单段 L1 摘要生成
//!
//! 设计特点:
//! - 任务管理器包裹单段生成（指数退避重试），成功后从库读回
//! - token 预算从后端配置传播，失败时按错误语义上抛

use tracing::{debug, info, warn};
use uuid::Uuid;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::MemoryL1;
use ramaria_memory::job::{JobManager, JobResult, JobType};
use ramaria_memory::l1::{L1Summarizer, L1SummarizerConfig};

use crate::engine::Engine;

// =========================================================
// 单段摘要生成（内部辅助）
// =========================================================

/// 为指定会话生成单段 L1 摘要。
///
/// 实现要点:
/// - `persona_uid` / 前缀覆盖 / 多画像分发开关写入 `L1SummarizerConfig`；
/// - `max_tokens` 从 `backend_config` 传播并以下限钳制（防 chat 侧小预算截断结构化 JSON）；
/// - 经 `JobManager::execute_with_retry` 包裹执行（指数退避重试 + 任务可观测性）；
/// - 成功后从存储读回最后一条 L1（`JobManager` 不返回业务结果）。
///
/// 参数:
/// - `fanout_others`: 多画像分发开关（真时按块内参与者复制 L1 行）。
/// - `fanout_include_self`: 分发参与者含"我"（User 角色发言者）；
///   仅 `fanout_others` 为真时有意义。
///
/// 返回:
/// - 成功时返回刚写入的 L1；生成后仍读不到记录视为内部错误（Validation）。
pub(crate) async fn generate_l1_summary(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
    fanout_others: bool,
    fanout_include_self: bool,
) -> RamariaResult<MemoryL1> {
    let storage = engine.storage_ref().as_ref();
    let llm = engine.llm_ref();

    let mut summarizer_config = L1SummarizerConfig::default();
    if let Some(uid) = persona_uid {
        summarizer_config.persona_uid = Some(uid.to_string());
    }
    if let Some(prefix) = user_prefix {
        summarizer_config.user_prefix = prefix.to_string();
    }
    if let Some(prefix) = assistant_prefix {
        summarizer_config.assistant_prefix = prefix.to_string();
    }
    summarizer_config.fanout_others = fanout_others;
    summarizer_config.fanout_include_self = fanout_include_self;

    // 输出预算从 backend_config 传播并下限钳制（结构化 JSON 输出需要更大预算）
    if let Ok(Some(backend)) = storage.get_backend_config().await {
        let floor = summarizer_config.max_tokens;
        summarizer_config.max_tokens = backend.max_tokens.max(floor);
        debug!(
            max_tokens = summarizer_config.max_tokens,
            backend_max_tokens = backend.max_tokens,
            "L1 摘要 max_tokens 已从 backend_config 传播"
        );
    }

    let summarizer = L1Summarizer::new(llm.as_ref(), storage, summarizer_config);
    let job_manager = JobManager::new(storage, engine.job_manager_config());
    let payload = serde_json::json!({ "session_id": session_id.to_string() }).to_string();

    let result = job_manager
        .execute_with_retry(JobType::L1Summary, Some(&payload), None, || {
            summarize_with_summarizer(&summarizer, session_id)
        })
        .await;

    match result {
        Ok(_job_id) => {
            let l1_list = storage.list_memory_l1(session_id).await?;
            l1_list
                .into_iter()
                .last()
                .ok_or_else(|| RamariaError::validation("L1 摘要生成后无法读取"))
        }
        Err(e) => Err(e),
    }
}

/// 单段摘要生成的异步闭包（供 `JobManager::execute_with_retry` 使用）。
///
/// 说明:
/// - LLM 调用失败归类为可重试（网络波动 / 服务暂不可用）。
async fn summarize_with_summarizer(summarizer: &L1Summarizer<'_>, session_id: Uuid) -> JobResult {
    match summarizer.summarize_session(session_id).await {
        Ok(_l1) => {
            info!(%session_id, "L1 摘要生成成功");
            JobResult::Success
        }
        Err(e) => {
            warn!(%session_id, error = %e, "L1 摘要生成失败，将重试");
            JobResult::Retryable(e.to_string())
        }
    }
}
