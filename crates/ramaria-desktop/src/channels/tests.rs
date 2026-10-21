//! crates/ramaria-desktop/src/channels/tests.rs - 桌面通道运行时管理单元测试
//!
//! 设计特点:
//! - 事件接收端以 NoopSink 注入（同 `start_core` 路径）：不构造 Tauri 句柄，不依赖事件循环
//! - 使用临时文件库装配真实引擎（全量 migration）：验证配置读取与凭据降级路径
//! - 通道装配仅用 QQ（指向本机不可达端口，首次连接失败走降级），不触达外网
//!
//! 安全约束:
//! - 不访问真实平台凭据（keychain 读取失败按未配置降级）
//! - 临时目录由用例负责清理

use super::*;
use ramaria_channel::{ConnectionState, NoopSink};
use ramaria_service::EngineOptions;
use std::path::PathBuf;

/// 启用 QQ 的测试配置（地址指向本机不可达端口，连接失败走自愈降级）。
const QQ_CONFIG: &str =
    "[channel]\nenabled = true\nqq_enabled = true\nqq_ws_url = \"ws://127.0.0.1:1\"\n";

/// 装配临时库引擎（配置可注入；不写入真实用户目录）。
async fn test_engine(tag: &str, config_toml: Option<&str>) -> (Arc<Engine>, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ramaria-desktop-channel-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    let config_path = dir.join("config.toml");
    if let Some(text) = config_toml {
        std::fs::write(&config_path, text).expect("写入配置应成功");
    }
    let engine = ramaria_service::Engine::open_with(
        EngineOptions::new(dir.join("ramaria.db")).with_config_path(config_path),
    )
    .await
    .expect("引擎装配应成功");
    (Arc::new(engine), dir)
}

/// 配置为空（通道全默认关闭）：返回未启动，不报错、不运行。
#[tokio::test]
async fn empty_config_returns_not_started() {
    let (engine, dir) = test_engine("empty", None).await;
    let manager = ChannelManager::new();

    let outcome = manager.start_core(engine, Arc::new(NoopSink), 0).await;
    assert!(!outcome.started, "未启用时不应启动");
    assert!(outcome.reason.is_some(), "应携带未启动原因");
    assert!(outcome.channels.is_empty());
    assert!(!manager.is_running().await);
    assert!(manager.statuses().await.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 启用 QQ：启动后状态列表含初始项；重复 start 幂等返回现状。
#[tokio::test]
async fn start_registers_channel_status_and_is_idempotent() {
    let (engine, dir) = test_engine("start", Some(QQ_CONFIG)).await;
    let manager = ChannelManager::new();

    let outcome = manager
        .start_core(engine.clone(), Arc::new(NoopSink), 0)
        .await;
    assert!(outcome.started, "reason={:?}", outcome.reason);
    assert_eq!(outcome.channels.len(), 1);
    assert_eq!(outcome.channels[0].channel, "qq");
    assert!(
        matches!(
            outcome.channels[0].state.as_str(),
            "disconnected" | "connecting" | "online" | "error"
        ),
        "状态文本应为四态之一：{}",
        outcome.channels[0].state
    );

    assert!(manager.is_running().await);
    let statuses = manager.statuses().await;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].channel, "qq");

    // 重复 start：幂等返回当前状态，不重复装配
    let again = manager.start_core(engine, Arc::new(NoopSink), 0).await;
    assert!(again.started, "重复 start 应幂等成功");
    assert_eq!(again.channels.len(), 1);

    manager.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 互斥端口被占用：返回未启动且携带原因；释放后同一端口可启动。
#[tokio::test]
async fn mutex_blocks_second_instance_on_same_port() {
    let (engine_a, dir_a) = test_engine("mutex-a", Some(QQ_CONFIG)).await;
    let (engine_b, dir_b) = test_engine("mutex-b", Some(QQ_CONFIG)).await;

    // 手动占位（模拟另一实例已持有通道运行时）
    let holder = TcpListener::bind(("127.0.0.1", 0)).expect("占位监听应成功");
    let port = holder.local_addr().expect("读取本地地址应成功").port();

    let manager = ChannelManager::new();
    let outcome = manager.start_core(engine_a, Arc::new(NoopSink), port).await;
    assert!(!outcome.started, "端口被占用时不应启动");
    assert!(outcome.reason.is_some());
    assert!(!manager.is_running().await);

    // 释放后同一端口可启动
    drop(holder);
    let outcome = manager.start_core(engine_b, Arc::new(NoopSink), port).await;
    assert!(outcome.started, "reason={:?}", outcome.reason);
    manager.stop().await;

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// stop 幂等：未启动直接返回；启动 → 停止 → 可再次启动。
#[tokio::test]
async fn stop_is_idempotent_and_restartable() {
    let manager = ChannelManager::new();
    manager.stop().await; // 未启动：无 panic

    let (engine, dir) = test_engine("restart", Some(QQ_CONFIG)).await;
    let outcome = manager
        .start_core(engine.clone(), Arc::new(NoopSink), 0)
        .await;
    assert!(outcome.started, "reason={:?}", outcome.reason);

    manager.stop().await;
    assert!(!manager.is_running().await);
    assert!(manager.statuses().await.is_empty());
    manager.stop().await; // 二次停止：幂等

    let outcome = manager.start_core(engine, Arc::new(NoopSink), 0).await;
    assert!(outcome.started, "停止后应可再次启动：{:?}", outcome.reason);
    manager.stop().await;

    let _ = std::fs::remove_dir_all(&dir);
}

/// 停止释放互斥端口：stop 后同端口可被外部重新绑定。
#[tokio::test]
async fn stop_releases_mutex_port() {
    let (engine, dir) = test_engine("release", Some(QQ_CONFIG)).await;
    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("探测监听应成功");
    let port = probe.local_addr().expect("读取本地地址应成功").port();
    drop(probe);

    let manager = ChannelManager::new();
    let outcome = manager.start_core(engine, Arc::new(NoopSink), port).await;
    assert!(outcome.started, "reason={:?}", outcome.reason);

    manager.stop().await;
    let rebound = TcpListener::bind(("127.0.0.1", port));
    assert!(
        rebound.is_ok(),
        "停止后互斥端口应已释放：{:?}",
        rebound.err()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 状态映射：state 文本、last_error 传递与时间戳逐项一致。
#[test]
fn status_to_view_maps_fields() {
    let mut status = ChannelStatus::new("qq");
    status.transition(
        ConnectionState::Error,
        Some("连接被拒绝".to_string()),
        1_700_000_000_000,
    );
    let view = status_to_view(&status);
    assert_eq!(view.channel, "qq");
    assert_eq!(view.state, "error");
    assert_eq!(view.last_error.as_deref(), Some("连接被拒绝"));
    assert_eq!(view.updated_at, 1_700_000_000_000);

    let fresh = ChannelStatus::new("telegram");
    let view = status_to_view(&fresh);
    assert_eq!(view.state, "disconnected");
    assert!(view.last_error.is_none(), "非错误态应无最近错误");
    assert_eq!(view.updated_at, 0);
}
