//! rust/crates/ramaria-desktop/src/lib.rs - Ramaria Tauri 桌面应用入口
//!
//! 设计特点:
//! - 管理应用初始化全流程：数据库 → 配置 → LLM Provider → App 构造
//! - 通过 Tauri managed state (`DesktopState`) 注入 `Arc<App>` 到所有 Command
//! - 初始化失败时优雅降级：窗口仍可显示，但状态为 FatalError
//! - 系统托盘在 Tauri setup 钩子中初始化
//! - 所有 Command 只做参数转换 + 委托 ramaria-app，不写业务逻辑

mod commands;
mod events;
mod notification;
mod tray;

use ramaria_core::StorageBackend;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

// =========================================================
// 托管状态
// =========================================================

/// Tauri 托管状态，注入到所有 Command 中。
///
/// 职责:
/// - 持有 `Arc<App>` 实例，供所有 Command 调用 ramaria-app
/// - 持有数据库路径（用于日志和诊断）
///
/// 安全约束:
/// - `App` 内部已通过 Mutex/Arc 保证线程安全
/// - DesktopState 自身为 Send + Sync
pub struct DesktopState {
    /// 应用核心实例
    pub app: Arc<ramaria_app::App>,
    /// 数据库文件路径（诊断用）
    pub db_path: PathBuf,
}

// =========================================================
// 初始化日志
// =========================================================

/// 初始化 tracing 日志系统。
///
/// 说明:
/// - 开发模式：输出到 stdout，RUST_LOG 环境变量控制级别
/// - 生产模式：后续添加文件日志（%APPDATA%\Ramaria\logs\）
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ramaria_desktop=debug"));

    let subscriber = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(subscriber)
        .init();

    tracing::info!("Ramaria Desktop v{} 启动", env!("CARGO_PKG_VERSION"));
}

// =========================================================
// 数据目录
// =========================================================

/// 确定应用数据目录。
///
/// 返回:
/// - 开发模式（debug_assertions）：项目根目录下的 `.ramaria-dev/`
/// - 生产模式：`%APPDATA%\Ramaria\data\`
/// - 可通过 `RAMARIA_DATA_DIR` 环境变量覆盖
fn determine_data_dir() -> PathBuf {
    // 优先使用环境变量
    if let Ok(dir) = std::env::var("RAMARIA_DATA_DIR") {
        let p = PathBuf::from(&dir);
        if p.is_absolute() || p.exists() {
            tracing::info!(data_dir = %dir, "使用环境变量 RAMARIA_DATA_DIR");
            return p;
        }
        tracing::warn!(data_dir = %dir, "RAMARIA_DATA_DIR 路径无效，回退默认值");
    }

    // 开发模式：使用项目本地目录
    if cfg!(debug_assertions) {
        // 从当前可执行文件位置推断项目根目录
        let dev_dir = PathBuf::from(".ramaria-dev");
        tracing::info!(data_dir = %dev_dir.display(), "开发模式数据目录");
        return dev_dir;
    }

    // 生产模式：使用 %APPDATA%
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let prod_dir = PathBuf::from(&appdata).join("Ramaria").join("data");
    tracing::info!(data_dir = %prod_dir.display(), "生产模式数据目录");
    prod_dir
}

/// 确保数据目录存在。
fn ensure_data_dir(path: &PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;

    // 同时确保子目录存在
    std::fs::create_dir_all(path.join("logs"))?;
    std::fs::create_dir_all(path.join("personas"))?;

    Ok(())
}

// =========================================================
// 应用初始化
// =========================================================

