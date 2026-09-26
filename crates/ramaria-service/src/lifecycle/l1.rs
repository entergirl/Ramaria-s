//! crates/ramaria-service/src/lifecycle/l1.rs - L1 摘要生成、重生成与补扫
//!
//! 设计特点:
//! - 手动重生成三种口径：单段（`regenerate_l1`）/ 幂等无级联（`regenerate_l1_no_cascade`）/
//!   渐进式感知（`regenerate_l1_progressive`），供封存失败补救与批量导入场景
//! - 单段口径：`max_tokens` 从 `backend_config` 传播并以下限钳制（防结构化 JSON 截断），
//!   经 `JobManager` 包裹执行（指数退避重试），成功后从库读回
//! - 索引镜像：每段 L1 生成后增量镜像（检索器 + 关键词镜像），无需等待整库重建
//! - 级联语义：单段与渐进式重生成末尾触发 L2 检查；无级联口径由调用方统一触发
//! - 补扫：消费封存失败登记的类型 `l1_summary_retry` 任务，LLM / 存储恢复后自动补跑
//! - 隐私：日志只记任务 ID / 会话 ID 与计数，不记摘要内容

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::MemoryL1;
use ramaria_memory::job::{JobManager, JobResult, JobType};
use ramaria_memory::l1::{
    L1RetryObserver, L1RetryStats, L1Summarizer, L1SummarizerConfig, MAX_L1_RETRY_JOBS_PER_RUN,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// L1 摘要手动重生成
// =========================================================

/// 为指定会话重新生成单段 L1 摘要（手动重试，末尾触发 L2 检查）。
///
/// 职责:
/// - 供封存中 L1 生成失败后的手动补救；会话可已关闭，也可仍在活跃中。
/// - 与封存单段口径共用底层编排（`backend_config` 预算传播 + `JobManager` 重试 + 读回）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `session_id`: 目标会话。
/// - `persona_uid`: L1 归属人格（`None` = 不绑定）。
/// - `user_prefix`: 覆盖默认"用户："前缀；`None` 用默认。
/// - `assistant_prefix`: 覆盖默认"助手："前缀；`None` 用默认。
///
/// 返回:
/// - `Ok(Some(l1))`: 生成成功（已写库并增量镜像）；
/// - `Ok(None)`: 会话无消息（跳过）。
pub(crate) async fn regenerate_l1(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
) -> RamariaResult<Option<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        warn!(%session_id, "regenerate_l1: 会话无消息，跳过");
        return Ok(None);
    }

    info!(
        %session_id,
        ?persona_uid,
        msg_count = messages.len(),
        "手动重试 L1 摘要"
    );

    match generate_l1_summary(
        engine,
        session_id,
        persona_uid,
        user_prefix,
        assistant_prefix,
    )
    .await
    {
        Ok(l1) => {
            info!(%session_id, l1_id = %l1.id, "L1 重试成功");
            crate::index::index_l1_into_mirrors(engine, &l1).await;
            crate::lifecycle::l2_l3::check_l2_trigger(engine, None).await;
            Ok(Some(l1))
        }
        Err(e) => {
            error!(%session_id, error = %e, "L1 重试失败");
            Err(e)
        }
    }
}

/// 生成单段 L1 摘要但不触发 L2 级联（幂等；供批量导入场景使用）。
///
/// 幂等性:
/// - 目标 persona 已有 L1 → 跳过生成（避免重复 LLM 调用），仅补索引镜像并返回 `Ok(None)`；
/// - 仅有旧的无归属摘要（`persona_uid` 为空）→ 先清理再生成；
/// - 无任何摘要 → 直接生成。
///
/// 说明:
/// - 与 [`regenerate_l1`] 的差异是跳过末尾 L2 检查；
///   调用方应在全部 L1 生成完成后自行触发级联。
pub(crate) async fn regenerate_l1_no_cascade(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
) -> RamariaResult<Option<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        warn!(%session_id, "regenerate_l1_no_cascade: 会话无消息，跳过");
        return Ok(None);
    }

    // 已有目标 persona 的 L1 → 幂等跳过（避免重复 LLM 调用）
    if let Some(target_uid) = persona_uid {
        let existing = storage.list_memory_l1(session_id).await?;
        if let Some(existing_l1) = existing
            .iter()
            .find(|l1| l1.persona_uid.as_deref() == Some(target_uid))
        {
            info!(
                %session_id,
                persona_uid = %target_uid,
                "该会话已有目标 persona 的 L1 摘要，跳过重新生成"
            );
            // 索引镜像仍要确保（此前重建可能未覆盖该条）
            crate::index::index_l1_into_mirrors(engine, existing_l1).await;
            return Ok(None);
        }
    }

    // 清理旧的无归属摘要（避免同一会话新旧两份并存），再做生成
    let deleted = storage.delete_memory_l1_by_session(session_id).await?;
    if deleted > 0 {
        info!(%session_id, deleted, "已清理旧的无归属 L1 摘要");
    }

    info!(
        %session_id,
        ?persona_uid,
        msg_count = messages.len(),
        "批量 L1 摘要（无级联）"
    );

    match generate_l1_summary(
        engine,
        session_id,
        persona_uid,
        user_prefix,
        assistant_prefix,
    )
    .await
    {
        Ok(l1) => {
            info!(%session_id, l1_id = %l1.id, "L1 生成成功（无级联）");
            crate::index::index_l1_into_mirrors(engine, &l1).await;
            Ok(Some(l1))
        }
        Err(e) => {
            error!(%session_id, error = %e, "L1 生成失败");
            Err(e)
        }
    }
}

