//! crates/ramaria-desktop/src/lib.rs - Ramaria Tauri 桌面应用入口
//!
//! 设计特点:
//! - 装配引导（日志 / 数据目录 / 引擎与生命周期）在 `bootstrap` 模块，本文件只保留
//!   托管状态与命令注册
//! - 通过 Tauri managed state (`DesktopState`) 注入引擎 / 生命周期到所有 Command
//! - 全部业务命令经服务层用例执行；桌面只保留事件桥、托盘与通知等宿主能力
//! - 系统托盘在 Tauri setup 钩子中初始化
//!
//! 日志与隐私:
//! - 默认只开 `info` 级：桌面 crate 的 debug 日志含运行细节，而日志文件会随
//!   诊断包外发；需要排查时用 `RUST_LOG` 显式开启 debug。
//! - 日志不落绝对路径与用户原文：路径一律经 `path_guard::redact_path_label`
//!   折叠为"文件名 + 短哈希"（由 `path_guard::privacy_audit_tests` 静态把关）。

mod bootstrap;
mod channels;
mod commands;
mod events;
mod notification;
mod path_guard;
mod proactive;
mod tray;
mod tray_badge;
mod webview;

use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;

use bootstrap::{determine_data_dir, ensure_data_dir, init_runtime, init_tracing};

// =========================================================
// 托管状态
// =========================================================

/// Tauri 托管状态，注入到所有 Command 中。
///
/// 职责:
/// - 持有服务层引擎：全部业务命令的统一用例入口
/// - 持有生命周期容器：活跃会话指针、空闲自动保存与后台调度
/// - 持有评估面板的"用户显式授权目录"
///
/// 安全约束:
/// - 各实例内部已通过 Mutex/Arc 保证线程安全
/// - DesktopState 自身为 Send + Sync
pub struct DesktopState {
    /// 服务层引擎（对话 / 会话 / 配置 / 模型等用例入口）
    pub engine: Arc<ramaria_service::Engine>,
    /// 服务层生命周期容器（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）
    pub lifecycle: Arc<ramaria_service::Lifecycle>,
    /// 社交通道运行时管理器（装配 / 启停 / 状态列表；crate 内命令访问）
    pub(crate) channel_manager: channels::ChannelManager,
    /// 评估面板的"用户显式授权目录"（原生目录对话框选择结果）。
    ///
    /// 语义:
    /// - 选择动作即授权：仅该目录（及其子目录）可被 `list_eval_files` /
    ///   `read_eval_result` 只读访问，用于收敛"任意路径读取"面；
    /// - 仅进程内有效（不持久化），重启后需重新选择。
    pub eval_allowed_dirs: std::sync::Mutex<Vec<PathBuf>>,
}

// =========================================================
// Tauri 应用入口
// =========================================================

