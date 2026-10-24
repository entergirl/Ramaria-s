//! crates/ramaria-service/src/model/download.rs - 嵌入模型下载编排与进度负载
//!
//! 设计特点:
//! - 开始时推进应用状态到 DownloadingModel，结束（成功或失败）恢复原状态
//! - 进度按固定间隔节流后交给接收方，开始 / 完成 / 失败必上报
//! - 同一时刻只允许一个下载：应用状态已是 DownloadingModel 时直接拒绝
//! - 负载字段为 camelCase，状态为小写蛇形，供桌面事件与前端横幅共用

use std::path::Path;
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::AppState;
use ramaria_llm::model_manager::ProgressCallback;
use serde::Serialize;

use super::download_model;
use crate::engine::Engine;

// =========================================================
// 下载进度事件负载
// =========================================================

/// 模型下载进度上报的最小间隔（毫秒）。
const DOWNLOAD_PROGRESS_INTERVAL_MS: u64 = 100;

/// 模型下载状态。
///
/// 序列化:
/// - 小写蛇形字符串：`started` / `progress` / `completed` / `failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDownloadStatus {
    /// 下载开始（总量未知）
    Started,
    /// 下载进行中
    Progress,
    /// 下载完成
    Completed,
    /// 下载失败（状态已恢复，可重试）
    Failed,
}

/// 模型下载进度负载（事件与前端横幅的共享结构）。
///
/// 字段约定:
/// - `status`: 当前阶段；
/// - `downloaded` / `total`: 已下载与总字节数（总量未知时为 0）；
/// - `percent`: 0~100 的完成百分比（总量未知时为 0，完成为 100）；
/// - `elapsed_ms`: 自下载开始的毫秒数；
/// - `model`: 模型标识（多下载并存时用于区分）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadProgress {
    pub status: ModelDownloadStatus,
    pub downloaded: u64,
    pub total: u64,
    pub percent: f64,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// 下载进度接收方（桌面事件桥 / 测试收集器）。
pub type ModelDownloadSink = Arc<dyn Fn(ModelDownloadProgress) + Send + Sync>;

/// 计算完成百分比（总量未知返回 0，结果保留两位小数）。
pub(super) fn percent_of(downloaded: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let percent = (downloaded as f64 / total as f64) * 100.0;
    (percent.clamp(0.0, 100.0) * 100.0).round() / 100.0
}

// =========================================================
// 下载编排（状态推进 + 进度上报）
// =========================================================

/// 下载嵌入模型并上报进度。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `model_id`: 模型标识（须在预置清单内）；
/// - `root`: 模型根目录覆盖（`None` 用平台默认）；
/// - `sink`: 进度接收方（`None` 只推进状态，不上报）。
///
/// 返回:
/// - 下载成功返回 `Ok(())`；失败返回模型管理器原始错误。
///
/// 说明:
/// - 开始时把应用状态推进到 `DownloadingModel`，结束后恢复原状态
///   （失败不阻塞主流程，用户可重试）；
/// - 同一时刻只允许一个下载：当前状态已是 `DownloadingModel` 时直接拒绝；
/// - 进度上报按 [`DOWNLOAD_PROGRESS_INTERVAL_MS`] 节流，完成 / 失败必上报；
/// - 进度接收方不参与错误语义：上报失败只记日志。
pub async fn download_model_with_progress(
    engine: &Engine,
    model_id: &str,
    root: Option<&Path>,
    sink: Option<ModelDownloadSink>,
) -> RamariaResult<()> {
    let previous = engine.current_state();
    if previous == AppState::DownloadingModel {
        return Err(RamariaError::validation(
            "模型下载已在进行中，请等待当前下载完成",
        ));
    }
    engine.set_state(AppState::DownloadingModel);

    let start = std::time::Instant::now();
    let downloaded = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let total = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let last_emit_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let model_name = model_id.to_string();

    // ---- 开始：总量未知 ----
    if let Some(sink) = &sink {
        sink(ModelDownloadProgress {
            status: ModelDownloadStatus::Started,
            downloaded: 0,
            total: 0,
            percent: 0.0,
            elapsed_ms: 0,
            model: Some(model_name.clone()),
        });
    }

    // ---- 进度：节流上报（总量 / 已下载取自管理器回调） ----
    let progress_sink = sink.clone();
    let progress_model = model_name.clone();
    let progress_downloaded = Arc::clone(&downloaded);
    let progress_total = Arc::clone(&total);
    let progress_last_emit = Arc::clone(&last_emit_ms);
    let callback: ProgressCallback = Arc::new(
        move |progress: ramaria_llm::model_manager::DownloadProgress| {
            use std::sync::atomic::Ordering;

            progress_downloaded.store(progress.downloaded_bytes, Ordering::Relaxed);
            progress_total.store(progress.total_bytes, Ordering::Relaxed);
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let previous_ms = progress_last_emit.load(Ordering::Relaxed);
            if elapsed_ms.saturating_sub(previous_ms) < DOWNLOAD_PROGRESS_INTERVAL_MS {
                return;
            }
            progress_last_emit.store(elapsed_ms, Ordering::Relaxed);
            if let Some(sink) = &progress_sink {
                sink(ModelDownloadProgress {
                    status: ModelDownloadStatus::Progress,
                    downloaded: progress.downloaded_bytes,
                    total: progress.total_bytes,
                    percent: percent_of(progress.downloaded_bytes, progress.total_bytes),
                    elapsed_ms,
                    model: Some(progress_model.clone()),
                });
            }
        },
    );

    let result = download_model(model_id, root, Some(callback)).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (final_downloaded, final_total) = {
        use std::sync::atomic::Ordering;
        (
            downloaded.load(Ordering::Relaxed),
            total.load(Ordering::Relaxed),
        )
    };

    match &result {
        Ok(()) => {
            if let Some(sink) = &sink {
                sink(ModelDownloadProgress {
                    status: ModelDownloadStatus::Completed,
                    downloaded: if final_total > 0 {
                        final_total
                    } else {
                        final_downloaded
                    },
                    total: final_total,
                    percent: 100.0,
                    elapsed_ms,
                    model: Some(model_name.clone()),
                });
            }
            tracing::info!(model_id, elapsed_ms, "嵌入模型下载完成");
        }
        Err(e) => {
            if let Some(sink) = &sink {
                sink(ModelDownloadProgress {
                    status: ModelDownloadStatus::Failed,
                    downloaded: final_downloaded,
                    total: final_total,
                    percent: percent_of(final_downloaded, final_total),
                    elapsed_ms,
                    model: Some(model_name.clone()),
                });
            }
            tracing::warn!(model_id, elapsed_ms, error = %e, "嵌入模型下载失败（状态已恢复）");
        }
    }

    engine.set_state(previous);
    result
}
