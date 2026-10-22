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

mod generate;
mod regen;
mod retry;

#[cfg(feature = "importer")]
pub(crate) use generate::generate_l1_summary;
pub(crate) use regen::{regenerate_l1, regenerate_l1_no_cascade, regenerate_l1_progressive};
pub(crate) use retry::{retry_pending_l1_jobs, retry_pending_l1_jobs_with_stats};

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
    use ramaria_memory::job::JobType;
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
            &dir.join("ramaria.db"),
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
