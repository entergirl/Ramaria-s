//! crates/ramaria-channel/src/runtime/worker.rs - Ramaria 通道事件编排模块
//!
//! 设计特点:
//! - 单事件编排顺序固定：绑定解析 → 启用门禁 → 图片下载 → 组装 → 落库 →
//!   理解（后台并行）→ 响应判定 → 回复生成 → 节流 → 出站发送（含重试）
//! - 单步失败降级不阻塞：图片理解在后台任务、生成失败不发送、发送失败仅记日志
//!   （回复已落库）；重复事件零重复写入与响应
//! - 出站按会话节流；可重试错误按短退避重试至上限
//! - 日志脱敏：外部标识经 `mask_id`，不记消息内容

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ramaria_core::privacy::mask_id;
use ramaria_service::{ChannelIngestRequest, ChannelReplyRequest, Engine};

use super::assemble::{DownloadedImage, assemble_inbound, md5_hex};
use super::download::ImageDownloader;
use crate::runtime::ChannelRuntime;
use crate::transport::{
    InboundSegment, OutboundMessage, OutboundTarget, Transport, TransportEvent,
};

// =========================================================
// 编排参数
// =========================================================

/// 编排参数（节流 / 发送重试）。
///
/// 字段约定:
/// - `min_send_interval`: 同会话两次出站的最小间隔（节流）。
/// - `send_retry_delay`: 可重试发送失败后的重试等待。
/// - `send_max_attempts`: 发送总尝试上限（1 次初始 + 重试；仅可重试错误消耗）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOptions {
    pub min_send_interval: Duration,
    pub send_retry_delay: Duration,
    pub send_max_attempts: u32,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            min_send_interval: Duration::from_secs(1),
            send_retry_delay: Duration::from_secs(1),
            send_max_attempts: 2,
        }
    }
}

// =========================================================
// 编排任务
// =========================================================

/// 通道编排任务（事件循环 + 关停句柄）。
///
/// 职责:
/// - 从传输层拉取事件并逐条编排（写入 / 理解 / 生成 / 发送）；
/// - 关停时停止拉取并等待循环退出（幂等由调用方控制生命周期）。
pub struct ChannelWorker {
    handle: tokio::task::JoinHandle<()>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    runtime: Arc<ChannelRuntime>,
}

impl ChannelWorker {
    /// 启动事件循环（后台任务；宿主持有句柄控制生命周期）。
    pub fn spawn(
        engine: Arc<Engine>,
        transport: Arc<dyn Transport>,
        runtime: Arc<ChannelRuntime>,
        downloader: Arc<dyn ImageDownloader>,
        options: WorkerOptions,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(run_loop(
            engine,
            transport,
            downloader,
            options,
            shutdown_rx,
        ));
        Self {
            handle,
            shutdown_tx,
            runtime,
        }
    }

    /// 停止事件循环并等待任务退出。
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.handle.await;
    }

    /// 运行时句柄（状态查询 / 绑定管理用）。
    pub fn runtime(&self) -> &Arc<ChannelRuntime> {
        &self.runtime
    }
}

/// 事件循环：顺序拉取事件并逐条编排，关停信号可随时打断。
async fn run_loop(
    engine: Arc<Engine>,
    transport: Arc<dyn Transport>,
    downloader: Arc<dyn ImageDownloader>,
    options: WorkerOptions,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let channel = transport.channel();
    let throttle: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
    loop {
        let item = tokio::select! {
            _ = &mut shutdown_rx => break,
            item = transport.next_event() => item,
        };
        match item {
            Some(Ok(event)) => {
                handle_event(
                    &engine,
                    transport.as_ref(),
                    downloader.as_ref(),
                    &options,
                    &throttle,
                    event,
                )
                .await;
            }
            Some(Err(error)) => {
                tracing::warn!(channel, error = %error, "通道事件读取失败，继续处理后续事件");
            }
            None => break,
        }
    }
    tracing::info!(channel, "通道编排任务退出");
}

// =========================================================
// 单事件编排
// =========================================================

