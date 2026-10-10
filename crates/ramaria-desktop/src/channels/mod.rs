//! crates/ramaria-desktop/src/channels/mod.rs - Ramaria 桌面通道运行时管理模块
//!
//! 设计特点:
//! - 桌面托管：通道运行时（传输 + 编排任务 + 主动投递注册）由本管理器统一启停，退出随进程收敛
//! - 连接互斥：固定本地端口占位（绑定即持有，随管理器释放），防止双开实例重复连接同一协议端
//! - 降级不阻塞：未启用 / 参数不完整 / 互斥失败均返回结构化结果，不阻塞桌面启动
//! - 状态上报：运行时状态经 Tauri 事件桥（`channel-status`）推送前端；日志只记通道名与分类
//! - 令牌纪律：平台令牌只随运行时内存持有，不落日志

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;

use ramaria_channel::{
    ChannelEventSink, ChannelProactiveSink, ChannelRuntime, ChannelRuntimeConfig, ChannelStatus,
    ChannelWorker, HttpImageDownloader, ImageDownloader, OneBotTransport, QqTransportOptions,
    TelegramFileDownloader, TelegramTransport, TelegramTransportOptions, Transport, WorkerOptions,
};
use ramaria_core::types::ChannelPlatform;
use ramaria_service::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use crate::events::{ChannelStatusPayload, EVENT_CHANNEL_STATUS};

// =========================================================
// 常量
// =========================================================

/// 通道运行时连接互斥端口（固定 loopback 绑定）。
///
/// 用途:
/// - 同一时刻只允许一个实例托管通道运行时（防双开：两实例同时连同一协议端会重复收发消息）；
/// - 进程退出 / 崩溃由操作系统回收监听，无需人工清理；
/// - 绑定失败 = 另一实例已持有通道运行时，或该端口被其它程序占用。
pub(crate) const CHANNEL_MUTEX_PORT: u16 = 47653;

// =========================================================
// 状态视图
// =========================================================

/// 通道状态视图（命令返回与事件负载共用）。
///
/// 序列化约定：camelCase 对齐前端 JS 访问（`status.lastError` / `status.updatedAt`）。
///
/// 字段约定:
/// - `state`: 连接状态文本（disconnected / connecting / online / error）。
/// - `last_error`: 最近错误（脱敏后文本）；仅 error 态有值。
/// - `updated_at`: 最近一次流转时间（Unix 毫秒；初始为 0）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStatusView {
    pub channel: String,
    pub state: String,
    pub last_error: Option<String>,
    pub updated_at: i64,
}

/// 通道启动结果（含未启动原因与当前状态列表）。
///
/// 字段约定:
/// - `started`: 是否处于运行态（含重复 start 的幂等返回）；
/// - `reason`: 未启动原因（`started = true` 时为 None）；
/// - `channels`: 已装配通道的状态列表（未运行时为空）。
#[derive(Debug, Clone)]
pub(crate) struct ChannelStartOutcome {
    pub started: bool,
    pub reason: Option<String>,
    pub channels: Vec<ChannelStatusView>,
}

/// 状态快照 → 视图（字段映射的唯一入口，便于单测锁定）。
pub(crate) fn status_to_view(status: &ChannelStatus) -> ChannelStatusView {
    ChannelStatusView {
        channel: status.channel.clone(),
        state: status.state.as_str().to_string(),
        last_error: status.last_error.clone(),
        updated_at: status.updated_at,
    }
}

// =========================================================
// 事件回调
// =========================================================

/// 桌面事件接收端：运行时状态变更转发为 Tauri 事件。
///
/// 实现约定:
/// - 回调同步且不阻塞：仅做事件发射（失败记 warn，不重试）；
/// - 事件负载携带本次变更的单通道状态，前端按 `channel` 合并更新。
struct DesktopChannelSink<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> ChannelEventSink for DesktopChannelSink<R> {
    fn on_status_changed(&self, status: &ChannelStatus) {
        let payload = ChannelStatusPayload {
            channels: vec![status_to_view(status)],
        };
        if let Err(e) = self.app.emit(EVENT_CHANNEL_STATUS, payload) {
            tracing::warn!(channel = %status.channel, error = %e, "通道状态事件发射失败");
        }
    }
}

// =========================================================
// 运行管理器
// =========================================================

/// 通道运行时管理器（桌面托管状态的一部分；Clone 廉价——内部 Arc）。
#[derive(Clone)]
pub struct ChannelManager {
    active: Arc<tokio::sync::Mutex<Option<ActiveChannels>>>,
}

/// 已启动的一组通道资源（Drop 顺序：worker → transport → 互斥监听）。
struct ActiveChannels {
    runtime: Arc<ChannelRuntime>,
    transports: HashMap<String, Arc<dyn Transport>>,
    workers: Vec<ChannelWorker>,
    /// 所属引擎（停止时回置通道投递接收端）。
    engine: Arc<Engine>,
    /// 互斥监听器：持有即占用端口；随本结构 Drop 释放。
    _mutex_guard: ChannelMutexGuard,
}

