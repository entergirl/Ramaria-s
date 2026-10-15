//! crates/ramaria-desktop/src/commands/import_continue.rs - 导入"继续处理" Tauri Command
//!
//! 设计特点:
//! - `continue_import`: 回补中断的导入（附件重扫 + L1 补跑 + 深度级联），
//!   后台任务执行、进度经 import-progress 事件转发
//! - 导出根防呆：只读白名单校验 + 导出根特征校验（含 resources/ 或导出 JSON），
//!   无效时同步拒绝并返回不含路径原值的用户文案
//! - 导入后处理互斥：post（图片理解 / 摘要处理）在途时拒绝并发续跑，避免抢行附件行
//! - 命令返回"任务已启动"回执；全部统计经 done 事件送达
//!   （含 resetCount / visionDone / visionFailed 附件回补字段）
//! - privacy 回归断言（文件底部）静态扫描本文件的日志与 instrument 写法

use crate::DesktopState;
use crate::events::{EVENT_IMPORT_PROGRESS, ImportProgressPayload};
use ramaria_service::ImportContinueRequest;
use ramaria_service::import::{is_post_active, is_valid_export_root};
use tauri::{AppHandle, Emitter, State};

use crate::commands::import_cmd::TauriImportProgressSink;

// =========================================================
// 入口校验辅助
// =========================================================

/// 无效导出根的用户提示（命令层校验失败返回，前端 toast 直接展示）。
const INVALID_EXPORT_ROOT_MESSAGE: &str = "所选目录不是聊天记录导出根：请选择包含 resources 文件夹（或导出 JSON）的那一层目录（通常为 QQChatExporter 下的 exports 目录）";

/// 导入后处理互斥判定：post 活跃时返回拒绝文案。
///
/// 参数:
/// - `post_active`: 服务层导入后处理活跃标志（`ramaria_service::import::is_post_active()`）。
///
/// 返回:
/// - `Some(文案)`: 拒绝继续处理；`None`: 放行。
fn post_busy_error(post_active: bool) -> Option<String> {
    post_active.then(|| "导入后的图片理解/摘要处理仍在进行，请稍候再试".to_string())
}

// =========================================================
// continue_import — 回补中断的导入
// =========================================================

/// 继续处理：回补中断的导入（附件重扫 + L1 补跑 + 深度级联）。
///
/// 参数:
/// - `export_root`: 导出根目录（缺省跳过附件回补，仅补 L1 与级联）。
/// - `reset_skipped`: 是否重置"有文件可定位"的存量跳过附件（缺省开启）。
///
/// 返回:
/// - `Ok(())`: 任务已启动（统计经 `import-progress` done 事件送达）。
/// - `Err(String)`: 入口校验失败（导入后处理在途 / 导出根非法等）。
///
/// 说明:
/// - 后台任务执行完整编排；完成 / 失败均发射一条 done 事件（失败时统计置零）。
#[tauri::command]
#[tracing::instrument(skip(state, app_handle, export_root))]
pub async fn continue_import(
    state: State<'_, DesktopState>,
    app_handle: AppHandle,
    export_root: Option<String>,
    reset_skipped: Option<bool>,
) -> Result<(), String> {
    // ---- 导入后处理互斥：图片理解 / 摘要处理在途时拒绝并发续跑（防抢行附件行） ----
    if let Some(message) = post_busy_error(is_post_active()) {
        return Err(message);
    }

    // ---- 导出根校验（只读白名单内的目录 + 导出根特征；拒绝文案不含路径原值） ----
    let export_roots = match export_root {
        Some(root) if !root.trim().is_empty() => {
            let validated = crate::path_guard::validate_read_dir_path(
                &root,
                &crate::path_guard::read_allowed_roots(&[]),
            )?;
            if !is_valid_export_root(&validated) {
                return Err(INVALID_EXPORT_ROOT_MESSAGE.to_string());
            }
            vec![validated]
        }
        _ => Vec::new(),
    };

    // 批量 LLM 请求间最小间隔（毫秒）：读当前生效配置的 `[thresholds].cluster_delay_ms`，
    // 读取失败时降级为 0（不阻塞继续处理）。
    let throttle_ms = state
        .engine
        .load_full_config()
        .await
        .map(|cfg| cfg.thresholds.cluster_delay_ms)
        .unwrap_or(0);

    let reset_skipped = reset_skipped.unwrap_or(true);
    // 图片缓存目录（数据库同级 `image_cache`；父目录缺失时降级不建立缓存）
    let cache_dir = crate::commands::attachment::image_cache_dir(&state.engine);
    tracing::info!(
        roots = export_roots.len(),
        reset_skipped,
        "导入继续处理：任务启动"
    );

    let engine = state.engine.clone();
    let handle = app_handle;
    tokio::spawn(async move {
        let sink = TauriImportProgressSink {
            app_handle: handle.clone(),
        };

        // 附件回补 → L1 幂等补跑 → 深度级联（服务层单一编排；进度由 sink 转发）
        let outcome = engine
            .continue_import(
                ImportContinueRequest {
                    export_roots,
                    reset_skipped,
                    throttle_ms,
                    l1_limit: 0,
                    cascade_deep: true,
                    cache_dir,
                },
                Some(&sink),
            )
            .await;

        match outcome {
            Ok(out) => {
                let summary = ramaria_service::import::done_summary(
                    &out.l1,
                    out.l2_triggered,
                    out.l3_triggered,
                    out.l1.l1_total,
                );
                let mut payload = ImportProgressPayload::done_with_stats(
                    summary.l1_success,
                    summary.l1_failed,
                    summary.l2_triggered,
                    summary.l3_triggered,
                    out.l1.l1_total,
                    &summary.message,
                );
                if let Some(stat) = &out.vision {
                    payload = payload.with_continue_stats(out.reset_count, stat.done, stat.failed);
                }
                if let Err(e) = handle.emit(EVENT_IMPORT_PROGRESS, &payload) {
                    tracing::warn!(error = %e, "发射 import-progress done 事件失败");
                }
            }
            Err(e) => {
                // 失败收束：done 事件（统计置零）保证前端状态机不悬挂
                tracing::error!(error = %e, "导入继续处理失败");
                let payload = ImportProgressPayload::done_with_stats(
                    0,
                    0,
                    false,
                    false,
                    0,
                    &crate::commands::service_error_message(&e, "继续处理失败"),
                );
                if let Err(emit_error) = handle.emit(EVENT_IMPORT_PROGRESS, &payload) {
                    tracing::warn!(error = %emit_error, "发射 import-progress done 事件失败");
                }
            }
        }
    });

    Ok(())
}