/// 编排一条入站事件（单步失败按各条口径中止或继续；不 panic、不上抛）。
///
/// 流程:
/// 1. 绑定解析（含自动建档）；未启用 → 整事件丢弃；
/// 2. 图片段逐个下载（失败按占位符继续）；
/// 3. 组装入站消息；正文为空 → 丢弃；
/// 4. 落库（重复事件跳过后续）；图片理解经后台任务并行；
/// 5. 响应判定未触发 → 结束；
/// 6. 回复生成（失败不发送）→ 出站节流 → 发送（可重试错误按上限重试）。
pub(crate) async fn handle_event(
    engine: &Arc<Engine>,
    transport: &dyn Transport,
    downloader: &dyn ImageDownloader,
    options: &WorkerOptions,
    throttle: &Mutex<HashMap<String, Instant>>,
    event: TransportEvent,
) {
    let channel = transport.channel();

    // ---- 1. 绑定解析 + 启用门禁 ----
    let binding = match engine
        .channel_resolve_binding(channel, &event.external_ref)
        .await
    {
        Ok(binding) => binding,
        Err(error) => {
            tracing::error!(
                channel,
                external_ref = %mask_id(&event.external_ref),
                error = %error,
                "通道绑定解析失败，丢弃事件"
            );
            return;
        }
    };
    if !binding.enabled {
        tracing::info!(
            channel,
            external_ref = %mask_id(&event.external_ref),
            "通道绑定未启用，丢弃事件"
        );
        return;
    }
    let persona = engine.channel_effective_persona_uid(&binding);

    // ---- 2. 图片段逐个下载（顺序与段序列一致） ----
    let mut images: Vec<Option<DownloadedImage>> = Vec::new();
    for segment in &event.segments {
        if let InboundSegment::Image { url, .. } = segment {
            match downloader.download(url).await {
                Ok(bytes) => {
                    let md5 = md5_hex(&bytes);
                    images.push(Some(DownloadedImage { md5, bytes }));
                }
                Err(error) => {
                    tracing::warn!(
                        channel,
                        error = %error,
                        "通道图片下载失败，按占位符继续"
                    );
                    images.push(None);
                }
            }
        }
    }

    // ---- 3. 组装 ----
    let assembled = assemble_inbound(&event, channel, &images);
    if assembled.message.text.trim().is_empty() {
        tracing::warn!(
            channel,
            external_ref = %mask_id(&event.external_ref),
            "组装后正文为空，丢弃事件"
        );
        return;
    }

    // ---- 4. 落库（幂等） ----
    let ingest = ChannelIngestRequest {
        channel: channel.to_string(),
        external_ref: event.external_ref.clone(),
        persona_uid: persona.clone(),
        self_id: event.self_id.clone(),
        message: assembled.message.clone(),
    };
    let outcome = match engine.channel_ingest(ingest).await {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(
                channel,
                external_ref = %mask_id(&event.external_ref),
                error = %error,
                "通道消息写入失败，丢弃事件（不重试）"
            );
            return;
        }
    };
    if !outcome.written {
        tracing::debug!(session_id = %outcome.session_id, "重复事件，跳过理解与响应");
        return;
    }

    // ---- 5. 图片理解（后台任务，与回复并行；失败不影响回复） ----
    if !outcome.images.is_empty() {
        let targets: Vec<(i64, DownloadedImage)> = outcome
            .images
            .iter()
            .map(|image| image.attachment_id)
            .zip(assembled.downloaded.iter().cloned())
            .collect();
        let engine = Arc::clone(engine);
        tokio::spawn(async move {
            for (attachment_id, image) in targets {
                match engine
                    .channel_understand_image(attachment_id, &image.md5, &image.bytes)
                    .await
                {
                    Ok(result) => {
                        tracing::debug!(attachment_id, result = ?result, "通道图片理解完成");
                    }
                    Err(error) => {
                        tracing::warn!(attachment_id, error = %error, "通道图片理解调用失败");
                    }
                }
            }
        });
    }

    // ---- 6. 响应判定 ----
    if !event.should_respond {
        return;
    }

    // ---- 7. 回复生成（失败不发送；不写半条） ----
    let reply_request = ChannelReplyRequest {
        session_id: outcome.session_id,
        persona_uid: persona,
        message: assembled.message.text.clone(),
    };
    let reply = match engine.channel_reply(reply_request).await {
        Ok(result) => result.reply,
        Err(error) => {
            tracing::error!(
                session_id = %outcome.session_id,
                error = %error,
                "通道回复生成失败，跳过发送"
            );
            return;
        }
    };
    if reply.trim().is_empty() {
        tracing::warn!(session_id = %outcome.session_id, "通道回复为空，跳过发送");
        return;
    }

    // ---- 8. 出站节流 + 发送（可重试错误按上限重试） ----
    enforce_send_interval(throttle, &event.external_ref, options.min_send_interval).await;
    let target = OutboundTarget {
        external_ref: event.external_ref.clone(),
    };
    let message = OutboundMessage::text(reply);
    send_with_retry(transport, &target, &message, options).await;
}

/// 出站节流：距上次发送不足间隔时等待补足，并记录本次发送时刻。
async fn enforce_send_interval(
    throttle: &Mutex<HashMap<String, Instant>>,
    external_ref: &str,
    min_interval: Duration,
) {
    let wait = {
        let mut guard = lock(throttle);
        let now = Instant::now();
        let wait = guard
            .get(external_ref)
            .and_then(|last| min_interval.checked_sub(now.duration_since(*last)))
            .unwrap_or_default();
        guard.insert(external_ref.to_string(), now + wait);
        wait
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

/// 出站发送：可重试错误按短退避重试至上限；最终失败仅记日志（回复已落库）。
async fn send_with_retry(
    transport: &dyn Transport,
    target: &OutboundTarget,
    message: &OutboundMessage,
    options: &WorkerOptions,
) {
    let max_attempts = options.send_max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match transport.send(target, message).await {
            Ok(()) => return,
            Err(error) => {
                let retryable = error.is_retryable();
                if retryable && attempt < max_attempts {
                    tracing::warn!(attempt, error = %error, "通道出站发送失败，准备重试");
                    attempt += 1;
                    tokio::time::sleep(options.send_retry_delay).await;
                    continue;
                }
                tracing::error!(
                    attempt,
                    retryable,
                    error = %error,
                    "通道出站发送失败（回复已落库，损失可接受）"
                );
                return;
            }
        }
    }
}

/// 获取互斥锁（中毒时取回内部数据，保持编排可用）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