/// 为指定会话重新生成 L1 摘要（渐进式感知口径）。
///
/// 职责:
/// - 与封存路径口径一致：`[l1.progressive]` 开启且会话触发阈值（消息数 / 时间跨度）时
///   按段生成多条 L1；未触发时回退单段摘要。
/// - 供长会话的手动重摘要使用；末尾触发 L2 检查（与 [`regenerate_l1`] 一致的级联语义）。
///
/// 参数:
/// - 与 [`regenerate_l1`] 一致（`persona_uid` / 前缀覆盖）。
///
/// 返回:
/// - `Ok(l1_list)`: 本次生成的全部段 L1（未触发渐进时为 1 条）；
/// - `Ok(vec![])`: 会话无消息。
pub(crate) async fn regenerate_l1_progressive(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
) -> RamariaResult<Vec<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        warn!(%session_id, "regenerate_l1_progressive: 会话无消息，跳过");
        return Ok(Vec::new());
    }

    info!(
        %session_id,
        ?persona_uid,
        msg_count = messages.len(),
        "手动重试 L1 摘要（渐进式感知）"
    );

    let progressive = engine.config().l1.progressive.clone();
    let llm = engine.llm_ref();
    match ramaria_memory::l1::generate_l1_summaries(
        storage,
        llm.as_ref(),
        &progressive,
        ramaria_memory::l1::L1GenerateRequest {
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        },
    )
    .await
    {
        Ok(l1_list) => {
            info!(
                %session_id,
                l1_count = l1_list.len(),
                "L1 重试成功（渐进式感知）"
            );
            // 每段 L1 都做增量镜像（与既有重试路径一致）
            for l1 in &l1_list {
                crate::index::index_l1_into_mirrors(engine, l1).await;
            }
            crate::lifecycle::l2_l3::check_l2_trigger(engine, None).await;
            Ok(l1_list)
        }
        Err(e) => {
            error!(%session_id, error = %e, "L1 重试失败（渐进式感知）");
            Err(e)
        }
    }
}

// =========================================================
// 单段摘要生成（内部辅助）
// =========================================================

