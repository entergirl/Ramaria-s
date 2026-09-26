//! crates/ramaria-service/src/seal.rs - 封存用例（会话收尾与记忆加工触发）
//!
//! 设计特点:
//! - 抢占幂等：先以条件更新（`ended_at IS NULL`）抢占关闭权，仅抢到者生成 L1；
//!   多进程 / 多线程同时封存同一会话时只有一次摘要生成（D-V21-007）
//! - 封存链路：L1 摘要（渐进式感知）→ 索引镜像增量 → utt 话语块 → examples 回复对
//!   → 宿主钩子（行为 / 风格 / L2 触发）
//! - 失败补偿：L1 生成失败登记 `l1_summary` pending 任务（与在线管线同一补偿语义，
//!   由补扫路径消费重试），会话本身仍视为已关闭（不阻塞用户继续新会话）
//! - 钩子注册式接入：行为规则 / 风格统计 / L2 触发的算法实现位于内核与宿主，
//!   本层只按位置调用（引擎以借用传入），未注册则跳过（不在服务层重写第二套算法）
//! - 钩子不持有 Engine：依赖在调用时经传入的引擎引用读取，
//!   避免 Engine ↔ 钩子引用环导致长驻进程内存不回收
//! - 隐私：日志只记计数与 ID，不记摘要与原文全文

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryL1;
use ramaria_memory::job::JobType;
use ramaria_memory::utt::builder::UttBuilder;
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::SealOutcome;

// =========================================================
// 封存钩子（宿主注册）
// =========================================================

/// 封存钩子：接收引擎引用与 persona_uid，内部自行处理失败（不阻塞封存主流程）。
///
/// 实现要求（注册方契约）:
/// - 钩子内部必须自行捕获并记录错误（与在线管线的注册式接入口径一致）——
///   本层不重复捕获，也不对钩子做 panic 隔离；
/// - **不得 panic**：panic 会穿越封存主流程上抛（L1 与 utt 已生成，会话已关闭，
///   但调用方会收到错误）；如需兜底请在钩子内部 `catch_unwind`；
/// - 不得长时间阻塞（封存是用户可感知的收尾路径）；
/// - **不得持有 Engine**：依赖在调用时经参数借用读取，钩子本身不保存引擎引用
///   或引擎内部依赖的装配期快照，避免 Engine ↔ 钩子引用环。
pub type SealHook = Arc<
    dyn for<'a> Fn(&'a Engine, &'a str) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + Sync,
>;

/// 封存钩子集合（未注册的步骤跳过）。
///
/// 字段约定:
/// - `behavior`: 行为规则增量更新（宿主实现，`[behavior].enabled` 时注册）；
/// - `style`: 风格统计增量更新（宿主实现，`[style].enabled` 时注册）；
/// - `l2_trigger`: L2 事件提取触发检查（链路形态由装配方决定：
///   单 persona 最小触发，或全 persona 提取 + 知识事实抽取 + L3 级联）。
#[derive(Clone, Default)]
pub struct SealHooks {
    pub behavior: Option<SealHook>,
    pub style: Option<SealHook>,
    pub l2_trigger: Option<SealHook>,
}

// =========================================================
// 用例入口
// =========================================================

