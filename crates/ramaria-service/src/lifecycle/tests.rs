//! crates/ramaria-service/src/lifecycle/tests.rs - Ramaria 会话生命周期容器单元测试
//!
//! 设计特点:
//! - 活跃指针与最后活跃缓存：set / get / forget / clear_active_if 全链路往返
//! - 手动关闭等价性：Lifecycle 关闭路径与 Engine::seal 产出摘要逐字段一致
//! - 空闲线程：超时自动封存、阈值热更新、活跃指针一致性清理、状态机与事实对齐
//! - 装配口径：desktop / mcp / none 三种选项的循环拉起差异与关停幂等
//! - 降级：LLM 恒失败时仍按"会话已关闭"收尾并登记补偿任务

use super::*;
use crate::test_support::{
    DeterministicEmbedding, L1_JSON_REPLY, MockLlm, engine_with_db, engine_with_l1_reply,
    engine_with_llm_config_and_embedding, seed_persona, seed_session_with_messages,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig, MemoryL1, now_ms};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// 测试用装配选项：空闲与 L2/L3 均以 1 秒轮次运行（关停等待收敛在 1 秒量级），
/// 首轮延迟 1 秒，其余按桌面口径。
fn test_options() -> LifecycleOptions {
    LifecycleOptions::desktop()
        .with_idle_interval(1)
        .with_l2_l3_interval(1)
        .with_l2_l3_first_delay(1)
}

/// 活跃指针与最后活跃缓存：set / get / forget / clear_active_if。
#[tokio::test]
async fn active_pointer_and_last_active_cache() {
    let (engine, _storage, dir) = engine_with_db("life-pointer-basic").await;
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());

    let sid = Uuid::new_v4();
    assert!(lifecycle.active_session_id().is_none(), "初始无活跃指针");
    lifecycle.set_active_session_id(Some(sid));
    assert_eq!(lifecycle.active_session_id(), Some(sid));
    lifecycle.set_active_session_id(None);
    assert!(lifecycle.active_session_id().is_none());

    // touch / last_active / forget
    lifecycle.touch_session(sid);
    assert!(
        lifecycle.last_active(sid).is_some_and(|t| t > 0),
        "touch 后应能读到活跃时间"
    );
    lifecycle.forget_session(sid);
    assert!(lifecycle.last_active(sid).is_none(), "forget 后缓存应清空");

    // clear_active_if：仅指针指向该会话时清空
    let other = Uuid::new_v4();
    lifecycle.set_active_session_id(Some(sid));
    lifecycle.touch_session(sid);
    lifecycle.clear_active_if(other);
    assert_eq!(
        lifecycle.active_session_id(),
        Some(sid),
        "指针未指向该会话时不应清空指针"
    );
    lifecycle.clear_active_if(sid);
    assert!(
        lifecycle.active_session_id().is_none(),
        "指针指向该会话时应清空"
    );
    assert!(lifecycle.last_active(sid).is_none(), "缓存应一并清理");

    let _ = std::fs::remove_dir_all(&dir);
}

