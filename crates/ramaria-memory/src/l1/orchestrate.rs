//! crates/ramaria-memory/src/l1/orchestrate.rs - L1 摘要生成编排与失败任务补扫（共用实现）
//!
//! 设计特点:
//! - 与传输无关：桌面（app 生命周期线程）与 MCP 宿主（服务层空闲检查）共用同一份
//!   生成编排与补扫逻辑，避免两条路径行为漂移（D-V21-012 同源原则）
//! - 生成编排：`backend_config` 的 `max_tokens` 传播（下限钳制，防截断结构化 JSON）
//!   → 渐进式摘要（未触发阈值时回退整会话摘要）→ `JobManager` 包裹（指数退避与任务可观测性）
//!   → 读回本会话全部 L1
//! - 补扫幂等：payload 非法 / 会话无消息 → 标记完成；会话已有 L1 → 标记完成不重复调 LLM；
//!   补跑失败 → 保持 pending 待下一轮（LLM 恢复后自然收敛）
//! - 消费去重（双保险）：① 只消费补偿登记类型 `l1_summary_retry`——在途生成任务
//!   `l1_summary` 创建后有短暂 pending 窗口，按类型误取会导致同一会话重复生成摘要；
//!   ② 逐条原子抢占（`pending` → `running` 条件更新）——多消费方并发补扫同一任务时只有一方执行；
//! - 单轮限额：最多重试 `max_per_run` 条，避免一次扫描长时间占用宿主线程
//! - 宿主钩子：L1 增量镜像与级联检查由调用方注入（桌面与服务层索引实现不同，见 `L1RetryObserver`）
//! - 隐私：日志只记 job_id / session_id 与计数，不记摘要内容

use async_trait::async_trait;
use ramaria_core::config::L1ProgressiveConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::MemoryL1;
use tracing::{info, warn};
use uuid::Uuid;

use super::{L1Summarizer, L1SummarizerConfig};
use crate::job::{JobManager, JobResult, JobType};

/// 单轮补扫最多重试的 L1 任务数（防止一次扫描长时间占用宿主线程）。
pub const MAX_L1_RETRY_JOBS_PER_RUN: usize = 8;

// =========================================================
// 生成编排（共用）
// =========================================================

/// L1 生成请求（会话与归属信息）。
///
/// 字段约定:
/// - `persona_uid`: L1 归属人格；`None` 表示不绑定（存量兼容）。
/// - `user_prefix` / `assistant_prefix`: 摘要素材的角色前缀覆盖；`None` 用默认。
/// - `fanout_others`: 多画像分发开关（群聊场景按块/段内他人发言者复制 L1 行）；默认关闭。
#[derive(Debug, Clone, Copy)]
pub struct L1GenerateRequest<'a> {
    pub session_id: Uuid,
    pub persona_uid: Option<&'a str>,
    pub user_prefix: Option<&'a str>,
    pub assistant_prefix: Option<&'a str>,
    /// 多画像分发开关（按块/段内他人发言者复制 L1 行）
    pub fanout_others: bool,
}

impl<'a> L1GenerateRequest<'a> {
    /// 以会话与人格构造请求（前缀使用默认，多画像分发关闭）。
    pub fn new(session_id: Uuid, persona_uid: Option<&'a str>) -> Self {
        Self {
            session_id,
            persona_uid,
            user_prefix: None,
            assistant_prefix: None,
            fanout_others: false,
        }
    }
}