/// 连接互斥占位（仅包一个已绑定的 loopback 监听器）。
struct ChannelMutexGuard {
    _listener: TcpListener,
}

impl ChannelMutexGuard {
    /// 尝试绑定互斥端口；被占用返回 None。
    fn acquire(port: u16) -> Option<Self> {
        TcpListener::bind(("127.0.0.1", port))
            .ok()
            .map(|listener| Self {
                _listener: listener,
            })
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelManager {
    /// 创建未运行的管理器。
    pub fn new() -> Self {
        Self {
            active: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// 是否处于运行态。
    pub(crate) async fn is_running(&self) -> bool {
        self.active.lock().await.is_some()
    }

    /// 全部通道状态列表（未运行时为空；顺序按通道名升序）。
    pub(crate) async fn statuses(&self) -> Vec<ChannelStatusView> {
        let guard = self.active.lock().await;
        guard
            .as_ref()
            .map(|active| {
                active
                    .runtime
                    .statuses()
                    .iter()
                    .map(status_to_view)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 启动通道运行时（生产入口；连接互斥使用固定端口）。
    pub(crate) async fn start<R: Runtime>(
        &self,
        engine: Arc<Engine>,
        app: AppHandle<R>,
    ) -> ChannelStartOutcome {
        self.start_with_port(engine, app, CHANNEL_MUTEX_PORT).await
    }

    /// 启动通道运行时（可指定互斥端口；`port = 0` 由系统分配，供测试使用）。
    pub(crate) async fn start_with_port<R: Runtime>(
        &self,
        engine: Arc<Engine>,
        app: AppHandle<R>,
        port: u16,
    ) -> ChannelStartOutcome {
        self.start_core(engine, Arc::new(DesktopChannelSink { app }), port)
            .await
    }

    /// 启动核心（事件接收端以抽象注入；测试经 [`NoopSink`] 等价路径覆盖）。
    ///
    /// 流程:
    /// 1. 已运行 → 幂等返回当前状态；
    /// 2. 读配置与凭据（凭据读取失败按未配置降级）；
    /// 3. 两平台均未装配 → 返回未启动（原因"未启用或配置不完整"），不报错；
    /// 4. 连接互斥（绑定失败 → 返回未启动，原因含占用提示）；
    /// 5. 构造运行时与事件接收端，逐平台装配传输与编排任务；
    /// 6. 注册通道主动投递接收端（投递门禁检查据此放行）。
    ///
    /// 参数:
    /// - `engine`: 服务层引擎（通道消息处理与凭据读取入口）。
    /// - `sink`: 运行状态事件接收端（桌面 = Tauri 事件桥）。
    /// - `port`: 互斥端口（0 = 系统分配）。
    pub(crate) async fn start_core(
        &self,
        engine: Arc<Engine>,
        sink: Arc<dyn ChannelEventSink>,
        port: u16,
    ) -> ChannelStartOutcome {
        // ---- 1. 幂等：已运行直接返回现状 ----
        {
            let guard = self.active.lock().await;
            if let Some(active) = guard.as_ref() {
                return ChannelStartOutcome {
                    started: true,
                    reason: None,
                    channels: active
                        .runtime
                        .statuses()
                        .iter()
                        .map(status_to_view)
                        .collect(),
                };
            }
        }

        // ---- 2. 配置与凭据（读取失败按未配置降级，不阻塞启动） ----
        let config = match engine.load_full_config().await {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(error = %e, "读取配置失败，通道运行时未启动");
                return ChannelStartOutcome {
                    started: false,
                    reason: Some("读取配置失败".to_string()),
                    channels: Vec::new(),
                };
            }
        };
        let qq_token = load_credential_silent(&engine, ChannelPlatform::Qq);
        let telegram_token = load_credential_silent(&engine, ChannelPlatform::Telegram);

        // ---- 3. 运行期参数：两平台均未装配则不启动（未启用属正常路径） ----
        let runtime_config =
            ChannelRuntimeConfig::from_config(&config.channel, qq_token, telegram_token);
        if runtime_config.is_empty() {
            tracing::info!("通道未启用或配置不完整，跳过运行时启动");
            return ChannelStartOutcome {
                started: false,
                reason: Some("通道未启用或配置不完整".to_string()),
                channels: Vec::new(),
            };
        }

        // ---- 4. 连接互斥（绑定失败 = 另一实例已托管或端口被占用） ----
        let Some(mutex_guard) = ChannelMutexGuard::acquire(port) else {
            tracing::warn!(port, "通道运行时互斥端口被占用，跳过启动");
            return ChannelStartOutcome {
                started: false,
                reason: Some("另一实例运行中或端口被占用".to_string()),
                channels: Vec::new(),
            };
        };

        // ---- 5. 运行时与事件接收端 ----
        let runtime = Arc::new(ChannelRuntime::new(runtime_config.clone(), sink));

        let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
        let mut workers: Vec<ChannelWorker> = Vec::new();

        // ---- 5a. QQ（OneBot v11 正向 WS） ----
        if let Some(qq_config) = runtime_config.qq.clone() {
            let transport: Arc<dyn Transport> = Arc::new(OneBotTransport::new(
                qq_config,
                QqTransportOptions::default(),
                runtime.clone(),
            ));
            assemble_transport(
                &engine,
                &runtime,
                transport,
                Arc::new(HttpImageDownloader::new()),
                &mut transports,
                &mut workers,
            )
            .await;
        }

        // ---- 5b. Telegram（Bot API 长轮询） ----
        if let Some(telegram_config) = runtime_config.telegram.clone() {
            let options = TelegramTransportOptions::default();
            let downloader: Arc<dyn ImageDownloader> = Arc::new(TelegramFileDownloader::new(
                telegram_config.bot_token.clone(),
                &options.api_base_url,
            ));
            let transport: Arc<dyn Transport> = Arc::new(TelegramTransport::new(
                telegram_config,
                options,
                runtime.clone(),
            ));
            assemble_transport(
                &engine,
                &runtime,
                transport,
                downloader,
                &mut transports,
                &mut workers,
            )
            .await;
        }

        // ---- 6. 通道主动投递接收端（未注册时通道投递按门禁丢弃） ----
        engine.set_channel_proactive_sink(Arc::new(ChannelProactiveSink::new(transports.clone())));

        let channels: Vec<ChannelStatusView> =
            runtime.statuses().iter().map(status_to_view).collect();
        let started_count = channels.len();
        *self.active.lock().await = Some(ActiveChannels {
            runtime,
            transports,
            workers,
            engine,
            _mutex_guard: mutex_guard,
        });

        tracing::info!(channels = started_count, "通道运行时已启动");
        ChannelStartOutcome {
            started: true,
            reason: None,
            channels,
        }
    }

    /// 停止通道运行时（幂等；未运行时直接返回）。
    ///
    /// 流程:
    /// 1. 取走运行态（后续 start 可重新装配）；
    /// 2. 逐个关停编排任务（等待事件循环退出）；
    /// 3. 逐个关停传输（失败仅 warn）；
    /// 4. 回置通道投递接收端为空集（投递门禁检查回到"未装配"）；
    /// 5. 互斥监听随资源结构 Drop 自动释放。
    pub(crate) async fn stop(&self) {
        let Some(active) = self.active.lock().await.take() else {
            return;
        };
        let ActiveChannels {
            runtime,
            transports,
            workers,
            engine,
            _mutex_guard,
        } = active;

        for worker in workers {
            worker.shutdown().await;
        }
        for (name, transport) in &transports {
            if let Err(error) = transport.shutdown().await {
                tracing::warn!(channel = %name, error = %error, "通道传输关停失败");
            }
        }
        engine.set_channel_proactive_sink(Arc::new(ChannelProactiveSink::new(HashMap::new())));

        tracing::info!(channels = runtime.statuses().len(), "通道运行时已停止");
    }
}

/// 异步启动通道运行时（不阻塞调用方）。
///
/// 说明:
/// - 启动结果仅记日志：未启用 / 参数不完整 / 互斥失败均按降级处理，通道不可用不影响本地功能。
pub(crate) fn spawn_start<R: Runtime>(
    manager: ChannelManager,
    engine: Arc<Engine>,
    app: AppHandle<R>,
) {
    tauri::async_runtime::spawn(async move {
        let outcome = manager.start(engine, app).await;
        if outcome.started {
            tracing::info!(channels = outcome.channels.len(), "通道运行时已启动");
        } else {
            tracing::info!(
                reason = outcome.reason.as_deref().unwrap_or(""),
                "通道运行时未启动（降级不阻塞）"
            );
        }
    });
}

// =========================================================
// 装配辅助
// =========================================================

/// 读取平台凭据（读取失败按未配置降级；令牌内容不进日志）。
fn load_credential_silent(engine: &Engine, platform: ChannelPlatform) -> Option<String> {
    match engine.channel_load_credential(platform) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(platform = %platform.as_str(), error = %error, "通道凭据读取失败，按未配置处理");
            None
        }
    }
}

/// 装配单条传输：补初始状态、首次连接（失败仅 warn）、启动编排任务并登记。
///
/// 说明:
/// - 初始状态先落快照，首次连接的真实流转（connecting / online / error）随后覆盖；
/// - 首次连接失败不阻塞启动：传输层后台任务持续自愈重连（协议端后启动可自动连上）。
async fn assemble_transport(
    engine: &Arc<Engine>,
    runtime: &Arc<ChannelRuntime>,
    transport: Arc<dyn Transport>,
    downloader: Arc<dyn ImageDownloader>,
    transports: &mut HashMap<String, Arc<dyn Transport>>,
    workers: &mut Vec<ChannelWorker>,
) {
    let channel = transport.channel();
    runtime.report_status(ChannelStatus::new(channel));
    if let Err(error) = transport.connect().await {
        tracing::warn!(channel, error = %error, "通道首次连接失败（后台自愈重连中）");
    }
    transports.insert(channel.to_string(), transport.clone());
    workers.push(ChannelWorker::spawn(
        engine.clone(),
        transport,
        runtime.clone(),
        downloader,
        WorkerOptions::default(),
    ));
}

#[cfg(test)]
mod tests;