/// close_active_session：无活跃 → Ok(None)；有活跃 → 关闭 + 1 条 L1 + 指针与缓存清空；
/// 再次调用 → Ok(None)。
#[tokio::test]
async fn close_active_session_seals_and_clears_pointer() {
    let (engine, storage, dir) = engine_with_l1_reply("life-close", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());

    // 无活跃会话 → Ok(None)
    assert!(
        lifecycle
            .close_active_session()
            .await
            .expect("无活跃会话应正常返回")
            .is_none()
    );

    lifecycle.set_active_session_id(Some(session));
    lifecycle.touch_session(session);
    let outcome = lifecycle
        .close_active_session()
        .await
        .expect("关闭应成功")
        .expect("应有封存结果");
    assert_eq!(outcome.session_id, session);
    assert!(outcome.sealed, "首次封存应抢到关闭权");
    assert_eq!(outcome.l1_count, 1, "短会话应生成单条 L1");

    let row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_some(), "会话应被关闭");
    assert_eq!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "应恰好一条 L1"
    );
    assert!(lifecycle.active_session_id().is_none(), "指针应被清空");
    assert!(lifecycle.last_active(session).is_none(), "缓存应被清空");

    // 再次调用：无活跃会话 → Ok(None)
    assert!(
        lifecycle
            .close_active_session()
            .await
            .expect("重复关闭应正常返回")
            .is_none()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// close_active_session 且 LLM 恒失败：会话仍被关闭、指针清空、返回 Ok(None)、
/// 库中登记 l1_summary_retry 补偿任务。
#[tokio::test]
async fn close_active_session_keeps_ok_when_l1_fails() {
    let (engine, storage, dir) =
        crate::test_support::engine_with_failing_llm("life-close-fail").await;
    seed_persona(&storage, "char-0001").await;
    let session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
    lifecycle.set_active_session_id(Some(session));
    lifecycle.touch_session(session);

    // 摘要生成失败但会话已收尾 → 复查后按"已关闭"处理，不向上抛
    let result = lifecycle
        .close_active_session()
        .await
        .expect("会话已关闭时应返回 Ok(None)");
    assert!(result.is_none(), "摘要失败但会话已关闭 → 返回 None");

    let row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_some(), "摘要失败不改变会话已关闭的事实");
    assert!(lifecycle.active_session_id().is_none(), "指针应被清空");
    assert!(lifecycle.last_active(session).is_none(), "缓存应被清空");
    let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
    assert!(
        pending
            .iter()
            .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
        "摘要失败应登记补偿任务: {pending:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空闲线程：超时会话被自动封存（关闭 + L1）→ shutdown 后循环不再运行，可重复调用。
#[tokio::test]
async fn idle_thread_seals_timed_out_session_then_shutdown_stops() {
    let (engine, storage, dir) = engine_with_l1_reply("life-idle-thread", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
    let engine = Arc::new(engine);

    let lifecycle = engine.start_lifecycle(test_options());
    assert!(
        lifecycle.idle_loop_running(),
        "拉起后空闲循环应处于运行状态"
    );

    // 轮询等待自动封存（上限 6 秒）：同时要求 L1 落库，避免命中"已关闭未写摘要"的窗口
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        let l1_count = storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len();
        if row.ended_at.is_some() && l1_count == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "空闲线程应在限时内封存超时会话（closed={}, l1={l1_count}）",
            row.ended_at.is_some()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    lifecycle.shutdown().await;
    assert!(!lifecycle.idle_loop_running(), "关停后空闲循环不应再运行");
    // 重复关停为空操作（不阻塞、不报错）
    lifecycle.shutdown().await;
    assert!(!lifecycle.idle_loop_running(), "重复关停后仍不应运行");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 阈值热更新：默认阈值下 5 分钟前活跃的会话不被封存；热更新到 1 分钟后在轮询上限内被封存。
#[tokio::test]
async fn idle_threshold_hot_update_takes_effect() {
    let (engine, storage, dir) = engine_with_l1_reply("life-hot", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    // 5 分钟前活跃：默认阈值 10 分钟下未超时
    let session = seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 5 * 60_000).await;
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(test_options());

    // 默认阈值：手动跑一轮（与后台线程同一实现）→ 不封存
    assert_eq!(
        lifecycle.tick_idle().await.expect("空闲检查应成功"),
        0,
        "默认 10 分钟阈值下 5 分钟活跃不应封存"
    );
    let row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_none(), "默认阈值下会话应保持活跃");

    // 热更新到 1 分钟 → 下一轮即生效（后台线程在轮询上限内完成封存）
    lifecycle.set_idle_minutes(1);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        if row.ended_at.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "热更新后空闲线程应在限时内封存超时会话"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    lifecycle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 活跃指针一致性：指针指向的会话被空闲线程封存 → 下一轮后指针为 None。
#[tokio::test]
async fn active_pointer_cleared_after_idle_seal() {
    let (engine, storage, dir) = engine_with_l1_reply("life-pointer-seal", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(test_options());
    lifecycle.set_active_session_id(Some(session));
    lifecycle.touch_session(session);

    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if lifecycle.active_session_id().is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "空闲封存后活跃指针应被一致性清理"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        lifecycle.last_active(session).is_none(),
        "指针清理时缓存应一并清理"
    );
    let row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_some(), "超时会话应已被空闲线程封存");

    lifecycle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 状态机与事实对齐：空闲轮次后按配置完整度推进到 Ready / Indexing。
#[tokio::test]
async fn idle_tick_aligns_state_with_facts() {
    // 引擎 1：配置完整 + 索引已建 + 嵌入可用 → 空闲轮次后推进到 Ready
    let (engine, storage, dir) = engine_with_llm_config_and_embedding(
        "life-state-ready",
        MockLlm::with_reply(L1_JSON_REPLY),
        RamariaConfig::default(),
        Some(Arc::new(DeterministicEmbedding::new())),
    )
    .await;
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("保存后端配置应成功");
    storage
        .set_index_version(1)
        .await
        .expect("写入索引版本应成功");
    let engine = Arc::new(engine);
    assert_eq!(
        engine.current_state(),
        AppState::NeedsSetup,
        "装配初值应为 NeedsSetup"
    );

    let lifecycle = engine.start_lifecycle(test_options());
    let deadline = Instant::now() + Duration::from_secs(6);
    while engine.current_state() != AppState::Ready {
        assert!(
            Instant::now() < deadline,
            "空闲轮次应在限时内把状态推进到 Ready（当前 {:?}）",
            engine.current_state()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    lifecycle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);

    // 引擎 2：同配置但索引未建（index_version=0）→ 判定为 Indexing
    let (engine, storage, dir) = engine_with_llm_config_and_embedding(
        "life-state-indexing",
        MockLlm::with_reply(L1_JSON_REPLY),
        RamariaConfig::default(),
        Some(Arc::new(DeterministicEmbedding::new())),
    )
    .await;
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("保存后端配置应成功");
    storage
        .set_index_version(0)
        .await
        .expect("写入索引版本应成功");
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
    assert_eq!(lifecycle.tick_idle().await.expect("空闲检查应成功"), 0);
    assert_eq!(
        engine.current_state(),
        AppState::Indexing,
        "索引待构建应判定为 Indexing"
    );
    lifecycle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// MCP 装配：仅空闲循环；不开 L2/L3 时无主 L1 在空闲轮次后仍保持无主。
/// none() 选项不拉起任何循环。
#[tokio::test]
async fn mcp_options_start_idle_only_and_skip_l2() {
    let mut config = RamariaConfig::default();
    // 只要发生 L2 检查，无主 L1 就会被归属回填（阈值 1）
    config.thresholds.l2_trigger_count = 1;
    config.thresholds.cluster_delay_ms = 0;
    let (engine, storage, dir) = crate::test_support::engine_with_llm_and_config(
        "life-mcp",
        MockLlm::with_reply(L1_JSON_REPLY),
        config,
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    // 已关闭会话 + 一条无主 L1（无主 L1 属此类由导入产生）
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");
    let l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::mcp().with_idle_interval(1));
    assert!(lifecycle.idle_loop_running(), "mcp 选项应拉起空闲循环");
    assert!(!lifecycle.l2_l3_running(), "mcp 选项不应拉起 L2/L3 调度");

    // 跑一轮空闲检查（与后台线程同一实现）：无主 L1 保持无主
    assert_eq!(lifecycle.tick_idle().await.expect("空闲检查应成功"), 0);
    let unbound = storage
        .list_unabsorbed_l1_unbound()
        .await
        .expect("查询无主 L1 应成功");
    assert_eq!(unbound.len(), 1, "不开 L2/L3 时无主 L1 不应被归属");
    assert!(unbound[0].persona_uid.is_none(), "无主 L1 归属不应被回填");

    lifecycle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);

    // none() 选项：不拉起任何后台循环
    let (engine, _storage, dir) = engine_with_db("life-none").await;
    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
    assert!(!lifecycle.idle_loop_running(), "none 选项不应拉起空闲循环");
    assert!(!lifecycle.l2_l3_running(), "none 选项不应拉起 L2/L3 调度");
    lifecycle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// shutdown 落库：关闭活跃会话（生成 L1）、置停止位、两个循环不再运行。
#[tokio::test]
async fn shutdown_closes_active_session_and_stops_loops() {
    let (engine, storage, dir) = engine_with_l1_reply("life-shutdown", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    // 刚活跃的会话：空闲轮次不会封存它，由 shutdown 的关闭路径收尾
    let session = seed_session_with_messages(&storage, "char-0001", 4, now_ms()).await;
    let engine = Arc::new(engine);

    let lifecycle = engine.start_lifecycle(test_options());
    lifecycle.set_active_session_id(Some(session));
    lifecycle.touch_session(session);
    assert!(lifecycle.idle_loop_running(), "拉起后空闲循环应运行");
    assert!(lifecycle.l2_l3_running(), "拉起后 L2/L3 循环应运行");

    lifecycle.shutdown().await;

    assert!(
        lifecycle.shutdown_flag().load(Ordering::Acquire),
        "停止位应已置位"
    );
    assert!(!lifecycle.idle_loop_running(), "关停后空闲循环不应再运行");
    assert!(!lifecycle.l2_l3_running(), "关停后 L2/L3 循环不应再运行");
    assert!(lifecycle.active_session_id().is_none(), "指针应被清空");

    let row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_some(), "shutdown 应关闭活跃会话");
    assert_eq!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "shutdown 应生成 L1 摘要"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 封存结果对照：手动关闭（Lifecycle）与引擎封存（Engine::seal）产出等价摘要。
#[tokio::test]
async fn lifecycle_close_matches_engine_seal() {
    let (engine, storage, dir) = engine_with_l1_reply("life-compare", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_a = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;
    let session_b = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;

    let engine = Arc::new(engine);
    let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
    lifecycle.set_active_session_id(Some(session_a));

    let outcome_a = lifecycle
        .close_active_session()
        .await
        .expect("手动关闭应成功")
        .expect("应有封存结果");
    assert!(outcome_a.sealed, "会话 A 应由本次调用封存");
    let outcome_b = engine.seal(session_b).await.expect("引擎封存应成功");
    assert!(outcome_b.sealed, "会话 B 应由本次调用封存");

    // 两个会话都关闭、各恰好 1 条 L1
    for sid in [session_a, session_b] {
        let row = storage
            .get_session(sid)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_some(), "会话 {sid} 应被关闭");
        assert_eq!(
            storage
                .list_memory_l1(sid)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "会话 {sid} 应恰好一条 L1"
        );
    }

    // 关键字段逐项一致（同一 mock LLM 回复 → 摘要素材一致）
    let l1_a = storage
        .list_memory_l1(session_a)
        .await
        .expect("读取 L1 应成功")
        .pop()
        .expect("会话 A 应有 L1");
    let l1_b = storage
        .list_memory_l1(session_b)
        .await
        .expect("读取 L1 应成功")
        .pop()
        .expect("会话 B 应有 L1");
    assert_eq!(l1_a.summary, l1_b.summary, "summary 应一致");
    assert_eq!(l1_a.keywords, l1_b.keywords, "keywords 应一致");
    assert_eq!(l1_a.persona_uid, l1_b.persona_uid, "persona_uid 应一致");
    assert_eq!(l1_a.valence, l1_b.valence, "valence 应一致");
    assert_eq!(l1_a.salience, l1_b.salience, "salience 应一致");
    assert_eq!(l1_a.absorbed, l1_b.absorbed, "absorbed 应一致");

    // serde 归一化对照：两条摘要的序列化字段集合与内容一致
    let value_a = serde_json::to_value(&l1_a).expect("序列化应成功");
    let value_b = serde_json::to_value(&l1_b).expect("序列化应成功");
    assert_eq!(value_a["summary"], value_b["summary"]);
    assert_eq!(value_a["keywords"], value_b["keywords"]);
    assert_eq!(value_a["persona_uid"], value_b["persona_uid"]);

    let _ = std::fs::remove_dir_all(&dir);
}