/// 生成会话 L1 摘要（渐进式感知；桌面封存与服务层封存共用同一份编排）。
///
/// 流程:
/// 1. 组装 `L1SummarizerConfig`（人格 / 前缀覆盖）并从 `backend_config` 传播 `max_tokens`
///    （以下限钳制，防止 chat 侧的小预算截断结构化 JSON）；
/// 2. 经 `JobManager::execute_with_retry` 调用渐进式摘要（未达阈值时内部回退整会话摘要）；
/// 3. 成功后读回本会话全部 L1（渐进式场景含多段）。
///
/// 返回:
/// - 成功时返回本会话的全部 L1（顺序与库内一致）；摘要生成后仍读不到记录视为内部错误。
pub async fn generate_l1_summaries(
    storage: &dyn StorageBackend,
    llm: &dyn LlmProvider,
    progressive: &L1ProgressiveConfig,
    req: L1GenerateRequest<'_>,
) -> RamariaResult<Vec<MemoryL1>> {
    let mut summarizer_config = L1SummarizerConfig::default();
    if let Some(uid) = req.persona_uid {
        summarizer_config.persona_uid = Some(uid.to_string());
    }
    if let Some(prefix) = req.user_prefix {
        summarizer_config.user_prefix = prefix.to_string();
    }
    if let Some(prefix) = req.assistant_prefix {
        summarizer_config.assistant_prefix = prefix.to_string();
    }
    // 多画像分发开关透传（群聊场景；关闭时行为与私聊一致）
    summarizer_config.fanout_others = req.fanout_others;

    // L1 输出预算从 backend_config 传播（下限钳制到 L1 默认值，防结构化 JSON 被截断）
    match storage.get_backend_config().await {
        Ok(Some(backend)) => {
            let floor = summarizer_config.max_tokens;
            summarizer_config.max_tokens = backend.max_tokens.max(floor);
            tracing::debug!(
                max_tokens = summarizer_config.max_tokens,
                "L1 摘要 max_tokens 已从 backend_config 传播"
            );
        }
        Ok(None) => {
            // 未配置 backend：属正常路径（首次运行 / 默认配置），用 L1 默认预算即可
            tracing::debug!("backend_config 未配置，L1 摘要使用默认输出预算");
        }
        Err(e) => {
            // 读取失败不阻塞摘要（仍按默认预算继续），但必须留痕便于排查
            warn!(
                error = %e,
                max_tokens = summarizer_config.max_tokens,
                "读取 backend_config 失败，L1 摘要使用默认输出预算"
            );
        }
    }

    let session_id = req.session_id;
    let summarizer = L1Summarizer::new(llm, storage, summarizer_config);

    // 渐进式配置为快照：闭包需要 'static 数据，clone 后移入
    let progressive_cfg = progressive.clone();
    let job_manager = JobManager::with_defaults(storage);
    let payload = serde_json::json!({ "session_id": session_id.to_string() }).to_string();

    let result = job_manager
        .execute_with_retry(JobType::L1Summary, Some(&payload), None, || {
            summarize_progressive(&summarizer, session_id, progressive_cfg.clone())
        })
        .await;

    match result {
        Ok(_job_id) => {
            // 摘要已写入存储：读回本会话全部 L1（渐进式场景含多段）
            let l1_list = storage.list_memory_l1(session_id).await?;
            if l1_list.is_empty() {
                Err(RamariaError::validation("L1 摘要生成后无法读取"))
            } else {
                Ok(l1_list)
            }
        }
        Err(e) => Err(e),
    }
}

/// 渐进式摘要生成闭包（供 `JobManager::execute_with_retry` 使用）。
///
/// 说明:
/// - LLM 调用失败归类为可重试（网络波动 / 服务暂不可用）；
/// - 成功但内容为空也视为成功（该会话确实无可用摘要素材）。
async fn summarize_progressive(
    summarizer: &L1Summarizer<'_>,
    session_id: Uuid,
    progressive: L1ProgressiveConfig,
) -> JobResult {
    match summarizer
        .summarize_progressive(session_id, &progressive)
        .await
    {
        Ok(l1_list) => {
            info!(%session_id, l1_count = l1_list.len(), "L1 摘要生成成功");
            JobResult::Success
        }
        Err(e) => {
            warn!(%session_id, error = %e, "L1 摘要生成失败，将重试");
            JobResult::Retryable(e.to_string())
        }
    }
}

// =========================================================
// 失败任务补扫（共用）
// =========================================================

/// 补扫计数（宿主日志与测试断言使用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct L1RetryStats {
    /// 本轮检视的 `l1_summary` pending 任务数（含跳过项）。
    pub scanned: usize,
    /// 本轮实际发起摘要重试的任务数。
    pub attempted: usize,
    /// 本轮成功补跑出 L1 摘要的任务数。
    pub completed: usize,
}

/// 补扫宿主钩子（未覆写的方法为空操作）。
///
/// 实现要求（与封存钩子同口径）:
/// - 不得 panic；不得长时间阻塞（补扫在宿主后台线程内执行）。
#[async_trait]
pub trait L1RetryObserver: Send + Sync {
    /// 每段 L1 生成成功后的增量镜像（桌面 = Retriever / 关键词镜像；服务层 = 服务层镜像）。
    async fn on_l1(&self, _l1: &MemoryL1) {}

    /// 单条任务补跑成功后的级联检查（桌面 = L2 触发检查；服务层 = 注册的 `l2_trigger` 钩子）。
    async fn on_cascade(&self, _persona_uid: Option<&str>) {}
}

