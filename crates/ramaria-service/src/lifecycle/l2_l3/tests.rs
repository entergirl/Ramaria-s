//! crates/ramaria-service/src/lifecycle/l2_l3/tests.rs - Ramaria L2 触发与 L3 调度单元测试
//!
//! 设计特点:
//! - L2 即时触发：空库 / 未达阈值 / 达阈值触发吸收，阈值边界口径锁定
//! - 无主 L1：计数与时间两条触发路径的归属回填与停止位中断
//! - L3 即时触发：计数线与时间线（`0` = 关闭）的判定组合
//! - 定时调度：按配置时间线触发 L2 / L3，默认阈值行为回归，停止位关停
//! - 首轮判定：Keep-only 稳定轮不豁免漂移检测

use super::l3::is_first_inference_round;
use super::schedule::run_scheduled_check;
use super::unbound::process_unbound_l1_for_l2;
use super::*;
use crate::test_support::{
    MockLlm, engine_with_db, engine_with_llm_and_config, seed_l1, seed_persona,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{MemoryEvent, MemoryL1, now_ms};
use ramaria_memory::job::JobType;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 空库（无 persona）：检查正常返回，不产生任何任务。
#[tokio::test]
async fn empty_store_is_noop() {
    let (engine, storage, dir) = engine_with_db("l2l3-empty").await;

    check_l2_trigger(&engine, None).await;

    let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
    assert!(pending.is_empty(), "空库不应创建任务: {pending:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 未达阈值：不触发提取（L1 保持未吸收、无事件、无任务）。
#[tokio::test]
async fn below_threshold_does_not_trigger() {
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-below", MockLlm::local(), RamariaConfig::default()).await;
    seed_persona(&storage, "char-0001").await;
    // 1 条未吸收 L1 < 默认阈值 5
    seed_l1(
        &storage,
        "char-0001",
        "用户提到最近在准备考试",
        Some("考试"),
        1_000,
    )
    .await;

    check_l2_trigger(&engine, None).await;

    // L1 保持未吸收（未触发提取 → 未被吸收）
    let unabsorbed = storage
        .list_unabsorbed_l1("char-0001")
        .await
        .expect("查询未吸收 L1 应成功");
    assert_eq!(unabsorbed.len(), 1, "未达阈值不应吸收 L1");

    // 无事件产出
    let events = storage
        .list_events_by_persona("char-0001", 0, 100)
        .await
        .expect("查询事件应成功");
    assert!(events.is_empty(), "未达阈值不应产出事件");

    // 无事件提取任务登记
    let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
    assert!(
        !pending
            .iter()
            .any(|(_, job_type, _)| job_type == JobType::EventExtract.as_str()),
        "未达阈值不应创建事件提取任务: {pending:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无主 L1：达到计数阈值 → 按来源会话归属到 persona，并进入 L2 提取链路。
#[tokio::test]
async fn unbound_l1_is_attributed_and_triggers() {
    let mut config = RamariaConfig::default();
    config.thresholds.l2_trigger_count = 1;
    // 测试不等待簇间节流（生产默认 800ms）
    config.thresholds.cluster_delay_ms = 0;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "l2l3-unbound",
        MockLlm::with_reply(r#"{"events": []}"#),
        config,
    )
    .await;
    seed_persona(&storage, "char-0001").await;

    // 无主 L1（persona_uid = None），来源会话归 char-0001
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    check_l2_trigger(&engine, None).await;

    // 归属回填为来源会话的 persona
    let stored = storage
        .list_memory_l1(session.id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].persona_uid.as_deref(),
        Some("char-0001"),
        "无主 L1 应回填到来源会话的 persona"
    );
    // 无主通道清空
    let unbound = storage
        .list_unabsorbed_l1_unbound()
        .await
        .expect("查询无主 L1 应成功");
    assert!(unbound.is_empty(), "归属后无主通道应清空");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 时间触发（路径 B）：最早无主 L1 年龄 ≥ 阈值时触发（即使计数不足）。
#[tokio::test]
async fn unbound_l1_age_trigger() {
    let mut config = RamariaConfig::default();
    // 测试不等待簇间节流（生产默认 800ms）
    config.thresholds.cluster_delay_ms = 0;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "l2l3-unbound-age",
        MockLlm::with_reply(r#"{"events": []}"#),
        config,
    )
    .await;
    seed_persona(&storage, "char-0001").await;

    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    // 1 条 10 天前的无主 L1（时间触发阈值 7 天，计数阈值不启用）
    let mut l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
    l1.created_at = now_ms() - 10 * 86_400_000;
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    let stats = process_unbound_l1_for_l2(&engine, None, 0, 7.0).await;

    assert_eq!(stats.total, 1);
    assert_eq!(stats.triggered_personas, 1, "年龄 ≥ 7 天应触发 L2");
    assert_eq!(stats.pending_groups, 0);

    let bound = storage
        .list_recent_l1_by_persona("char-0001", 100)
        .await
        .expect("查询 L1 应成功");
    assert_eq!(bound.len(), 1, "时间触发同样应归属 L1");
    assert_eq!(bound[0].persona_uid.as_deref(), Some("char-0001"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 停止位置位：不处理任何 persona（无主 L1 保持无主、未回填）。
#[tokio::test]
async fn shutdown_flag_interrupts_check() {
    let mut config = RamariaConfig::default();
    config.thresholds.l2_trigger_count = 1;
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-shutdown", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    // 无主 L1 + 一条已归属未吸收 L1（两类输入都应保持原状）
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    seed_l1(&storage, "char-0001", "用户提到最近在准备考试", None, 1_000).await;

    let flag = AtomicBool::new(true);
    check_l2_trigger(&engine, Some(&flag)).await;

    // 无主 L1 保持无主
    let unbound = storage
        .list_unabsorbed_l1_unbound()
        .await
        .expect("查询应成功");
    assert_eq!(unbound.len(), 1, "停止位置位时无主 L1 不应被归属");
    assert!(unbound[0].persona_uid.is_none(), "无主 L1 归属不应被回填");
    // 已归属 L1 保持未吸收（未触发提取）
    let unabsorbed = storage
        .list_unabsorbed_l1("char-0001")
        .await
        .expect("查询应成功");
    assert_eq!(unabsorbed.len(), 1, "停止位置位时不应触发提取");

    let _ = std::fs::remove_dir_all(&dir);
}

/// L3 触发条件未满足：未吸收事件为空 → 不创建性格推断任务。
#[tokio::test]
async fn l3_trigger_skips_without_events() {
    let (engine, storage, dir) = engine_with_db("l2l3-l3-none").await;
    seed_persona(&storage, "char-0001").await;

    assert!(
        !check_l3_trigger(&engine, None, "char-0001").await,
        "无未吸收事件不应触发推断"
    );

    let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
    assert!(
        !pending
            .iter()
            .any(|(_, job_type, _)| job_type == JobType::PersonalityInference.as_str()),
        "无未吸收事件不应创建性格推断任务: {pending:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 即时路径：L3 时间线阈值 `0` = 不按时间触发 —— 超龄事件保持未吸收、不启动推断。
#[tokio::test]
async fn instant_check_zero_l3_days_disables_age_trigger() {
    let mut config = RamariaConfig::default();
    config.thresholds.l3_trigger_days = 0;
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-l3-zero", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    // 40 天前的事件：即使超过默认 30 天，阈值 0 下也不应触发
    let start = now_ms() - 40 * 86_400_000;
    let mut event = MemoryEvent::new(
        "char-0001".to_string(),
        "旧事件".to_string(),
        "很久以前发生的事件".to_string(),
        start,
        start + 3_600_000,
    );
    event.confidence = 0.8;
    storage.save_event(&event).await.expect("写入事件应成功");

    assert!(
        !check_l3_trigger(&engine, None, "char-0001").await,
        "时间线阈值 0 时不应按事件年龄触发推断"
    );
    let unabsorbed = storage
        .list_unabsorbed_events("char-0001")
        .await
        .expect("查询未吸收事件应成功");
    assert_eq!(unabsorbed.len(), 1, "未触发时事件应保持未吸收");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 即时路径：L3 时间线阈值大于 0 时生效 —— 超龄事件触发推断（对照 `0` 值关闭）。
#[tokio::test]
async fn instant_check_age_trigger_fires_when_days_enabled() {
    let mut config = RamariaConfig::default();
    config.thresholds.l3_trigger_days = 1;
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-l3-age", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    // 2 天前的事件：超过自定义阈值 1 天 → 应触发
    let start = now_ms() - 2 * 86_400_000;
    let mut event = MemoryEvent::new(
        "char-0001".to_string(),
        "工作压力事件".to_string(),
        "用户最近工作压力很大".to_string(),
        start,
        start + 3_600_000,
    );
    event.confidence = 0.8;
    storage.save_event(&event).await.expect("写入事件应成功");

    assert!(
        check_l3_trigger(&engine, None, "char-0001").await,
        "时间线阈值大于 0 且事件超龄时应触发推断"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 即时路径：时间线阈值 `0` 不影响计数线 —— 未吸收事件达计数阈值仍触发。
#[tokio::test]
async fn instant_check_count_trigger_ignores_disabled_age_line() {
    let mut config = RamariaConfig::default();
    config.thresholds.l3_trigger_days = 0;
    config.thresholds.l3_trigger_count = 1;
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-l3-count", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    let start = now_ms() - 3_600_000;
    let mut event = MemoryEvent::new(
        "char-0001".to_string(),
        "工作压力事件".to_string(),
        "用户最近工作压力很大".to_string(),
        start,
        start + 600_000,
    );
    event.confidence = 0.8;
    storage.save_event(&event).await.expect("写入事件应成功");

    assert!(
        check_l3_trigger(&engine, None, "char-0001").await,
        "时间线关闭不影响计数线：达计数阈值即触发"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 后台调度任务可按停止位关停（拉起后置位，限时内退出）。
#[tokio::test]
async fn spawn_scheduler_stops_on_flag() {
    let (engine, _storage, dir) = engine_with_db("l2l3-spawn").await;
    let engine = Arc::new(engine);
    let shutdown = Arc::new(AtomicBool::new(false));

    let handle = spawn_scheduler(Arc::clone(&engine), Arc::clone(&shutdown), 0, 1);
    shutdown.store(true, Ordering::Release);

    // 轮询等待退出（最多 6 秒）：不依赖固定 sleep，避免慢机偶发失败
    let deadline = Instant::now() + Duration::from_secs(6);
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "停止位置位后调度任务应在限时内退出"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle.await.expect("调度任务不应 panic");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 定时检查时间线（配置驱动）
// =========================================================

/// 定时检查：L2 时间线按 `[thresholds].l2_trigger_days` 判定 —— 2 天前的未吸收
/// L1 超过自定义阈值（1 天）时触发提取，L1 被吸收且有事件产出。
#[tokio::test]
async fn scheduled_check_uses_configured_l2_days() {
    let mut config = RamariaConfig::default();
    config.thresholds.l2_trigger_days = 1;
    // 测试不等待簇间节流（生产默认 800ms）
    config.thresholds.cluster_delay_ms = 0;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "l2l3-sched-l2",
        MockLlm::with_reply(r#"{"events": []}"#),
        config,
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    // 3 条 2 天前的未吸收 L1（关键词连通 → 单簇）；计数阈值 5 未满足
    let two_days_ago = now_ms() - 2 * 86_400_000;
    for i in 0..3 {
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大",
            Some("工作压力"),
            two_days_ago + i,
        )
        .await;
    }

    run_scheduled_check(&engine, None).await;

    let unabsorbed = storage
        .list_unabsorbed_l1("char-0001")
        .await
        .expect("查询未吸收 L1 应成功");
    assert!(
        unabsorbed.is_empty(),
        "超过自定义天数阈值应触发提取并吸收 L1: {unabsorbed:?}"
    );
    let events = storage
        .list_events_by_persona("char-0001", 0, 100)
        .await
        .expect("查询事件应成功");
    assert!(!events.is_empty(), "触发提取应有事件产出（降级事件亦可）");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 定时检查：L2 时间线阈值 `0` = 不按时间触发 —— 未吸收 L1 保持未吸收、无事件产出。
#[tokio::test]
async fn scheduled_check_zero_l2_days_disables_age_trigger() {
    let mut config = RamariaConfig::default();
    config.thresholds.l2_trigger_days = 0;
    config.thresholds.cluster_delay_ms = 0;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "l2l3-sched-l2-zero",
        MockLlm::with_reply(r#"{"events": []}"#),
        config,
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    // 3 条 10 天前的未吸收 L1：即使超过默认 7 天，阈值 0 下也不应触发
    let long_ago = now_ms() - 10 * 86_400_000;
    for i in 0..3 {
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大",
            Some("工作压力"),
            long_ago + i,
        )
        .await;
    }

    run_scheduled_check(&engine, None).await;

    let unabsorbed = storage
        .list_unabsorbed_l1("char-0001")
        .await
        .expect("查询未吸收 L1 应成功");
    assert_eq!(unabsorbed.len(), 3, "阈值 0 时时间线不触发，L1 保持未吸收");
    let events = storage
        .list_events_by_persona("char-0001", 0, 100)
        .await
        .expect("查询事件应成功");
    assert!(events.is_empty(), "未触发不应产出事件");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 定时检查：L3 时间线按 `[thresholds].l3_trigger_days` 判定 —— 2 天前的未吸收
/// 事件超过自定义阈值（1 天）时触发推断，事件被吸收。
#[tokio::test]
async fn scheduled_check_uses_configured_l3_days() {
    let mut config = RamariaConfig::default();
    config.thresholds.l3_trigger_days = 1;
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-sched-l3", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    let start = now_ms() - 2 * 86_400_000;
    let mut event = MemoryEvent::new(
        "char-0001".to_string(),
        "工作压力事件".to_string(),
        "用户最近工作压力很大".to_string(),
        start,
        start + 3_600_000,
    );
    event.confidence = 0.8; // ≥ 0.6 才参与性格推断
    storage.save_event(&event).await.expect("写入事件应成功");

    run_scheduled_check(&engine, None).await;

    let unabsorbed = storage
        .list_unabsorbed_events("char-0001")
        .await
        .expect("查询未吸收事件应成功");
    assert!(
        unabsorbed.is_empty(),
        "超过自定义天数阈值应触发 L3 推断并吸收事件: {unabsorbed:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 定时检查：L3 时间线阈值 `0` = 不按时间触发 —— 未吸收事件保持未吸收。
#[tokio::test]
async fn scheduled_check_zero_l3_days_disables_age_trigger() {
    let mut config = RamariaConfig::default();
    config.thresholds.l3_trigger_days = 0;
    let (engine, storage, dir) =
        engine_with_llm_and_config("l2l3-sched-l3-zero", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    // 40 天前的事件：即使超过默认 30 天，阈值 0 下也不应触发
    let start = now_ms() - 40 * 86_400_000;
    let mut event = MemoryEvent::new(
        "char-0001".to_string(),
        "旧事件".to_string(),
        "很久以前发生的事件".to_string(),
        start,
        start + 3_600_000,
    );
    event.confidence = 0.8;
    storage.save_event(&event).await.expect("写入事件应成功");

    run_scheduled_check(&engine, None).await;

    let unabsorbed = storage
        .list_unabsorbed_events("char-0001")
        .await
        .expect("查询未吸收事件应成功");
    assert_eq!(unabsorbed.len(), 1, "阈值 0 时时间线不触发，事件保持未吸收");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 定时检查：默认阈值（7 天）行为回归 —— 8 天前的未吸收 L1 触发提取，
/// 6 天前的保持未吸收。
#[tokio::test]
async fn scheduled_check_default_l2_days_behavior() {
    let mut config = RamariaConfig::default();
    config.thresholds.cluster_delay_ms = 0;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "l2l3-sched-default",
        MockLlm::with_reply(r#"{"events": []}"#),
        config,
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    // char-0001：3 条 8 天前的 L1（超过默认 7 天）→ 应触发
    let over_age = now_ms() - 8 * 86_400_000;
    for i in 0..3 {
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大",
            Some("工作压力"),
            over_age + i,
        )
        .await;
    }
    // char-0002：3 条 6 天前的 L1（未达默认 7 天）→ 不应触发
    let within_age = now_ms() - 6 * 86_400_000;
    for i in 0..3 {
        seed_l1(
            &storage,
            "char-0002",
            "用户最近睡眠质量不好",
            Some("睡眠"),
            within_age + i,
        )
        .await;
    }

    run_scheduled_check(&engine, None).await;

    let triggered = storage
        .list_unabsorbed_l1("char-0001")
        .await
        .expect("查询未吸收 L1 应成功");
    assert!(
        triggered.is_empty(),
        "默认 7 天阈值下 8 天前的 L1 应触发提取"
    );
    let pending = storage
        .list_unabsorbed_l1("char-0002")
        .await
        .expect("查询未吸收 L1 应成功");
    assert_eq!(
        pending.len(),
        3,
        "6 天前的 L1 未达默认 7 天阈值，保持未吸收"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 定时检查：默认阈值（30 天）行为回归 —— 40 天前的未吸收事件触发推断并吸收，
/// 20 天前的保持未吸收。
#[tokio::test]
async fn scheduled_check_default_l3_days_behavior() {
    let (engine, storage, dir) = engine_with_llm_and_config(
        "l2l3-sched-default-l3",
        MockLlm::local(),
        RamariaConfig::default(),
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;

    let over_age = now_ms() - 40 * 86_400_000;
    let mut triggered_event = MemoryEvent::new(
        "char-0001".to_string(),
        "旧事件".to_string(),
        "很久以前发生的事件".to_string(),
        over_age,
        over_age + 3_600_000,
    );
    triggered_event.confidence = 0.8;
    storage
        .save_event(&triggered_event)
        .await
        .expect("写入事件应成功");

    let within_age = now_ms() - 20 * 86_400_000;
    let mut pending_event = MemoryEvent::new(
        "char-0002".to_string(),
        "近期事件".to_string(),
        "近期发生的事件".to_string(),
        within_age,
        within_age + 3_600_000,
    );
    pending_event.confidence = 0.8;
    storage
        .save_event(&pending_event)
        .await
        .expect("写入事件应成功");

    run_scheduled_check(&engine, None).await;

    let triggered = storage
        .list_unabsorbed_events("char-0001")
        .await
        .expect("查询未吸收事件应成功");
    assert!(
        triggered.is_empty(),
        "默认 30 天阈值下 40 天前的事件应触发推断并吸收"
    );
    let pending = storage
        .list_unabsorbed_events("char-0002")
        .await
        .expect("查询未吸收事件应成功");
    assert_eq!(
        pending.len(),
        1,
        "20 天前的事件未达默认 30 天阈值，保持未吸收"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// L3 首轮判定（稳定轮不豁免漂移检测）
// =========================================================

/// 回归：Keep-only 稳定轮（traits_updated / traits_deprecated 均为 0，
/// 但本轮仍产出活跃 trait）不是首轮，必须继续执行漂移检测。
#[test]
fn keep_only_round_is_not_first_round() {
    use ramaria_memory::inference::{PhaseBResult, PhaseBSource};

    let keep_only = PhaseBResult {
        traits_saved: 0,
        traits_updated: 0,
        traits_deprecated: 0,
        source: PhaseBSource::LlmInference,
        trait_ids: vec![1, 2],
        traits: vec![],
    };
    assert!(
        !is_first_inference_round(&keep_only),
        "Keep-only 稳定轮不应被判为首轮（否则漂移检测被跳过）"
    );

    let no_trait = PhaseBResult {
        traits_saved: 0,
        traits_updated: 0,
        traits_deprecated: 0,
        source: PhaseBSource::MockFallback,
        trait_ids: vec![],
        traits: vec![],
    };
    assert!(
        is_first_inference_round(&no_trait),
        "仅当本轮无任何活跃 trait 时才视为首轮"
    );
}
