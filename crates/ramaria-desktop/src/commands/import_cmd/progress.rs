//! crates/ramaria-desktop/src/commands/import_cmd/progress.rs - Ramaria 导入进度事件转发
//!
//! 设计特点:
//! - 服务层回调 → Tauri `import-progress` 事件，字段与前端契约一致
//! - 图片理解完成收尾事件的可选统计随后透出
//! - 事件发射失败只记 warn，不中断导入批次

use crate::events::{EVENT_IMPORT_PROGRESS, ImportProgressPayload};
use ramaria_service::{ImportDoneSummary, ImportL1Progress, ImportProgressSink};
use tauri::{AppHandle, Emitter};

// =========================================================
// 导入进度转发（服务层回调 → Tauri 事件）
// =========================================================

/// 导入进度回调：把服务层阶段进度转发为 Tauri `import-progress` 事件。
///
/// 说明:
/// - 事件负载字段与前端契约一致（phase / current / total / message /
///   各阶段预计总量 / ETA / done 统计）；
/// - 事件发射失败只记 warn，不中断导入批次；
/// - 供导入与"继续处理"两条命令共用（后者单独发 done 事件）。
pub(crate) struct TauriImportProgressSink {
    pub(crate) app_handle: AppHandle,
}

impl ImportProgressSink for TauriImportProgressSink {
    fn on_l1_progress(&self, p: &ImportL1Progress) {
        let mut payload = ImportProgressPayload::new(p.phase, p.current, p.total, &p.message)
            .with_estimates(p.l1_total, p.l2_total, p.l3_total, p.eta_seconds);
        // 图片理解完成收尾事件：done / failed / skipped 计数随可选字段透出
        if let (Some(done), Some(failed), Some(skipped)) =
            (p.vision_done, p.vision_failed, p.vision_skipped)
        {
            payload = payload.with_vision_stats(done, failed, skipped);
        }
        if let Err(e) = self.app_handle.emit(EVENT_IMPORT_PROGRESS, &payload) {
            tracing::warn!(error = %e, "发射 import-progress 事件失败");
        }
    }

    fn on_done(&self, s: &ImportDoneSummary) {
        let payload = ImportProgressPayload::done_with_stats(
            s.l1_success,
            s.l1_failed,
            s.l2_triggered,
            s.l3_triggered,
            s.total_sessions,
            &s.message,
        );
        if let Err(e) = self.app_handle.emit(EVENT_IMPORT_PROGRESS, &payload) {
            tracing::warn!(error = %e, "发射 import-progress done 事件失败");
        }
    }
}