/// 补扫并重试 L1 摘要失败遗留的 pending 任务。
///
/// 背景:
/// - 封存（桌面 `save_and_close_session` / 服务层 `seal::run`）中 L1 生成失败会登记 pending 的
///   `l1_summary` 后台任务；若缺少消费点，摘要在 LLM 恢复后仍长期缺失。
/// - 本函数即该消费点（共用实现）：桌面启动期与 L2/L3 定时线程、MCP 宿主空闲检查各自调用。
///
/// 行为（逐条幂等，单条失败不影响其他任务）:
/// - 仅处理补偿登记类型 `l1_summary_retry` 的 pending 任务，单轮最多重试 `max_per_run` 条
///   （其余留待下一轮）；在途生成任务（`l1_summary`）不消费。
/// - 逐条原子抢占（`pending` → `running`）：抢占失败说明已被其他消费方取走，本轮跳过。
/// - payload 缺失/非法 → 标记完成（无法重试，避免永久 pending）。
/// - 该 session 已有 L1（用户可能已手动重试）→ 标记完成，不重复调用 LLM。
/// - session 不存在或无消息 → 无法再产出摘要，标记完成（记 warn）。
/// - 补跑成功 → 标记完成并调用 `observer`（镜像 + 级联）；补跑失败 → 保持 pending（记 warn）。
///
/// 返回:
/// - [`L1RetryStats`]：本轮检视 / 尝试 / 完成计数。
pub async fn retry_pending_l1_jobs(
    storage: &dyn StorageBackend,
    llm: &dyn LlmProvider,
    progressive: &L1ProgressiveConfig,
    max_per_run: usize,
    observer: &dyn L1RetryObserver,
) -> L1RetryStats {
    let pending = match storage.list_pending_jobs().await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "L1 补扫：查询 pending 任务失败，本轮跳过");
            return L1RetryStats::default();
        }
    };

    let job_manager = JobManager::with_defaults(storage);
    let mut stats = L1RetryStats::default();

    for (job_id, job_type, payload) in pending {
        // 只消费补偿登记类型：在途生成任务（l1_summary）创建后到置 running 之间有短暂
        // pending 窗口，误取会对同一会话重复生成摘要（并发场景实测可复现）。
        if job_type != JobType::L1SummaryRetry.as_str() {
            continue;
        }
        if stats.attempted >= max_per_run {
            info!(
                limit = max_per_run,
                "L1 补扫：本轮已达重试上限，剩余任务留待下一轮"
            );
            break;
        }
        stats.scanned += 1;

        let Some((session_id, persona_uid)) = parse_l1_retry_payload(payload.as_deref()) else {
            warn!(
                job_id,
                "L1 补扫：payload 缺失或非法，标记任务完成（无法重试）"
            );
            mark_l1_job_completed(&job_manager, job_id).await;
            stats.completed += 1;
            continue;
        };

        // 已有 L1 → 视为已补跑（用户可能已手动 regenerate），标记完成
        match storage.list_memory_l1(session_id).await {
            Ok(l1_list) if !l1_list.is_empty() => {
                info!(
                    job_id,
                    %session_id,
                    "L1 补扫：该会话已有 L1 摘要，标记任务完成"
                );
                mark_l1_job_completed(&job_manager, job_id).await;
                stats.completed += 1;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                warn!(
                    job_id,
                    %session_id,
                    error = %e,
                    "L1 补扫：查询 L1 失败，保持 pending 待下一轮"
                );
                continue;
            }
        }

        // 会话不存在或无消息 → 不可能再产出 L1，标记完成避免永久 pending
        match storage.list_messages(session_id).await {
            Ok(messages) if messages.is_empty() => {
                warn!(job_id, %session_id, "L1 补扫：会话无消息，标记任务完成");
                mark_l1_job_completed(&job_manager, job_id).await;
                stats.completed += 1;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                warn!(
                    job_id,
                    %session_id,
                    error = %e,
                    "L1 补扫：读取会话消息失败，保持 pending 待下一轮"
                );
                continue;
            }
        }

        // 原子抢占（pending → running）：多消费方（桌面 / MCP 宿主）并发补扫时
        // 只有一方拿到执行权；抢占失败 / 存储异常均保持 pending，下一轮再试。
        match job_manager.claim_pending(job_id).await {
            Ok(true) => {}
            Ok(false) => {
                info!(job_id, %session_id, "L1 补扫：任务已被其他消费方抢占，本轮跳过");
                continue;
            }
            Err(e) => {
                warn!(job_id, error = %e, "L1 补扫：抢占任务失败，保持 pending 待下一轮");
                continue;
            }
        }

        stats.attempted += 1;
        info!(job_id, %session_id, ?persona_uid, "L1 补扫：开始重试摘要生成");
        let request = L1GenerateRequest::new(session_id, persona_uid.as_deref());
        match generate_l1_summaries(storage, llm, progressive, request).await {
            Ok(l1_list) if !l1_list.is_empty() => {
                info!(
                    job_id,
                    %session_id,
                    l1_count = l1_list.len(),
                    "L1 补扫：摘要补跑成功"
                );
                for l1 in &l1_list {
                    observer.on_l1(l1).await;
                }
                observer.on_cascade(persona_uid.as_deref()).await;
                mark_l1_job_completed(&job_manager, job_id).await;
                stats.completed += 1;
            }
            Ok(_) => {
                warn!(job_id, %session_id, "L1 补扫：未产出摘要，保持 pending 待下一轮");
            }
            Err(e) => {
                warn!(
                    job_id,
                    %session_id,
                    error = %e,
                    "L1 补扫：重试失败，保持 pending 待下一轮（LLM 可能仍不可用）"
                );
            }
        }
    }

    if stats.scanned > 0 {
        info!(
            scanned = stats.scanned,
            attempted = stats.attempted,
            completed = stats.completed,
            "L1 补扫完成"
        );
    }
    stats
}