// =========================================================
// 隐私回归断言（日志脱敏 grep 审计，仅测试构建参与）
// =========================================================

#[cfg(test)]
mod privacy_regression_tests {
    //! 断言本文件的日志不直接落导出根路径原值。
    //!
    //! 说明:
    //! - 以源码文本静态断言，拦截"把导出根路径原值直接塞回 tracing 字段"的回归；
    //! - 通过 `env!("CARGO_MANIFEST_DIR")` 定位本文件，不依赖测试运行时目录；
    //! - 仅在 `cfg(test)` 参与，不影响生产构建。

    /// 读取本文件源码，并统一行尾为 LF。
    fn self_source() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/commands/import_continue.rs");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("无法读取本文件用于隐私断言: {path:?} 错误: {e}"))
            .replace("\r\n", "\n")
    }

    /// 禁止出现的"路径原值"日志形态。
    ///
    /// 字段名前缀与值表达式后缀分开存储、运行时拼接，避免完整字面量出现在
    /// 本文件源码中而干扰对"目标文件是否含路径原值日志"的静态判定。
    const FORBIDDEN_PATH_LOG_FRAGMENTS: &[(&str, &str)] =
        &[("= %real_path", ".display()"), ("= %file_path", ",")];

    /// continue_import 入口 span 必须 skip 的人参（否则 instrument 在 info 级
    /// 自动记录导出根路径参数）。
    const MUST_SKIP_ARGS: &[&str] = &["export_root", "app_handle", "state"];

    #[test]
    fn no_raw_path_in_logs() {
        let src = self_source();
        for &(prefix, suffix) in FORBIDDEN_PATH_LOG_FRAGMENTS {
            let forbidden = format!("{prefix}{suffix}");
            assert!(
                !src.contains(&forbidden),
                "检测到日志把绝对路径原值直接写入 tracing 字段（禁止形态 {forbidden:?}）——路径须经 redact_path_label 脱敏"
            );
        }
    }

    #[test]
    fn instrument_skips_path_args() {
        let src = self_source();
        // 以函数签名定位其正上方的 #[tracing::instrument(skip(...))] 属性块：
        // 在签名前 400 字符内回溯最近一次 "instrument(skip(" 的起点即该 skip 列表。
        let fn_start = src
            .find("pub async fn continue_import(")
            .expect("本文件必须包含 continue_import 定义（用于定位其 instrument skip 块）");
        // 回溯 400 字节并按需右移到字符边界（中文注释下避免切开多字节 UTF-8）
        let mut probe_start = fn_start.saturating_sub(400);
        while !src.is_char_boundary(probe_start) {
            probe_start += 1;
        }
        let skip_start = src[probe_start..fn_start]
            .rfind("instrument(skip(")
            .map(|off| probe_start + off)
            .unwrap_or_else(|| panic!("未找到 continue_import 的 instrument skip 块"));
        // 按字符取 skip 块文本（前 ~240 字符已覆盖整个 skip(...) 列表），避免字节切片切多字节
        let skip_block: String = src[skip_start..].chars().take(240).collect();
        for arg in MUST_SKIP_ARGS {
            assert!(
                skip_block.contains(arg),
                "instrument skip 块必须包含 {arg}，否则该参数会在 info 级被自动记录（泄露导出根路径）"
            );
        }
    }
}

// =========================================================
// 入口校验断言（互斥判定与导出根拒绝文案）
// =========================================================

#[cfg(test)]
mod entry_guard_tests {
    //! 断言"继续处理"入口的互斥判定与导出根防呆文案。
    //!
    //! 说明:
    //! - 判定以纯函数形式提供（`post_busy_error`），本模块覆盖拒绝 / 放行两分支；
    //! - 标志的真实置位 / 复位由服务层用例覆盖（`is_post_active` 的 RAII 生命周期）。

    use super::*;

    #[test]
    fn post_busy_error_rejects_while_post_active() {
        let message = post_busy_error(true).expect("post 活跃时应拒绝");
        assert_eq!(message, "导入后的图片理解/摘要处理仍在进行，请稍候再试");
        assert!(post_busy_error(false).is_none(), "post 非活跃时应放行");
    }

    #[test]
    fn invalid_export_root_message_guides_toward_exports_dir() {
        assert!(INVALID_EXPORT_ROOT_MESSAGE.contains("不是聊天记录导出根"));
        assert!(INVALID_EXPORT_ROOT_MESSAGE.contains("resources"));
        assert!(INVALID_EXPORT_ROOT_MESSAGE.contains("导出 JSON"));
        assert!(INVALID_EXPORT_ROOT_MESSAGE.contains("exports"));
    }
}
