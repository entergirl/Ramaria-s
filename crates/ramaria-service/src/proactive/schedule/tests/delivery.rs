//! crates/ramaria-service/src/proactive/schedule/tests/delivery.rs - 投放路由与门禁
//!
//! 设计特点:
//! - 由 父测试模块 以 mod delivery; 收纳，经 use super::* 取用父模块辅助桩与装配。
//! - 覆盖：本地 / 通道分流、通道四层门禁（总开关 / 绑定 / 接收端 / 外部标识）、
//!   平台失败不记账、本地失败维持既有记账口径、全局计数计费条件。
//! - 用固定时间戳驱动单轮调度，装配真实 SQLite 临时库与 mock LLM，不依赖网络与真实时钟。
//! - 断言口径：接收端留档、库中 reload、全局状态 reload。

use super::*;
use crate::proactive::schedule::{ProactiveTickSummary, run_tick};
use crate::proactive::sink::ProactiveMessage;
use crate::proactive::state::ProactiveState;
use crate::test_support::{MockLlm, engine_with_llm_and_config};
use ramaria_core::types::now_ms;

// =========================================================
// 场景参数与辅助
// =========================================================

/// 通道投递场景参数（默认全放行，按需覆写单项）。
#[derive(Clone, Copy)]
struct ChannelCase {
    /// 通道总开关（`[channel].proactive_enabled`）。
    proactive_enabled: bool,
    /// 会话外部标识（None = 会话无外部标识）。
    session_external_ref: Option<&'static str>,
    /// 是否预先建立绑定。
    create_binding: bool,
    /// 绑定的启用开关。
    binding_enabled: bool,
    /// 绑定的主动投递开关。
    binding_allow_proactive: bool,
    /// 是否注册通道投递接收端。
    register_channel_sink: bool,
}

impl ChannelCase {
    /// 全放行基线：开关开、绑定存在且启用且允许主动、接收端已注册。
    fn open() -> Self {
        Self {
            proactive_enabled: true,
            session_external_ref: Some("c2c:10001"),
            create_binding: true,
            binding_enabled: true,
            binding_allow_proactive: true,
            register_channel_sink: true,
        }
    }
}

/// 执行一个通道投递场景：装配 → 造通道会话（+绑定）→ 注册接收端 → 单轮调度。
///
/// 返回:
/// - 单轮摘要 / 人格状态 / 通道接收端留档 / 本地接收端留档 / 临时目录。
async fn run_channel_case(
    tag: &str,
    case: ChannelCase,
) -> (
    ProactiveTickSummary,
    ProactiveState,
    Vec<ProactiveMessage>,
    Vec<ProactiveMessage>,
    std::path::PathBuf,
) {
    let mut config = test_config();
    config.channel.proactive_enabled = case.proactive_enabled;
    let (engine, storage, dir) =
        engine_with_llm_and_config(tag, MockLlm::with_reply(REPLY), config).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let session_id = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", case.session_external_ref)
        .await
        .expect("创建通道会话应成功")
        .id;
    if case.create_binding {
        engine
            .channel_set_binding(
                "qq",
                case.session_external_ref.unwrap_or("c2c:10001"),
                None,
                Some(case.binding_enabled),
                Some(case.binding_allow_proactive),
            )
            .await
            .expect("绑定写入应成功");
    }

    let local_sink = Arc::new(TestSink::ok());
    let channel_sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(local_sink.clone());
    if case.register_channel_sink {
        engine.set_channel_proactive_sink(channel_sink.clone());
    }

    let picker = CountingPicker::returning(directive(Some(session_id)));
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    let reloaded = reload_state(&storage).await;
    (
        summary,
        reloaded,
        channel_sink.received(),
        local_sink.received(),
        dir,
    )
}

// =========================================================
// 路由分流
// =========================================================

/// 通道消息由通道接收端投递（本地接收端零调用）；本地消息由本地接收端投递。
#[tokio::test]
async fn deliver_routes_channel_message_to_channel_sink() {
    let (summary, _state, channel_received, local_received, dir) =
        run_channel_case("proactive-route-channel", ChannelCase::open()).await;
    assert_eq!(summary.generated, 1);
    assert_eq!(summary.delivered, 1, "通道投递应成功");
    assert_eq!(channel_received.len(), 1, "通道接收端应收到");
    assert!(local_received.is_empty(), "本地接收端不应被调用");
    let message = &channel_received[0];
    assert_eq!(message.channel, "qq");
    assert_eq!(message.external_ref.as_deref(), Some("c2c:10001"));
    assert_eq!(message.content, REPLY);
    let _ = std::fs::remove_dir_all(dir);

    // 本地会话 → 本地接收端（通道接收端零调用）
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-route-local",
        MockLlm::with_reply(REPLY),
        test_config(),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let session_id = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("创建本地会话应成功")
        .id;

    let local_sink = Arc::new(TestSink::ok());
    let channel_sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(local_sink.clone());
    engine.set_channel_proactive_sink(channel_sink.clone());
    let picker = CountingPicker::returning(directive(Some(session_id)));
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.generated, 1);
    assert_eq!(summary.delivered, 1, "本地投递应成功");
    assert_eq!(local_sink.received().len(), 1, "本地消息应由本地接收端收到");
    assert!(channel_sink.received().is_empty(), "通道接收端不应被调用");
    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 通道门禁：任一不过即丢弃（零发送、零记账）