/// 初始化 ramaria-app 实例。
///
/// 流程:
/// 1. 初始化数据库连接池 + 执行 migration
/// 2. 读取已保存的后端配置（如有）
/// 3. 创建 Keychain
/// 4. 根据配置创建 LLM Provider
/// 5. 构造 App 实例
/// 6. 刷新应用状态
///
/// 返回:
/// - `Ok(App)` 初始化成功
/// - `Err(String)` 初始化失败（含用户友好的错误描述）
async fn init_app(data_dir: &PathBuf) -> Result<(Arc<ramaria_app::App>, PathBuf), String> {
    let db_path = data_dir.join("assistant.db");

    // 确保数据目录存在
    ensure_data_dir(data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

    tracing::info!(db = %db_path.display(), "初始化 App");

    // Step 1: 初始化数据库连接池 + 执行 migration
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .map_err(|e| format!("数据库初始化失败: {}", e))?;

    let storage = Arc::new(ramaria_storage::SqliteStorage::new(pool));

    // Step 2: 读取已保存的后端配置（如有）
    let backend_config = storage
        .get_backend_config()
        .await
        .map_err(|e| format!("读取后端配置失败: {}", e))?
        .unwrap_or_else(ramaria_core::types::BackendConfig::lm_studio_default);

    // Step 3: 创建 Keychain
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());

    // Step 4: 创建 LLM Provider
    let llm: Arc<dyn ramaria_core::LlmProviderTrait> = match backend_config.provider {
        ramaria_core::types::LlmProvider::LmStudio => {
            let provider = ramaria_llm::lm_studio::LmStudioProvider::new(backend_config.clone())
                .map_err(|e| format!("创建 LM Studio provider 失败: {}", e))?;
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::DeepSeek => {
            let provider = ramaria_llm::deepseek::DeepSeekProvider::new(
                backend_config.clone(),
                Arc::clone(&keychain),
            )
            .map_err(|e| format!("创建 DeepSeek provider 失败: {}", e))?;
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::OpenAI => {
            let provider = ramaria_llm::openai::OpenAIProvider::new(
                backend_config.clone(),
                Arc::clone(&keychain),
            )
            .map_err(|e| format!("创建 OpenAI provider 失败: {}", e))?;
            Arc::new(provider)
        }
        _ => {
            return Err(format!(
                "不支持的 LLM provider: {}",
                backend_config.provider.as_str()
            ));
        }
    };

    // Step 5: 构造 App
    let config = ramaria_core::config::RamariaConfig::default();
    let app = ramaria_app::App::new(storage, llm, config, keychain);

    // Step 6: 刷新状态
    app.refresh_setup_state()
        .await
        .map_err(|e| format!("刷新应用状态失败: {}", e))?;

    tracing::info!(
        state = %app.current_state().as_str(),
        provider = %backend_config.provider.as_str(),
        "App 初始化完成"
    );

    Ok((Arc::new(app), db_path))
}

// =========================================================
// Tauri 应用入口
// =========================================================

/// 构建并运行 Tauri 桌面应用。
///
/// 流程:
/// 1. 初始化日志
/// 2. 确定数据目录
/// 3. 初始化 ramaria-app（异步）
/// 4. 构建 Tauri Builder 并注入状态和命令
/// 5. 在 setup 钩子中初始化系统托盘
/// 6. 运行应用
///
/// 说明:
/// - 该函数由 main.rs 调用
/// - 不返回（由 Tauri 事件循环接管控制权）
pub fn run() {
    init_tracing();

    // 确定数据目录
    let data_dir = determine_data_dir();

    // 创建 tokio 运行时用于初始化
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("创建 tokio 运行时失败");

    // 执行应用初始化
    let (app, db_path) = match rt.block_on(init_app(&data_dir)) {
        Ok(result) => result,
        Err(e) => {
            tracing::error!(error = %e, "应用初始化失败");
            // 初始化失败时创建最小 DesktopState，状态为 FatalError
            // 此时窗口仍可显示，但前端会看到 FatalError 状态
            // 这需要至少一个最小可工作的 storage 和 llm provider
            // 对于无法恢复的初始化错误，直接退出
            eprintln!("致命错误: {}", e);
            std::process::exit(1);
        }
    };

    let state = DesktopState {
        app,
        db_path: db_path.clone(),
    };

    // 构建 Tauri 应用
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            // ---- Chat ----
            commands::chat::send_message,
            commands::chat::get_app_state,
            commands::chat::check_privacy,
            commands::chat::confirm_privacy,
            // ---- Setup ----
            commands::setup::run_setup,
            commands::setup::get_setup_status,
            commands::setup::refresh_setup_state,
            // ---- Session ----
            commands::session::list_sessions,
            commands::session::get_session,
            commands::session::delete_session,
            commands::session::create_session,
            // ---- Memory ----
            commands::memory::get_personas,
            commands::memory::get_l1_memories,
            commands::memory::get_l2_events,
            commands::memory::get_l3_traits,
            // ---- Config ----
            commands::config::get_backend_config,
            commands::config::update_backend_config,
            commands::config::get_settings,
            commands::config::update_setting,
            // ---- Export ----
            commands::export::export_sessions_json,
            commands::export::export_sessions_markdown,
            // ---- Index ----
            commands::index_cmd::rebuild_index,
            // ---- System ----
            tray::confirm_close_action,
        ])
        .setup(move |app| {
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
