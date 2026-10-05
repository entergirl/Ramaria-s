//! crates/ramaria-desktop/src/lib.rs - Ramaria Tauri 桌面应用入口
//!
//! 设计特点:
//! - 管理应用初始化全流程：服务层引擎装配（连接池 / migration / 配置 / LLM / 嵌入）
//!   → 配置双写链路 → 封存钩子链注册 → 生命周期拉起
//! - 通过 Tauri managed state (`DesktopState`) 注入引擎 / 生命周期到所有 Command
//! - 全部业务命令经服务层用例执行；桌面只保留事件桥、托盘与通知等宿主能力
//! - 系统托盘在 Tauri setup 钩子中初始化
//!
//! 日志与隐私:
//! - 默认只开 `info` 级：桌面 crate 的 debug 日志含运行细节，而日志文件会随
//!   诊断包外发；需要排查时用 `RUST_LOG` 显式开启 debug。
//! - 日志不落绝对路径与用户原文：路径一律经 `path_guard::redact_path_label`
//!   折叠为"文件名 + 短哈希"（由 `path_guard::privacy_audit_tests` 静态把关）。

mod commands;
mod events;
mod notification;
mod path_guard;
mod proactive;
mod tray;
mod webview;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Manager;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

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
    /// 评估面板的"用户显式授权目录"（原生目录对话框选择结果）。
    ///
    /// 语义:
    /// - 选择动作即授权：仅该目录（及其子目录）可被 `list_eval_files` /
    ///   `read_eval_result` 只读访问，用于收敛"任意路径读取"面；
    /// - 仅进程内有效（不持久化），重启后需重新选择。
    pub eval_allowed_dirs: std::sync::Mutex<Vec<PathBuf>>,
}

// =========================================================
// 初始化日志
// =========================================================

/// 初始化 tracing 日志系统。
///
/// 说明:
/// - 始终输出到 stdout（控制台/终端）。
/// - 同时写入文件日志 `{log_dir}/ramaria.log`（每次启动覆盖旧日志）。
/// - 使用 `Mutex<File>` 保证线程安全，文件在初始化时立即创建，无后台线程延迟。
fn init_tracing(log_dir: &std::path::Path) {
    // 默认只开 info 级：桌面 crate 的 debug 日志含词条/路径等运行细节，而日志
    // 文件会随诊断包外发；需要排查时用 RUST_LOG 显式开启（如 RUST_LOG=debug）。
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // 日志文件：立即创建（create + truncate），避免异步写延迟
    let log_file_path = log_dir.join("ramaria.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_file_path)
        .unwrap_or_else(|e| panic!("无法创建日志文件 '{}': {}", log_file_path.display(), e));

    let stdout_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    let file_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log_file));

    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    // tracing_subscriber 的 fmt layer 通过 MakeWriter 写入时会自动添加换行符。
    tracing::info!("Ramaria Desktop v{} 启动", env!("CARGO_PKG_VERSION"));
    tracing::info!(
        file = %path_guard::redact_path_label(&log_file_path),
        "日志文件已创建"
    );
}

// =========================================================
// 数据目录
// =========================================================

/// 确定应用数据目录（返回绝对路径）。
///
/// 返回:
/// - 开发模式（debug_assertions）：编译时 crate 目录下的 `.ramaria-dev/`
///   （使用 `CARGO_MANIFEST_DIR` 编译时常量，不依赖运行时 CWD）
/// - 生产模式：`%APPDATA%\Ramaria\data\`
/// - 可通过 `RAMARIA_DATA_DIR` 环境变量覆盖
fn determine_data_dir() -> PathBuf {
    // 优先使用环境变量
    if let Ok(dir) = std::env::var("RAMARIA_DATA_DIR") {
        let p = PathBuf::from(&dir);
        if p.is_absolute() {
            return p;
        }
        // 尝试相对于当前 exe 所在目录解析
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let abs = exe_dir.join(&p);
                if abs.exists() {
                    return abs;
                }
            }
        }
    }

    // 开发模式：使用编译时常量定位 crate 目录（绝对路径，不依赖 CWD）
    if cfg!(debug_assertions) {
        // CARGO_MANIFEST_DIR 在编译时即为绝对路径
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".ramaria-dev");
    }

    // 生产模式：使用 %APPDATA%
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(&appdata).join("Ramaria").join("data")
}

/// 确保数据目录存在。
fn ensure_data_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;

    // 同时确保子目录存在
    std::fs::create_dir_all(path.join("logs"))?;
    std::fs::create_dir_all(path.join("personas"))?;

    Ok(())
}

// =========================================================
// 应用初始化
// =========================================================

/// 索引待构建时完成一次构建并刷新状态。
///
/// 说明:
/// - 仅在应用状态为"索引待构建"（`Indexing`）时动作：缺索引的库在入口侧一次性收敛
///   （构建写回索引版本后状态机推进）；普通库不产生任何动作；
/// - 构建失败保持现状（记 warn），可由用户稍后手动重建。
pub(crate) async fn ensure_index_ready(engine: &ramaria_service::Engine) {
    if engine.current_state() != ramaria_core::types::AppState::Indexing {
        return;
    }
    match engine.ensure_index_loaded().await {
        Ok(_) => {
            if let Err(e) = engine.refresh_setup_state().await {
                tracing::warn!(error = %e, "索引构建后刷新应用状态失败（降级不阻塞）");
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "索引构建失败，保持待构建状态（可稍后手动重建）"
            );
        }
    }
}

/// 桌面运行时的装配产物。
///
/// 字段约定:
/// - `engine`: 服务层引擎（全部业务用例入口）；
/// - `lifecycle`: 服务层生命周期容器（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）。
struct DesktopRuntime {
    engine: Arc<ramaria_service::Engine>,
    lifecycle: Arc<ramaria_service::Lifecycle>,
}