/// 执行封存用例。
///
/// 流程:
/// 1. 抢占式关闭（失败 = 已被他人封存 / 会话不存在 → 直接返回 `sealed=false`）；
/// 2. 读取会话归属（DB 真相源）与消息（无消息 → 关闭即完成，不生成空摘要）；
/// 3. 生成 L1（渐进式感知；失败 → 登记 pending 任务并返回错误）；
/// 4. 索引镜像增量（检索器 + 关键词镜像）；
/// 5. utt 话语块增量构建；
/// 6. examples 回复对抽取入库；
/// 7. 宿主钩子（行为 / 风格 / L2 触发）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `session_id`: 目标会话。
///
/// 返回:
/// - `sealed`: 是否由本次调用抢到并完成封存（false = 未抢到，不重复生成 L1）；
/// - `l1_count`: 本次生成的 L1 条数。
pub(crate) async fn run(engine: &Engine, session_id: Uuid) -> RamariaResult<SealOutcome> {
    // ---- 0. 封存许可（服务层门禁） ----
    // 关闭时直接跳过（不抢占关闭、不生成摘要）：入口层配置（如 [mcp].allow_seal = false）
    // 的语义是「只写不封存」，会话保持活跃，留待允许封存的宿主或下次允许时处理。
    if !engine.seal_allowed() {
        tracing::info!(
            %session_id,
            "封存已禁用（服务层门禁）：跳过本次封存与摘要生成（会话保持活跃）"
        );
        return Ok(SealOutcome {
            session_id,
            sealed: false,
            l1_count: 0,
        });
    }

    let storage = engine.storage_ref().as_ref();

    // ---- 1. 抢占式关闭（幂等：仅一个调用方抢到） ----
    let claimed = storage.close_session_if_active(session_id).await?;
    if !claimed {
        tracing::debug!(%session_id, "会话未抢到关闭权（已关闭或不存在），跳过封存加工");
        return Ok(SealOutcome {
            session_id,
            sealed: false,
            l1_count: 0,
        });
    }

    // ---- 2. 归属与消息 ----
    let session = storage.get_session(session_id).await?;
    let persona_uid = session.as_ref().and_then(|s| s.persona_uid.clone());
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        tracing::info!(%session_id, "会话已关闭但无消息，跳过 L1 生成");
        return Ok(SealOutcome {
            session_id,
            sealed: true,
            l1_count: 0,
        });
    }
    tracing::info!(
        %session_id,
        persona_uid = persona_uid.as_deref().unwrap_or("none"),
        msg_count = messages.len(),
        "会话封存开始（已抢占关闭权）"
    );

    // ---- 3. L1 摘要（渐进式感知；失败登记 pending 重试任务） ----
    let l1_list = match generate_l1(engine, session_id, persona_uid.as_deref()).await {
        Ok(list) => list,
        Err(e) => {
            tracing::error!(
                %session_id,
                error = %e,
                "L1 摘要生成失败（会话已关闭，摘要缺失；已登记重试任务）"
            );
            register_l1_retry(engine, session_id, persona_uid.as_deref()).await;
            return Err(e);
        }
    };
    tracing::info!(%session_id, l1_count = l1_list.len(), "L1 摘要生成完成");

    // ---- 4. 索引镜像增量（检索器 + 关键词镜像） ----
    for l1 in &l1_list {
        crate::index::index_l1_into_mirrors(engine, l1).await;
    }

    // ---- 5. utt 话语块（失败降级，不阻塞封存） ----
    build_utt(engine, session_id, session.as_ref()).await;

    // ---- 6. examples 回复对（失败降级，不阻塞封存） ----
    extract_examples(engine, session_id).await;

    // ---- 7. 宿主钩子（行为 / 风格 / L2 触发；未注册则跳过） ----
    let hooks = engine.seal_hooks();
    run_hook(
        engine,
        &hooks.behavior,
        persona_uid.as_deref(),
        "行为规则增量更新",
    )
    .await;
    run_hook(
        engine,
        &hooks.style,
        persona_uid.as_deref(),
        "风格统计增量更新",
    )
    .await;
    run_hook(
        engine,
        &hooks.l2_trigger,
        persona_uid.as_deref(),
        "L2 触发检查",
    )
    .await;

    Ok(SealOutcome {
        session_id,
        sealed: true,
        l1_count: l1_list.len(),
    })
}

// =========================================================
// L1 摘要生成
// =========================================================

/// 生成会话 L1 摘要（渐进式感知，与在线管线封存口径一致）。
///
/// 实现要点:
/// - 编排与桌面共用 `ramaria_memory::l1::generate_l1_summaries`（同源，不重写第二套）：
///   `[l1.progressive]` 触发时按段生成多条 L1，否则单条整会话摘要；
///   `max_tokens` 从 `backend_config` 传播并以下限钳制；经 `JobManager` 包裹执行。
///
/// 返回:
/// - 成功时返回本会话的全部 L1（顺序与库内一致）。
async fn generate_l1(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
) -> RamariaResult<Vec<MemoryL1>> {
    let progressive = engine.config().l1.progressive.clone();
    ramaria_memory::l1::generate_l1_summaries(
        engine.storage_ref().as_ref(),
        engine.llm_ref().as_ref(),
        &progressive,
        ramaria_memory::l1::L1GenerateRequest::new(session_id, persona_uid),
    )
    .await
}

/// 登记 L1 重试任务（摘要失败时补偿，供补扫路径消费）。
///
/// 类型说明:
/// - 使用补偿登记专用类型 `l1_summary_retry`（[`JobType::L1SummaryRetry`]）：
///   与在途生成任务（`l1_summary`）区分，避免补扫消费方误取在途任务造成重复摘要。
async fn register_l1_retry(engine: &Engine, session_id: Uuid, persona_uid: Option<&str>) {
    let storage = engine.storage_ref().as_ref();
    let payload = serde_json::json!({
        "session_id": session_id.to_string(),
        "persona_uid": persona_uid,
        "reason": "auto_retry_on_seal"
    })
    .to_string();
    match storage
        .create_background_job(JobType::L1SummaryRetry.as_str(), Some(&payload))
        .await
    {
        Ok(job_id) => {
            tracing::info!(%session_id, job_id, "已登记 L1 重试任务（pending）");
        }
        Err(job_err) => {
            tracing::error!(%session_id, error = %job_err, "登记 L1 重试任务失败（需人工补摘要）");
        }
    }
}

