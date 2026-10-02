//! crates/ramaria-service/src/idle/tests.rs - Ramaria 空闲检查模块测试
//!
//! 设计特点:
//! - 由 idle.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖空闲扫描 / 封存门禁 /
//!   L1 摘要补扫 / 宿主循环与并发去重四条路径
//! - 真实 SQLite（临时文件库 + 全量 migration）：多引擎同库并发场景直接构造
//! - L1 生成以确定性 mock LLM（固定 JSON 回复）驱动，不依赖网络与真实模型
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::*;
use crate::test_support::{
    L1_JSON_REPLY, MockLlm, engine_on_existing_db, engine_with_failing_llm, engine_with_l1_reply,
    seed_persona, seed_session_with_messages,
};
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use std::time::Instant;

/// 3 个会话 2 个超时：只封存超时的 2 个，未超时的保持活跃。
#[tokio::test]
async fn tick_seals_only_expired_sessions() {
    let (engine, storage, dir) = engine_with_l1_reply("idle", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;

    // 阈值 10 分钟：20 分钟前 → 超时；刚刚 → 未超时
    let stale_base = now_ms() - 20 * 60_000;
    let stale_a = seed_session_with_messages(&storage, "char-0001", 2, stale_base).await;
    let stale_b = seed_session_with_messages(&storage, "char-0001", 2, stale_base + 5_000).await;
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

/// 阈值参数化：`tick_with_threshold` 按传入阈值判定（0 分钟 → 刚活跃会话也视为超时）。
#[tokio::test]
async fn tick_with_threshold_uses_given_threshold() {
    let (engine, storage, dir) = engine_with_l1_reply("idle-threshold", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session = seed_session_with_messages(&storage, "char-0001", 2, now_ms()).await;

    // 缺省口径（10 分钟）：刚活跃 → 不封存
    assert_eq!(tick(&engine).await.expect("空闲检查应成功"), 0);

    // 阈值 0 分钟：立即视为超时 → 封存
    assert_eq!(
        tick_with_threshold(&engine, 0)
            .await
            .expect("空闲检查应成功"),
        1,
        "阈值 0 分钟时刚活跃会话也应封存"
    );
    let row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_some(), "会话应被关闭");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 封存门禁：许可关闭时整轮跳过
/// （超时会话不封存、不生成 L1、会话保持活跃）。
#[tokio::test]
async fn tick_is_noop_when_seal_disabled() {
    let (engine, storage, dir) = engine_with_l1_reply("idle-gated", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

    engine.set_seal_allowed(false);
    assert_eq!(
        engine.tick_idle().await.expect("空闲检查应成功"),
        0,
        "许可关闭时不应封存任何会话"
    );

    let row = storage
        .get_session(session)
        .await
        .expect("查询应成功")
        .expect("会话应存在");
    assert!(row.ended_at.is_none(), "许可关闭时会话应保持活跃");
    assert!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "许可关闭时不应生成 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// L1 摘要补扫：封存中 L1 失败登记的 pending 任务，在 LLM 恢复后
/// 由空闲检查自动补跑（MCP 独用无桌面时的消费点）。
#[tokio::test]
async fn tick_consumes_pending_l1_retry_after_llm_recovers() {
    let (engine, storage, dir) = engine_with_failing_llm("idle-retry").await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

    // 第一轮：超时会话被抢占关闭，但 L1 生成失败 → 登记 pending 重试任务
    // （返回 0：封存失败不计入成功数，但会话已被抢占关闭）
    assert_eq!(
        engine.tick_idle().await.expect("空闲检查应成功"),
        0,
        "L1 生成失败不应计入封存成功数"
    );
    let closed = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(closed.ended_at.is_some(), "超时会话应已被抢占关闭");
    let pending = storage
        .list_pending_jobs()
        .await
        .expect("查询 pending 应成功");
    assert!(
        pending
            .iter()
            .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
        "L1 失败应登记 l1_summary_retry pending 任务: {pending:?}"
    );
    assert!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "LLM 不可用时不应生成 L1"
    );

    // LLM 恢复：同库第二台引擎（成功 mock）执行空闲检查 → 先补扫 pending，产出摘要
    let recovered = engine_on_existing_db(
        &dir.join("assistant.db"),
        MockLlm::with_reply(L1_JSON_REPLY),
        RamariaConfig::default(),
    )
    .await;
    assert_eq!(
        recovered.tick_idle().await.expect("空闲检查应成功"),
        0,
        "会话已关闭，本轮无需封存"
    );
    assert_eq!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "LLM 恢复后空闲检查应补跑出 L1"
    );
    let remaining = storage
        .list_pending_jobs()
        .await
        .expect("查询 pending 应成功");
    assert!(
        !remaining
            .iter()
            .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
        "补跑成功后任务不应再停留 pending: {remaining:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 补扫只消费补偿登记类型（`l1_summary_retry`）：在途生成任务（`l1_summary`）
/// 处于 pending 窗口时不得被误取（误取会重复生成同一会话的 L1）。
#[tokio::test]
async fn tick_retry_ignores_inflight_l1_generation_jobs() {
    let (engine, storage, dir) = engine_with_l1_reply("idle-retry-type", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
    // 模拟"已在途生成"现场：会话已关闭 + 一条旧类型（l1_summary）pending 任务
    storage
        .close_session(session)
        .await
        .expect("关闭会话应成功");
    let payload = serde_json::json!({ "session_id": session.to_string() }).to_string();
    storage
        .create_background_job("l1_summary", Some(&payload))
        .await
        .expect("登记任务应成功");

    assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);
    assert!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "在途生成任务类型（l1_summary）不应被补扫消费"
    );
    let pending = storage
        .list_pending_jobs()
        .await
        .expect("查询 pending 应成功");
    assert!(
        pending
            .iter()
            .any(|(_, job_type, _)| job_type == "l1_summary"),
        "旧类型任务应保持原状态（由真正的执行方收敛）: {pending:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 补扫并发去重：两台引擎同时补扫同一 pending 补偿任务 →
/// 原子抢占保证只有一方执行，最终恰好一份 L1。
#[tokio::test]
async fn concurrent_retry_from_two_engines_generates_once() {
    // 第一轮用恒失败 LLM：L1 失败 → 登记补偿任务（会话已被抢占关闭）
    let (engine_a, storage, dir) = engine_with_failing_llm("idle-retry-race").await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
    assert_eq!(engine_a.tick_idle().await.expect("空闲检查应成功"), 0);

    // LLM 恢复：同一库上两台引擎并发补扫
    let db_path = dir.join("assistant.db");
    let engine_b = engine_on_existing_db(
        &db_path,
        MockLlm::with_reply(L1_JSON_REPLY),
        RamariaConfig::default(),
    )
    .await;
    let engine_c = engine_on_existing_db(
        &db_path,
        MockLlm::with_reply(L1_JSON_REPLY),
        RamariaConfig::default(),
    )
    .await;
    let (result_b, result_c) = tokio::join!(engine_b.tick_idle(), engine_c.tick_idle());
    result_b.expect("空闲检查应成功");
    result_c.expect("空闲检查应成功");

    assert_eq!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "并发补扫不得产生重复 L1（原子抢占去重）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 宿主循环（进程内空闲检测）
// =========================================================

/// 选项夹取：配置小于下限时按 [`MIN_IDLE_CHECK_INTERVAL_SECONDS`] 处理（防热循环）。
#[test]
fn idle_loop_options_clamp_configured_interval() {
    let mut config = RamariaConfig::default();
    assert_eq!(
        IdleLoopOptions::from_config(&config).interval_seconds(),
        config.session.idle_check_interval_seconds as u64,
        "配置缺省（60s）应原样生效"
    );

    config.session.idle_check_interval_seconds = 0;
    assert_eq!(
        IdleLoopOptions::from_config(&config).interval_seconds(),
        MIN_IDLE_CHECK_INTERVAL_SECONDS,
        "0 秒应被夹取到下限（tokio 定时器不接受零周期）"
    );

    // 显式构造不做夹取（测试与特殊宿主自行保证间隔合法）
    assert_eq!(IdleLoopOptions::new(1).interval_seconds(), 1);
}

/// 循环自动封存：拉起后按间隔扫描，超时会话被封闭并生成 L1；关停后不再运行。
#[tokio::test]
async fn idle_loop_seals_timed_out_session_then_stops() {
    let (engine, storage, dir) = engine_with_l1_reply("idle-loop", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    // 20 分钟前最后发言 → 超过 10 分钟空闲阈值
    let session =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

    // 间隔 1 秒（首次检查延后一个周期，不会在拉起瞬间就触发 LLM）
    let engine = Arc::new(engine);
    let mut idle_loop = engine.spawn_idle_loop_with(IdleLoopOptions::new(1));
    assert!(idle_loop.is_running(), "拉起后循环应处于运行状态");

    // 轮询等待自动封存完成（最多 6 秒）：不依赖固定 sleep，避免慢机偶发失败；
    // 同时等待"会话已关闭 + L1 已落库"——抢占关闭与 L1 生成落库之间存在窗口，
    // 只看关闭会在窗口内误判为"未生成 L1"。
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        let session_row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        let l1_count = storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len();
        if session_row.ended_at.is_some() && l1_count == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 跳出后按最终状态断言（超时同样走到这里，给出准确的失败原因）
    let session_row = storage
        .get_session(session)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(
        session_row.ended_at.is_some(),
        "空闲检查循环应在间隔内自动封存超时会话"
    );
    assert_eq!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "自动封存应生成 L1 摘要"
    );

    // 优雅关停：置停止位并等待在途轮次结束
    idle_loop.shutdown().await;
    assert!(!idle_loop.is_running(), "关停后循环不应再运行");
    // 重复关停为空操作（不阻塞、不报错）
    idle_loop.shutdown().await;

    let _ = std::fs::remove_dir_all(&dir);
}

/// 多宿主并发（服务层等价物）：同一库上两台引擎同时扫描 → 只生成一份 L1。
#[tokio::test]
async fn concurrent_tick_from_two_engines_seals_once() {
    let (engine_a, storage, dir) = engine_with_l1_reply("idle-concurrent", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session =
        seed_session_with_messages(&storage, "char-0001", 4, now_ms() - 20 * 60_000).await;

    // 第二台引擎：同一库文件、独立连接池与内存状态（模拟"桌面 + MCP"并存）
    let engine_b = engine_on_existing_db(
        &dir.join("assistant.db"),
        MockLlm::with_reply(L1_JSON_REPLY),
        RamariaConfig::default(),
    )
    .await;

    // 并发扫描：条件更新抢占保证只有一方进入封存链路
    let (result_a, result_b) = tokio::join!(engine_a.tick_idle(), engine_b.tick_idle());
    let sealed_a = result_a.expect("引擎 A 空闲检查应成功");
    let sealed_b = result_b.expect("引擎 B 空闲检查应成功");
    assert_eq!(
        sealed_a + sealed_b,
        1,
        "同一超时会话只允许一方抢到封存（抢占幂等）"
    );
    assert_eq!(
        storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "并发扫描不得产生重复 L1 摘要"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 循环与前台用例并存（并发手测的宿主层等价物）：空闲循环运行期间前台 `seal`
/// 照常工作，两者共同收敛全部超时会话，且每个会话恰好一份 L1（抢占幂等）。
#[tokio::test]
async fn idle_loop_and_foreground_seal_do_not_duplicate_l1() {
    let (engine, storage, dir) = engine_with_l1_reply("idle-foreground", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    // 两个超时会话：一个由前台抢占，另一个交给循环
    let session_front =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
    let session_loop =
        seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

    let engine = Arc::new(engine);
    let mut idle_loop = engine.spawn_idle_loop_with(IdleLoopOptions::new(1));

    // 前台立即抢封存（与循环的首轮扫描并发）：抢到与否都是合法结果，
    // 正确性由下方"每个会话恰好一份 L1"断言承担
    let _ = engine.seal(session_front).await.expect("前台封存不应报错");

    // 等待两个会话都被关闭且各有一份 L1（前台 + 循环共同收敛，上限 6 秒）
    //
    // 说明: 抢占（ended_at 置位）先于摘要写入完成，仅在"已关闭"时断言 L1 条数
    // 会命中该时间窗导致偶发抖动；等待条件因此同时要求 L1 落库。
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let front_closed = storage
            .get_session(session_front)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在")
            .ended_at
            .is_some();
        let loop_closed = storage
            .get_session(session_loop)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在")
            .ended_at
            .is_some();
        let front_l1 = storage
            .list_memory_l1(session_front)
            .await
            .expect("读取 L1 应成功")
            .len();
        let loop_l1 = storage
            .list_memory_l1(session_loop)
            .await
            .expect("读取 L1 应成功")
            .len();
        if front_closed && loop_closed && front_l1 == 1 && loop_l1 == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "循环与前台应在上限内关闭全部超时会话并各产出一份 L1（front_l1={front_l1}, loop_l1={loop_l1}）"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 并发封存不产生重复摘要：每个会话恰好一份 L1
    for session in [session_front, session_loop] {
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "会话 {session} 应恰好一份 L1（抢占幂等）"
        );
    }

    idle_loop.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