/// 标记 L1 补扫任务为完成（状态写失败仅记 warn，不阻塞补扫主流程）。
async fn mark_l1_job_completed(job_manager: &JobManager<'_>, job_id: i64) {
    if let Err(e) = job_manager.mark_completed(job_id).await {
        warn!(
            job_id,
            error = %e,
            "L1 补扫：标记任务完成失败（已补跑，仅状态未更新）"
        );
    }
}

/// 解析 L1 补扫任务 payload（封存失败登记时写入的 JSON）。
///
/// 结构:
/// - `{"session_id": "<uuid>", "persona_uid": "<uid|空>", "reason": "..."}`
///
/// 返回:
/// - `Some((session_id, persona_uid))`：`session_id` 合法即成功（persona 可为 None）。
/// - `None`：payload 缺失、非 JSON 或 `session_id` 非法（该任务无法重试）。
fn parse_l1_retry_payload(payload: Option<&str>) -> Option<(Uuid, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(payload?).ok()?;
    let session_id = Uuid::parse_str(value.get("session_id")?.as_str()?).ok()?;
    let persona_uid = value
        .get("persona_uid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some((session_id, persona_uid))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// payload 解析：合法 JSON（含/不含 persona）→ Some；缺失/非法 → None。
    #[test]
    fn parse_l1_retry_payload_cases() {
        let sid = Uuid::new_v4();

        let full = serde_json::json!({
            "session_id": sid.to_string(),
            "persona_uid": "char-0001",
            "reason": "auto_retry_on_close"
        })
        .to_string();
        let (parsed_sid, persona) = parse_l1_retry_payload(Some(&full)).expect("合法 payload");
        assert_eq!(parsed_sid, sid);
        assert_eq!(persona.as_deref(), Some("char-0001"));

        // persona 为 null / 空串 → None（L1 归属走默认）
        let no_persona = serde_json::json!({ "session_id": sid.to_string() }).to_string();
        let (parsed_sid, persona) = parse_l1_retry_payload(Some(&no_persona)).expect("无 persona");
        assert_eq!(parsed_sid, sid);
        assert!(persona.is_none());

        // 缺失 payload / 非法 JSON / session_id 非法 → None（任务无法重试）
        assert!(parse_l1_retry_payload(None).is_none());
        assert!(parse_l1_retry_payload(Some("not json")).is_none());
        assert!(parse_l1_retry_payload(Some(r#"{"session_id": "not-a-uuid"}"#)).is_none());
    }

    /// 请求构造：默认前缀为空（由 summarizer 使用内置默认）。
    #[test]
    fn generate_request_defaults_use_summarizer_prefixes() {
        let sid = Uuid::new_v4();
        let request = L1GenerateRequest::new(sid, Some("char-0001"));
        assert_eq!(request.session_id, sid);
        assert_eq!(request.persona_uid, Some("char-0001"));
        assert!(request.user_prefix.is_none());
        assert!(request.assistant_prefix.is_none());
    }
}