/// 初始化桌面运行时。
///
/// 流程:
/// 1. 装配服务层引擎（连接池 + migration + 配置加载 + LLM / 嵌入恢复）；
/// 2. 配置双写链路：加载 config.toml + DB 两侧，一致性校验（以文件为准）并回写；
/// 3. 注册完整封存钩子链（行为 + 风格 + L2→L3 级联 + 知识抽取）；
/// 4. 拉起生命周期容器（空闲检查 + L2/L3 调度 + 启动期补扫全开）；
/// 5. 刷新启动状态；
/// 6. 索引未构建时在启动期完成一次构建并再次刷新状态。
///
/// 返回:
/// - 成功时返回桌面运行时（引擎 / 生命周期）；
/// - 失败时返回用户友好的错误描述（进程退出由调用方处理）。
///
/// 说明:
/// - 空闲检测与 L2/L3 调度统一由服务层生命周期负责，避免两套后台循环并存；
/// - 日志中的路径一律经 `path_guard::redact_path_label` 折叠（日志随诊断包外发）。
async fn init_runtime(data_dir: &Path) -> Result<DesktopRuntime, String> {
    let db_path = data_dir.join("assistant.db");
    let config_path = data_dir.join("config.toml");

    // 确保数据目录存在
    ensure_data_dir(data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

    tracing::info!(db = %path_guard::redact_path_label(&db_path), "初始化引擎");

    // Step 1: 装配服务层引擎（连接池 / migration / 配置 / LLM / 嵌入）
    let engine = Arc::new(
        ramaria_service::Engine::open_with(
            ramaria_service::EngineOptions::new(db_path.clone())
                .with_config_path(config_path.clone()),
        )
        .await
        .map_err(|e| format!("引擎装配失败: {}", e))?,
    );

    // Step 2: 配置双写链路：加载 config.toml + DB 两侧，一致性校验以文件为准
    let sync_outcome = engine
        .reload_config()
        .await
        .map_err(|e| format!("配置同步加载失败: {}", e))?;
    if !sync_outcome.file_existed {
        tracing::info!(
            file = %path_guard::redact_path_label(&config_path),
            "config.toml 不存在，已生成含全部默认值的模板"
        );
    }
    for err in &sync_outcome.file_parse_errors {
        tracing::warn!(error = %err, "config.toml 解析问题，已回退默认配置");
    }
    if sync_outcome.mismatches.is_empty() {
        tracing::info!("配置双写一致性校验通过（文件与 DB 一致）");
    } else {
        tracing::warn!(
            count = sync_outcome.mismatches.len(),
            "配置双写一致性校验发现不一致项，已按 config.toml 为准回写 DB"
        );
        for m in &sync_outcome.mismatches {
            // 仅记录键名，不打印配置值（避免泄露 base_url 等敏感细节）
            tracing::warn!(key = %m.key, "配置不一致（以文件为准，已回写 DB）");
        }
    }
    for err in &sync_outcome.db_write_failures {
        tracing::warn!(error = %err, "DB 侧配置回写失败（降级不阻塞）");
    }

    // Step 3: 注册封存钩子（桌面 = 完整链：行为 + 风格 + L2→L3 级联 + 知识抽取）
    engine.set_seal_hooks(ramaria_service::full_seal_hooks(&engine));

    // Step 4: 拉起生命周期（空闲检查 + L2/L3 调度 + 启动期补扫全开）
    let lifecycle = engine.start_lifecycle(ramaria_service::LifecycleOptions::desktop());

    // Step 5: 刷新启动状态
    engine
        .refresh_setup_state()
        .await
        .map_err(|e| format!("刷新应用状态失败: {}", e))?;

    // Step 6: 索引未构建的库在启动期完成一次构建并刷新状态
    ensure_index_ready(&engine).await;

    tracing::info!(
        state = %engine.current_state().as_str(),
        provider = %engine.llm().name(),
        "桌面运行时初始化完成"
    );

    Ok(DesktopRuntime { engine, lifecycle })
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
/// 7. 在 setup 钩子中初始化系统托盘
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
    //
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
        eval_allowed_dirs: std::sync::Mutex::new(Vec::new()),
    };

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
            // ---- Setup ----
            commands::setup::run_setup,
            commands::setup::refresh_setup_state,
            commands::setup::test_llm_connection,
            // ---- Embedding ----
            commands::setup::validate_embedding_model,
            commands::setup::save_embedding_model,
            commands::setup::get_embedding_model,
            commands::setup::get_degraded_reason,
            // ---- Session ----
            commands::session::list_sessions,
            commands::session::get_session,
            commands::session::create_session,
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
            commands::index_cmd::rebuild_index,
            // ---- Import ----
            commands::import_cmd::analyze_qq_chat,
            commands::import_cmd::import_qq_chat,
            commands::import_cmd::detect_qq_format,
            // ---- Persona ----
            commands::persona::list_personas_full,
            commands::persona::update_persona_info,
            commands::persona::refresh_persona,
            commands::persona::regenerate_import_pipeline,
            // ---- Proactive（主动消息名单）----
            commands::proactive_cmd::list_proactive_personas,
            commands::proactive_cmd::set_proactive_persona,
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

            // 初始化系统托盘
            if let Err(e) = tray::setup_tray(app.handle()) {
                tracing::error!(error = %e, "系统托盘初始化失败，应用继续运行");
                // 托盘失败不是致命错误，应用仍可运行
            }

            tracing::info!("Tauri 应用 setup 完成");
            Ok(())
        });

    // 运行应用
    builder
        .run(tauri::generate_context!())
        .expect("运行 Tauri 应用时发生错误");
}