// =========================================================

/// 六类门禁场景逐项：丢弃 = 零发送 + 不记账（投递时间 / 当日计数 / 选题冷却均不更新）。
#[tokio::test]
async fn channel_gates_drop_when_disabled() {
    let open = ChannelCase::open();
    let cases = [
        (
            "gate-switch-off",
            ChannelCase {
                proactive_enabled: false,
                ..open
            },
        ),
        (
            "gate-binding-disabled",
            ChannelCase {
                binding_enabled: false,
                ..open
            },
        ),
        (
            "gate-binding-no-proactive",
            ChannelCase {
                binding_allow_proactive: false,
                ..open
            },
        ),
        (
            "gate-binding-missing",
            ChannelCase {
                create_binding: false,
                ..open
            },
        ),
        (
            "gate-sink-absent",
            ChannelCase {
                register_channel_sink: false,
                ..open
            },
        ),
        (
            "gate-external-ref-missing",
            ChannelCase {
                session_external_ref: None,
                ..open
            },
        ),
    ];

    for (tag, case) in cases {
        let (summary, state, channel_received, local_received, dir) =
            run_channel_case(tag, case).await;
        assert_eq!(summary.generated, 1, "{tag}: 生成应成功");
        assert_eq!(summary.delivered, 0, "{tag}: 应被丢弃（未投递）");
        assert!(channel_received.is_empty(), "{tag}: 通道接收端不应收到");
        assert!(local_received.is_empty(), "{tag}: 本地接收端不应收到");
        assert_eq!(state.last_sent_at, None, "{tag}: 丢弃不应记投递时间");
        assert_eq!(state.daily_count, 0, "{tag}: 丢弃不应记当日计数");
        assert!(state.recent_topics.is_empty(), "{tag}: 丢弃不应记选题冷却");
        let _ = std::fs::remove_dir_all(dir);
    }
}

// =========================================================
// 失败语义与计费
// =========================================================

/// 通道平台失败：不记账（投递时间 / 当日计数 / 退避累计 / 选题冷却均不更新）。
#[tokio::test]
async fn platform_failure_not_billed() {
    let mut config = test_config();
    config.channel.proactive_enabled = true;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-platform-fail",
        MockLlm::with_reply(REPLY),
        config,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let session_id = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("创建通道会话应成功")
        .id;
    engine
        .channel_set_binding("qq", "c2c:10001", None, Some(true), Some(true))
        .await
        .expect("绑定写入应成功");
    engine.set_proactive_sink(Arc::new(TestSink::ok()));
    engine.set_channel_proactive_sink(Arc::new(TestSink::failing()));

    let picker = CountingPicker::returning(directive(Some(session_id)));
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.generated, 1);
    assert_eq!(summary.delivered, 0, "平台失败应记为未投递");

    let reloaded = reload_state(&storage).await;
    assert_eq!(reloaded.last_sent_at, None, "平台失败不应记投递时间");
    assert_eq!(reloaded.daily_count, 0, "平台失败不应记当日计数");
    assert_eq!(reloaded.silence_streak, 0, "平台失败不应累计退避");
    assert!(reloaded.recent_topics.is_empty(), "平台失败不应记选题冷却");
    let _ = std::fs::remove_dir_all(dir);
}

/// 平台失败不消耗全局日上限（全局计数与人格级同口径）。
#[tokio::test]
async fn global_count_skips_platform_failure() {
    let mut config = test_config();
    config.channel.proactive_enabled = true;
    config.proactive.daily_total_limit = 5;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-global-platform-fail",
        MockLlm::with_reply(REPLY),
        config,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let session_id = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("创建通道会话应成功")
        .id;
    engine
        .channel_set_binding("qq", "c2c:10001", None, Some(true), Some(true))
        .await
        .expect("绑定写入应成功");
    engine.set_proactive_sink(Arc::new(TestSink::ok()));
    engine.set_channel_proactive_sink(Arc::new(TestSink::failing()));

    let picker = CountingPicker::returning(directive(Some(session_id)));
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.delivered, 0);

    let global = reload_global(&storage).await;
    assert_eq!(global.daily_count, 0, "平台失败不应计入全局日计数");
    let _ = std::fs::remove_dir_all(dir);
}

/// 本地失败：维持既有记账口径（消息已落库应用内可见，计投递时间与全局计数）。
#[tokio::test]
async fn local_failure_still_billed() {
    let mut config = test_config();
    config.proactive.daily_total_limit = 5;
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-local-fail-billed",
        MockLlm::with_reply(REPLY),
        config,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    engine.set_proactive_sink(Arc::new(TestSink::failing()));
    let picker = CountingPicker::returning(directive(None));
    let now = now_ms();
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(summary.generated, 1);
    assert_eq!(summary.delivered, 0, "本地失败应记为未投递");

    let reloaded = reload_state(&storage).await;
    assert_eq!(
        reloaded.last_sent_at,
        Some(now),
        "本地失败维持既有记账口径（消息已落库）"
    );
    assert_eq!(reloaded.daily_count, 1);
    assert!(reloaded.recent_topics.is_empty(), "投递失败不应记选题冷却");

    let global = reload_global(&storage).await;
    assert_eq!(global.daily_count, 1, "本地失败仍计入全局日计数");
    let _ = std::fs::remove_dir_all(dir);
}