/// 为指定会话生成单段 L1 摘要。
///
/// 实现要点:
/// - `persona_uid` / 前缀覆盖写入 `L1SummarizerConfig`；
/// - `max_tokens` 从 `backend_config` 传播并以下限钳制（防 chat 侧小预算截断结构化 JSON）；
/// - 经 `JobManager::execute_with_retry` 包裹执行（指数退避重试 + 任务可观测性）；
/// - 成功后从存储读回最后一条 L1（`JobManager` 不返回业务结果）。
///
/// 返回:
/// - 成功时返回刚写入的 L1；生成后仍读不到记录视为内部错误（Validation）。
async fn generate_l1_summary(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
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
    let job_manager = JobManager::with_defaults(storage);
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
        crate::seal::run_hook(&hooks.l2_trigger, persona_uid, "L2 触发检查（L1 补扫）").await;
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, MockLlm, engine_on_existing_db, engine_with_db, engine_with_failing_llm,
        engine_with_l1_reply, engine_with_llm_and_config, engine_with_shared_llm, seed_persona,
        seed_session_with_messages,
    };
    use crate::types::{RecallLayer, RecallRequest};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use std::sync::Arc;

    /// 无 pending 补偿任务：补扫返回 0（边界用例）。
    #[tokio::test]
    async fn no_pending_jobs_returns_zero() {
        let (engine, _storage, dir) = engine_with_db("l1-retry-none").await;
        assert_eq!(
            retry_pending_l1_jobs(&engine).await,
            0,
            "无补偿任务时补扫应返回 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 封存失败登记补偿任务 → LLM 恢复后补扫成功（L1 落库、任务清空）。
    #[tokio::test]
    async fn retry_pending_after_llm_recovers() {
        let (engine, storage, dir) = engine_with_failing_llm("l1-retry-recover").await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        // 封存失败：会话被关闭、L1 未生成、登记 l1_summary_retry 补偿任务
        assert!(engine.seal(session).await.is_err(), "L1 失败时封存应报错");
        let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(
            pending
                .iter()
                .any(|(_, job_type, _)| job_type == JobType::L1SummaryRetry.as_str()),
            "L1 失败应登记补偿任务: {pending:?}"
        );

        // LLM 恢复：同库第二台引擎直接补扫 → 完成 1 条、L1 落库、pending 清空
        let recovered = engine_on_existing_db(
            &dir.join("assistant.db"),
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
        )
        .await;
        assert_eq!(
            retry_pending_l1_jobs(&recovered).await,
            1,
            "补扫应成功完成 1 条任务"
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "LLM 恢复后补扫应产出 L1"
        );
        let remaining = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(
            !remaining
                .iter()
                .any(|(_, job_type, _)| job_type == JobType::L1SummaryRetry.as_str()),
            "补跑成功后任务不应停留 pending: {remaining:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Engine::retry_pending_l1_jobs 门面可达：无 pending 时返回 0。
    #[tokio::test]
    async fn retry_pending_l1_jobs_facade_returns_zero_without_pending() {
        let (engine, _storage, dir) = engine_with_db("l1-retry-facade").await;
        assert_eq!(
            engine.retry_pending_l1_jobs().await,
            0,
            "无补偿任务时门面应返回 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无消息会话：单段重生成返回 Ok(None)，渐进式返回 Ok(vec![])。
    #[tokio::test]
    async fn regenerate_returns_none_without_messages() {
        let (engine, storage, dir) = engine_with_db("l1-regen-empty").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");

        assert!(
            engine
                .regenerate_l1(session.id, Some("char-0001"), None, None)
                .await
                .expect("无消息应正常返回")
                .is_none(),
            "单段重生成：无消息应返回 None"
        );
        assert!(
            engine
                .regenerate_l1_no_cascade(session.id, Some("char-0001"), None, None)
                .await
                .expect("无消息应正常返回")
                .is_none(),
            "无级联重生成：无消息应返回 None"
        );
        assert!(
            engine
                .regenerate_l1_progressive(session.id, Some("char-0001"), None, None)
                .await
                .expect("无消息应正常返回")
                .is_empty(),
            "渐进式重生成：无消息应返回空列表"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单段重生成成功：写库 + 索引镜像生效（立即可召回）。
    #[tokio::test]
    async fn regenerate_l1_writes_and_becomes_recallable() {
        let (engine, storage, dir) = engine_with_l1_reply("l1-regen-write", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;
        // 先加载索引：验证重生成后的增量镜像链路（检索即可命中）
        engine.ensure_index_loaded().await.expect("索引加载应成功");

        let l1 = engine
            .regenerate_l1(session, Some("char-0001"), None, None)
            .await
            .expect("重生成应成功")
            .expect("有消息会话应产出 L1");
        assert_eq!(
            l1.persona_uid.as_deref(),
            Some("char-0001"),
            "L1 归属应为人格参数"
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "重生成应写库一条 L1"
        );

        let recalled = engine
            .recall(RecallRequest {
                query: Some("工作压力".to_string()),
                persona: Some("char-0001".to_string()),
                include: Some(vec![RecallLayer::L1]),
                ..RecallRequest::default()
            })
            .await
            .expect("召回应成功");
        assert!(
            recalled.items.iter().any(|i| i.text.contains("工作压力")),
            "重生成的 L1 应立即可检索: {:?}",
            recalled.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 幂等：已有目标 persona 的 L1 → 不重复生成，LLM 调用不增加。
    #[tokio::test]
    async fn regenerate_l1_no_cascade_is_idempotent() {
        let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY));
        let (engine, storage, dir) = engine_with_shared_llm(
            "l1-regen-idempotent",
            Arc::clone(&llm),
            RamariaConfig::default(),
            None,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let first = engine
            .regenerate_l1_no_cascade(session, Some("char-0001"), None, None)
            .await
            .expect("首次生成应成功");
        assert!(first.is_some(), "首次应产出 L1");
        let calls_after_first = llm.chat_calls();
        assert!(calls_after_first > 0, "首次生成应调用 LLM");

        let second = engine
            .regenerate_l1_no_cascade(session, Some("char-0001"), None, None)
            .await
            .expect("幂等跳过应正常返回");
        assert!(second.is_none(), "已有目标 persona 的 L1 → 跳过生成");
        assert_eq!(
            llm.chat_calls(),
            calls_after_first,
            "幂等跳过不应新增 LLM 调用"
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "幂等跳过不应重复写入"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 渐进式重生成：开启渐进式且触发阈值时产出多段。
    #[tokio::test]
    async fn regenerate_l1_progressive_produces_segments() {
        let mut config = RamariaConfig::default();
        config.l1.progressive.enabled = true;
        // 6 条消息 > 3 → 触发；每段最多 2 条 → 切分为多段
        config.l1.progressive.msg_threshold = 3;
        config.l1.progressive.tail_msg_count = 2;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "l1-regen-progressive",
            MockLlm::with_reply(L1_JSON_REPLY),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 6, 1_000).await;

        let list = engine
            .regenerate_l1_progressive(session, Some("char-0001"), None, None)
            .await
            .expect("渐进式重生成应成功");
        assert!(
            list.len() >= 2,
            "触发阈值应产出多段 L1（实际 {} 段）",
            list.len()
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            list.len(),
            "库中条数应与返回列表一致"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