// =========================================================
// utt / examples
// =========================================================

/// utt 话语块增量构建（失败降级，不阻塞封存）。
///
/// 降级:
/// - `[utt].enabled=false` → 跳过；
/// - 会话缺失 / 构建失败 → warn（下次封存自动补齐）；
/// - embedding 不可用 → 块照常入库（无向量，检索走子串降级）。
async fn build_utt(
    engine: &Engine,
    session_id: Uuid,
    session: Option<&ramaria_core::types::Session>,
) {
    let config = engine.config();
    if !config.utt.enabled {
        tracing::debug!(%session_id, "utt 配置关闭，跳过话语块构建");
        return;
    }
    let Some(session) = session else {
        tracing::warn!(%session_id, "会话记录缺失，跳过 utt 构建");
        return;
    };

    let builder = UttBuilder::from_config(&config.utt);
    // 嵌入 provider 取快照后在锁外使用（不可用 → 块照常入库，无向量）
    let embedding = engine.embedding_ref();
    let embedder = embedding.as_deref();
    match builder
        .build_session(engine.storage_ref().as_ref(), session, embedder)
        .await
    {
        Ok(stats) => {
            tracing::info!(
                %session_id,
                created = stats.chunks_created,
                skipped = stats.chunks_skipped,
                removed = stats.chunks_removed,
                embedding_ok = stats.embedding_ok,
                embedding_failed = stats.embedding_failed,
                "utt 话语块增量构建完成"
            );
        }
        Err(e) => {
            tracing::warn!(%session_id, error = %e, "utt 话语块构建失败（不阻塞封存，下次自动补齐）");
        }
    }
}

/// examples 回复对抽取入库（失败降级，不阻塞封存）。
async fn extract_examples(engine: &Engine, session_id: Uuid) {
    let config = engine.config();
    if let Err(e) = ramaria_memory::example::extract_and_save_for_session(
        engine.storage_ref().as_ref(),
        session_id,
        &config.examples,
    )
    .await
    {
        tracing::warn!(
            %session_id,
            error = %e,
            "examples 回复对抽取入库失败（不阻塞封存，下次自动补齐）"
        );
    }
}

// =========================================================
// 钩子调用
// =========================================================