/// 构建并运行 Tauri 桌面应用。
///
/// 流程:
/// 1. 确定数据目录
/// 2. 确保目录存在（含 logs/ 子目录）
/// 3. 初始化日志（stdout + 文件）
/// 4. 清理 WebView2 远程调试端口注入（release）
/// 5. 初始化桌面运行时（引擎 / 生命周期，异步）
/// 6. 构建 Tauri Builder 并注入状态和命令
/// 7. 在 setup 钩子中初始化系统托盘并刷新未读徽标
/// 8. 运行应用
///
/// 说明:
/// - 该函数由 main.rs 调用
/// - 不返回（由 Tauri 事件循环接管控制权）
pub fn run() {
    // Step 1: 确定数据目录
    let data_dir = determine_data_dir();

    // Step 2: 确保数据目录存在（含 logs/ 等子目录）
    // 必须在 init_tracing 之前，因为文件日志写入 logs/
    if let Err(e) = ensure_data_dir(&data_dir) {
        eprintln!("致命错误: 无法创建数据目录 '{}': {}", data_dir.display(), e);
        std::process::exit(1);
    }

    // Step 3: 初始化日志（输出到 stdout + 文件，文件立即创建）
    init_tracing(&data_dir.join("logs"));
    tracing::info!(
        dir = %path_guard::redact_path_label(&data_dir),
        "数据目录已就绪"
    );

    // Step 4: release 构建清理 WebView2 远程调试端口注入（须在任何线程创建之前）
    #[cfg(not(debug_assertions))]
    webview::sanitize_webview2_debug_args();

    // 创建 tokio 运行时用于初始化
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("创建 tokio 运行时失败");

    // 执行应用初始化
    // 初始化失败属不可恢复（无 storage / 无 LLM provider 时所有 Command 均不可用），
    // 故记录错误后直接退出；用户可从日志与 stderr 获取失败原因。
    let runtime = match rt.block_on(init_runtime(&data_dir)) {
        Ok(result) => result,
        Err(e) => {
            tracing::error!(error = %e, "应用初始化失败，进程退出");
            eprintln!("致命错误: {}", e);
            std::process::exit(1);
        }
    };

    let state = DesktopState {
        engine: runtime.engine,
        lifecycle: runtime.lifecycle,
        channel_manager: channels::ChannelManager::new(),
        eval_allowed_dirs: std::sync::Mutex::new(Vec::new()),
    };
    // 退出路径需要独立持有管理器句柄（state 随构建移入 Tauri 管理器）
    let channel_manager = state.channel_manager.clone();

    // 构建 Tauri 应用
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        // 桌面通知插件：chat 回复完成且主窗口不可见时发送系统通知
        // （notification.rs 经 NotificationExt 调用，未注册会在运行期 panic）
        .plugin(tauri_plugin_notification::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            // ---- Chat ----
            commands::chat::send_message,
            commands::chat::save_current_session,
            commands::chat::generate_l1,
            commands::chat::get_app_state,
            commands::chat::check_privacy,
            commands::chat::confirm_privacy,
            commands::chat::recall_message,
            commands::chat::send_local_image,
            commands::image_source::get_image_source_dir,
            commands::image_source::set_image_source_dir,
            // ---- Setup ----
            commands::setup::run_setup,
            commands::setup::refresh_setup_state,
            commands::setup::test_llm_connection,
            // ---- Vision ----
            commands::vision::set_vision_support,
            // ---- Embedding ----
            commands::setup::validate_embedding_model,
            commands::model_download::download_embedding_model,
            commands::setup::save_embedding_model,
            commands::setup::get_embedding_model,
            commands::setup::get_degraded_reason,
            // ---- Session ----
            commands::session::list_sessions,
            commands::session::get_session,
            commands::session::create_session,
            commands::session::mark_session_read,
            // ---- Memory ----
            commands::memory::get_personas,
            commands::memory::get_l1_memories,
            commands::memory::get_l2_events,
            commands::memory::trigger_memory_pipeline,
            commands::memory::get_personality_profile,
            commands::memory::get_trait_evidence,
            commands::memory::get_profile_status,
            commands::memory::get_facts,
            // ---- Config ----
            commands::config::get_backend_config,
            commands::config::update_backend_config,
            commands::config::get_settings,
            commands::config::update_setting,
            commands::config::get_full_config,
            commands::config::update_full_config,
            // ---- MCP 接入（设置页面板）----
            commands::mcp::get_mcp_info,
            // ---- Export ----
            commands::export::export_sessions_json,
            commands::export::export_sessions_markdown,
            // ---- Index ----
            commands::index_cmd::get_index_status,
            commands::index_cmd::rebuild_index,
            // ---- Import ----
            commands::import_cmd::analyze_qq_chat,
            commands::import_cmd::import_qq_chat,
            commands::import_cmd::detect_qq_format,
            commands::import_continue::continue_import,
            commands::import_status::get_import_status,
            // ---- Attachment（会话浏览图片）----
            commands::attachment::read_attachment_image,
            commands::attachment::clear_image_cache,
            // ---- Persona ----
            commands::persona::list_personas_full,
            commands::persona::update_persona_info,
            commands::persona::refresh_persona,
            commands::persona::regenerate_import_pipeline,
            commands::persona::preview_persona_prompt,
            commands::preset::preview_persona_import,
            commands::preset::import_persona_preset,
            commands::preset::persona_export,
            // ---- Proactive（主动消息名单）----
            commands::proactive_cmd::list_proactive_personas,
            commands::proactive_cmd::set_proactive_persona,
            // ---- Channel（社交平台接入）----
            commands::channel::get_channel_status,
            commands::channel::start_channel,
            commands::channel::stop_channel,
            commands::channel::list_channel_bindings,
            commands::channel::set_channel_binding,
            // ---- Rules（行为规则管理）----
            commands::rules::list_rules,
            commands::rules::set_rule_enabled,
            commands::rules::edit_rule,
            commands::rules::rule_evidence,
            // ---- Keywords（关键词池只读 + 别名）----
            commands::keywords::list_keywords,
            commands::keywords::list_pending_aliases,
            commands::keywords::resolve_alias,
            // ---- Style（说话风格统计只读）----
            commands::style::get_style_stats,
            // ---- Evaluation（评估调试只读面板）----
            commands::evaluation::pick_eval_dir,
            commands::evaluation::list_eval_files,
            commands::evaluation::read_eval_result,
            // ---- Diagnostics ----
            commands::diagnostics::check_update,
            commands::diagnostics::get_version,
            commands::diagnostics::export_diagnostics,
            // ---- System ----
            commands::dialog::save_file_dialog,
            commands::dialog::open_file_dialog,
            commands::dialog::open_directory_dialog,
            tray::confirm_close_action,
        ])
        .setup(move |app| {
            // 注册主动消息投递接收端：emit 事件 + 系统通知（点击聚焦并定位会话）。
            // 须在调度循环首次投递前就位；未注册时服务层调度静默丢弃。
            let engine = app.state::<DesktopState>().engine.clone();
            engine.set_proactive_sink(Arc::new(proactive::TauriProactiveSink::new(
                app.handle().clone(),
            )));
            tracing::info!("主动消息投递接收端已注册");

            // 注册索引构建进度回调，并在后台启动首次构建（不阻塞 setup）
            let index_handle = app.handle().clone();
            engine.set_index_progress_hook(Arc::new(move |progress| {
                crate::events::emit_index_progress(&index_handle, progress);
            }));
            let build_engine = engine.clone();
            tauri::async_runtime::spawn(async move {
                build_engine.start_index_build();
            });

            // 初始化系统托盘
            if let Err(e) = tray::setup_tray(app.handle()) {
                tracing::error!(error = %e, "系统托盘初始化失败，应用继续运行");
                // 托盘失败不是致命错误，应用仍可运行
            }

            // 启动时刷新一次托盘未读徽标（查询失败仅告警，不阻塞 setup）
            tray::spawn_tray_refresh(app.handle().clone());

            // 启动通道运行时（已启用且参数完整时装配；异步执行，不阻塞 setup）
            channels::spawn_start(
                app.state::<DesktopState>().channel_manager.clone(),
                app.state::<DesktopState>().engine.clone(),
                app.handle().clone(),
            );

            tracing::info!("Tauri 应用 setup 完成");
            Ok(())
        });

    // 运行应用
    let app = builder
        .build(tauri::generate_context!())
        .expect("构建 Tauri 应用失败");
    app.run(move |_app_handle, event| {
        // 退出前停止通道运行时（限时等待在途编排退出；失败仅忽略，进程即将结束）
        if let tauri::RunEvent::Exit = event {
            let _ = tauri::async_runtime::block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(5), channel_manager.stop())
                    .await
            });
        }
    });
}