/// 调用封存钩子（未注册 / 无归属时跳过）。
///
/// 说明:
/// - 钩子内部自行处理失败（注册方约定），本层不重复捕获；
/// - 引擎以借用传入（钩子契约：不持有引擎，见 [`SealHook`]）；
/// - 会话无 persona 归属时不调用（钩子以 persona 为输入，无归属无从更新）；
/// - `pub(crate)`: 供 L1 补扫在补跑成功后复用同一 L2 触发钩子。
pub(crate) async fn run_hook(
    engine: &Engine,
    hook: &Option<SealHook>,
    persona_uid: Option<&str>,
    label: &str,
) {
    let (Some(hook), Some(persona_uid)) = (hook.as_ref(), persona_uid) else {
        tracing::debug!(hook = label, "封存钩子未注册或会话无归属，跳过");
        return;
    };
    tracing::debug!(hook = label, persona_uid, "调用封存钩子");
    hook(engine, persona_uid).await;
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, engine_with_db, engine_with_l1_reply, seed_persona,
        seed_session_with_messages,
    };
    use crate::types::{RecallLayer, RecallRequest};
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 封存生成单条 L1，并完成抢占幂等（二次调用不重复生成）。
    #[tokio::test]
    async fn seal_generates_l1_once_and_is_idempotent() {
        let (engine, storage, dir) = engine_with_l1_reply("seal", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;
        // 先加载索引：验证 L1 生成后的增量镜像链路（检索即可命中）
        engine.ensure_index_loaded().await.expect("索引加载");

        let first = engine.seal(session_id).await.expect("封存应成功");
        assert!(first.sealed, "首次调用应抢到关闭权");
        assert_eq!(first.l1_count, 1, "短会话应生成单条 L1");

        let l1_list = storage
            .list_memory_l1(session_id)
            .await
            .expect("读取 L1 应成功");
        assert_eq!(l1_list.len(), 1, "库中应只有一条 L1");
        assert_eq!(
            l1_list[0].persona_uid.as_deref(),
            Some("char-0001"),
            "L1 归属应为会话 persona"
        );

        // 二次调用：已关闭 → 未抢到 → 不重复生成
        let second = engine.seal(session_id).await.expect("封存应幂等成功返回");
        assert!(!second.sealed, "二次调用不应抢到关闭权");
        assert_eq!(second.l1_count, 0);
        assert_eq!(
            storage
                .list_memory_l1(session_id)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "重复封存不得产生第二条 L1"
        );

        // 增量镜像生效：新 L1 可被召回
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
            "封存后 L1 应立即可检索: {:?}",
            recalled.items
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 并发抢占语义：两个调用方同时封存，只有一方生成 L1。
    #[tokio::test]
    async fn seal_concurrent_calls_have_single_winner() {
        let (engine, storage, dir) = engine_with_l1_reply("seal-race", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        // 并发触发两个封存（同一会话）
        let engine_a = &engine;
        let (first, second) = tokio::join!(async { engine_a.seal(session_id).await }, async {
            engine_a.seal(session_id).await
        });
        let winners = [first.expect("封存不报错"), second.expect("封存不报错")]
            .into_iter()
            .filter(|outcome| outcome.sealed)
            .count();
        assert_eq!(winners, 1, "并发封存应只有一个赢家（抢占幂等）");
        assert_eq!(
            storage
                .list_memory_l1(session_id)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "并发封存不得产生重复 L1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 封存门禁（D-V21-009）：许可关闭时封存用例直接跳过
    /// （不抢占关闭、不改会话状态、不调用 LLM 生成摘要）。
    #[tokio::test]
    async fn seal_is_noop_when_seal_disabled() {
        let (engine, storage, dir) = engine_with_l1_reply("seal-gated", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        engine.set_seal_allowed(false);
        let outcome = engine
            .seal(session_id)
            .await
            .expect("封存应正常返回（跳过）");
        assert!(!outcome.sealed, "许可关闭时不应执行封存");
        assert_eq!(outcome.l1_count, 0);

        let session = storage
            .get_session(session_id)
            .await
            .expect("查询应成功")
            .expect("会话应存在");
        assert!(session.ended_at.is_none(), "许可关闭时不应关闭会话");
        assert!(
            storage
                .list_memory_l1(session_id)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "许可关闭时不应生成 L1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空会话封存：会话关闭但无消息 → 不生成 L1（不调用 LLM）。
    #[tokio::test]
    async fn seal_empty_session_skips_l1() {
        let (engine, storage, dir) = engine_with_l1_reply("seal-empty", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话");

        let outcome = engine.seal(session.id).await.expect("封存应成功");
        assert!(outcome.sealed);
        assert_eq!(outcome.l1_count, 0);
        assert!(
            storage
                .list_memory_l1(session.id)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "空会话不应生成 L1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 失败：会话仍关闭、错误可见，并登记 pending 重试任务（不丢补偿）。
    #[tokio::test]
    async fn seal_l1_failure_registers_retry_job() {
        // 空回复 mock → L1 JSON 解析失败（模拟 LLM 不可用/输出非法）
        let (engine, storage, dir) = engine_with_db("seal-fail").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let err = engine.seal(session_id).await.expect_err("L1 失败应报错");
        assert!(!err.to_string().is_empty(), "错误信息应可读");

        // 会话已关闭（不阻塞用户继续新会话）
        let session = storage
            .get_session(session_id)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(session.ended_at.is_some(), "会话应已关闭");

        // 登记了 pending 重试任务
        let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(
            pending
                .iter()
                .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
            "L1 失败应登记 l1_summary_retry 补偿任务: {pending:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 封存钩子：注册后按位置调用（行为 / 风格 / L2 各一次），未注册则跳过。
    #[tokio::test]
    async fn seal_invokes_registered_hooks() {
        let (engine, storage, dir) = engine_with_l1_reply("seal-hooks", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let behavior_calls = Arc::new(AtomicUsize::new(0));
        let style_calls = Arc::new(AtomicUsize::new(0));
        let l2_calls = Arc::new(AtomicUsize::new(0));
        let make_hook = |counter: Arc<AtomicUsize>| -> SealHook {
            Arc::new(move |_engine: &Engine, _persona: &str| {
                let counter = Arc::clone(&counter);
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }) as Pin<Box<dyn Future<Output = ()> + Send + '_>>
            })
        };
        engine.set_seal_hooks(SealHooks {
            behavior: Some(make_hook(Arc::clone(&behavior_calls))),
            style: Some(make_hook(Arc::clone(&style_calls))),
            l2_trigger: Some(make_hook(Arc::clone(&l2_calls))),
        });

        engine.seal(session_id).await.expect("封存应成功");
        assert_eq!(behavior_calls.load(Ordering::SeqCst), 1);
        assert_eq!(style_calls.load(Ordering::SeqCst), 1);
        assert_eq!(l2_calls.load(Ordering::SeqCst), 1);

        // 未抢到的封存不得再触发钩子
        engine.seal(session_id).await.expect("重复封存应成功返回");
        assert_eq!(
            behavior_calls.load(Ordering::SeqCst),
            1,
            "重复封存不重复调用钩子"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
