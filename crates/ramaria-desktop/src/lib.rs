//! crates/ramaria-desktop/src/lib.rs - Ramaria Tauri 桌面应用入口
//!
//! 设计特点:
//! - 管理应用初始化全流程：数据库 → 配置 → LLM Provider → Embedding 恢复 → App 构造
//! - 通过 Tauri managed state (`DesktopState`) 注入 `Arc<App>` 到所有 Command
//! - 初始化失败时优雅降级：窗口仍可显示，但状态为 FatalError
//! - 系统托盘在 Tauri setup 钩子中初始化
//! - 所有 Command 只做参数转换 + 委托 ramaria-app，不写业务逻辑

mod commands;
mod events;
mod notification;
mod path_guard;
mod tray;

use ramaria_core::StorageBackend;
use ramaria_core::traits::EmbeddingProvider;
use sqlx::SqlitePool;
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
    /// 数据库连接池（供导入器等直接访问 SQLite）
    pub pool: SqlitePool,
    /// 数据库文件路径（诊断用）
    pub db_path: PathBuf,
    /// config.toml 路径（配置双写同步服务用，v1.4）
    pub config_path: PathBuf,
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
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ramaria_desktop=debug"));

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

    // 注意：需手动添加换行，因为 tracing_subscriber 的 layer 不会自动在每条日志后加换行
    // 实际上 fmt::layer 会自动处理，但直接写 File 时需要确认。
    // tracing_subscriber 的 fmt layer 通过 MakeWriter 写入时会自动添加换行符。

    tracing::info!("Ramaria Desktop v{} 启动", env!("CARGO_PKG_VERSION"));
    tracing::info!(path = %log_file_path.display(), "日志文件已创建");
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
fn ensure_data_dir(path: &PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;

    // 同时确保子目录存在
    std::fs::create_dir_all(path.join("logs"))?;
    std::fs::create_dir_all(path.join("personas"))?;

    Ok(())
}

// =========================================================
// LLM Provider 构造
// =========================================================

/// 按 provider 类型构造 LLM Provider（三处调用收敛为单点实现）。
///
/// 参数:
/// - `provider`: LLM 后端类型（LmStudio / DeepSeek / OpenAI）
/// - `config`: 后端配置（含 base_url / model_id / capability）
/// - `keychain`: OS keychain 实例（线上 provider 构造时读取 API key）
/// - `cache`: LLM 响应精确缓存（v1.5 C；None = 不注入，行为回退 v1.4）
///
/// 返回:
/// - `Ok(Arc<dyn LlmProvider>)` 构造成功
/// - `Err(String)` 构造失败（用户友好的中文错误描述）
///
/// 说明:
/// - 供 `init_app` / `update_backend_config` / `run_setup` 共用，
///   保证三处 cache 注入条件与错误文案一致。
pub(crate) fn build_llm_provider(
    provider: ramaria_core::types::LlmProvider,
    config: &ramaria_core::types::BackendConfig,
    keychain: Arc<ramaria_llm::keychain::Keychain>,
    cache: Option<Arc<dyn ramaria_core::traits::LlmResponseCache>>,
) -> Result<Arc<dyn ramaria_core::traits::LlmProvider>, String> {
    let provider_arc: Arc<dyn ramaria_core::traits::LlmProvider> = match provider {
        ramaria_core::types::LlmProvider::LmStudio => {
            let instance = ramaria_llm::lm_studio::LmStudioProvider::new(config.clone())
                .map_err(|e| format!("创建 LM Studio provider 失败: {}", e))?;
            let instance = match &cache {
                Some(cache) => instance.with_cache(Arc::clone(cache)),
                None => instance,
            };
            Arc::new(instance)
        }
        ramaria_core::types::LlmProvider::DeepSeek => {
            let instance =
                ramaria_llm::deepseek::DeepSeekProvider::new(config.clone(), Arc::clone(&keychain))
                    .map_err(|e| format!("创建 DeepSeek provider 失败: {}", e))?;
            let instance = match &cache {
                Some(cache) => instance.with_cache(Arc::clone(cache)),
                None => instance,
            };
            Arc::new(instance)
        }
        ramaria_core::types::LlmProvider::OpenAI => {
            let instance =
                ramaria_llm::openai::OpenAIProvider::new(config.clone(), Arc::clone(&keychain))
                    .map_err(|e| format!("创建 OpenAI provider 失败: {}", e))?;
            let instance = match &cache {
                Some(cache) => instance.with_cache(Arc::clone(cache)),
                None => instance,
            };
            Arc::new(instance)
        }
        _ => {
            return Err(format!("不支持的 LLM provider: {}", provider.as_str()));
        }
    };
    Ok(provider_arc)
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
/// 5. 尝试恢复已保存的嵌入模型（如有）
/// 6. 配置双写同步：加载 config.toml + DB 两侧并做一致性校验
/// 7. 构造 App 实例
/// 8. 刷新应用状态
///
/// 返回:
/// - `Ok(App)` 初始化成功
/// - `Err(String)` 初始化失败（含用户友好的错误描述）
async fn init_app(
    data_dir: &PathBuf,
) -> Result<(Arc<ramaria_app::App>, SqlitePool, PathBuf, PathBuf), String> {
    let db_path = data_dir.join("assistant.db");
    let config_path = data_dir.join("config.toml");

    // 确保数据目录存在
    ensure_data_dir(data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

    tracing::info!(db = %db_path.display(), "初始化 App");

    // Step 1: 初始化数据库连接池 + 执行 migration
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .map_err(|e| format!("数据库初始化失败: {}", e))?;

    let storage = Arc::new(ramaria_storage::SqliteStorage::new(pool.clone()));

    // Step 2: 读取已保存的后端配置（如有）
    let backend_config = storage
        .get_backend_config()
        .await
        .map_err(|e| format!("读取后端配置失败: {}", e))?
        .unwrap_or_else(ramaria_core::types::BackendConfig::lm_studio_default);

    // Step 3: 创建 Keychain
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());

    // Step 5: 尝试恢复已保存的嵌入模型（复用 BackendConfig，与 base_url 一致）
    let embedding: Option<Arc<dyn EmbeddingProvider>> = {
        match &backend_config.embedding_model_path {
            Some(saved_path) if !saved_path.is_empty() => {
                let model_dir = std::path::Path::new(saved_path);
                if !model_dir.exists() {
                    tracing::warn!(
                        path = %saved_path,
                        "已保存的嵌入模型目录不存在，启动后将以降级模式运行"
                    );
                    None
                } else {
                    match ramaria_llm::embedding::native::create_native_provider(model_dir) {
                        Ok(provider) => {
                            let info = provider.model_info();
                            tracing::info!(
                                path = %saved_path,
                                model_id = %info.model_id,
                                dim = info.dimension,
                                "已恢复嵌入模型"
                            );
                            Some(Arc::new(provider) as Arc<dyn EmbeddingProvider>)
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %saved_path,
                                error = %e,
                                "加载已保存的嵌入模型失败，启动后将以降级模式运行"
                            );
                            None
                        }
                    }
                }
            }
            _ => {
                tracing::debug!("无已保存的嵌入模型，跳过恢复");
                None
            }
        }
    };

    // Step 6: 配置双写同步（v1.4）：加载 config.toml + DB 两侧，一致性校验以文件为准
    let storage_dyn: Arc<dyn StorageBackend> = storage.clone();
    let config_sync = ramaria_app::ConfigSyncService::new(storage_dyn, config_path.clone());
    let sync_outcome = config_sync
        .load()
        .await
        .map_err(|e| format!("配置同步加载失败: {}", e))?;
    if !sync_outcome.file_existed {
        tracing::info!(
            path = %config_path.display(),
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

    // Step 7: 构造 App（基于同步后的配置，填充实际路径）
    let mut config = sync_outcome.config;
    config.paths.data_dir = data_dir.to_string_lossy().to_string();
    config.paths.log_dir = data_dir.join("logs").to_string_lossy().to_string();
    config.paths.config_dir = data_dir.to_string_lossy().to_string();
    config.paths.vector_index_dir = data_dir.join("vectors").to_string_lossy().to_string();

    // Step 7.5: 创建 LLM Provider（基于同步后的配置注入精确缓存，v1.5 C）
    //
    // 缓存策略（[cache] 配置组）：
    // - `enabled=true`（默认）：创建 SqliteLlmCache 并注入 provider，
    //   重跑/重试/失败恢复场景命中缓存不重复花费 API 账单；
    // - `enabled=false`：不注入缓存，LLM 调用行为回退 v1.4。
    // - 缓存实例同时保存到 App（`set_llm_cache`），供热更新路径复用。
    let llm_cache: Option<Arc<dyn ramaria_core::traits::LlmResponseCache>> = if config.cache.enabled
    {
        Some(Arc::new(ramaria_storage::SqliteLlmCache::new(
            pool.clone(),
            config.cache.max_entries,
            config.cache.eviction,
        )))
    } else {
        None
    };
    let llm: Arc<dyn ramaria_core::LlmProviderTrait> = build_llm_provider(
        backend_config.provider,
        &backend_config,
        Arc::clone(&keychain),
        llm_cache.clone(),
    )?;
    let app = ramaria_app::App::new(storage, llm, embedding, config, keychain);
    // 保存缓存实例引用：后端热更新（update_llm）时复用同一缓存
    app.set_llm_cache(llm_cache);

    // Step 8: 刷新状态
    app.refresh_setup_state()
        .await
        .map_err(|e| format!("刷新应用状态失败: {}", e))?;

    // Step 9: 如果状态为 Ready，启动后台任务（空闲检测 + L2/L3 定时检查）
    if app.current_state() == ramaria_core::AppState::Ready {
        app.start_background_tasks();
        tracing::info!("后台任务已启动");
    }

    tracing::info!(
        state = %app.current_state().as_str(),
        provider = %backend_config.provider.as_str(),
        "App 初始化完成"
    );

    Ok((Arc::new(app), pool, db_path, config_path))
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
/// 4. 初始化 ramaria-app（异步）
/// 5. 构建 Tauri Builder 并注入状态和命令
/// 6. 在 setup 钩子中初始化系统托盘
/// 7. 运行应用
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
    tracing::info!(data_dir = %data_dir.display(), "数据目录已就绪");

    // 创建 tokio 运行时用于初始化
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("创建 tokio 运行时失败");

    // 执行应用初始化
    let (app, pool, db_path, config_path) = match rt.block_on(init_app(&data_dir)) {
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
        pool,
        db_path: db_path.clone(),
        config_path,
    };

    // 构建 Tauri 应用
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
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
            commands::setup::get_setup_status,
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
            commands::session::delete_session,
            commands::session::create_session,
            // ---- Memory ----
            commands::memory::get_personas,
            commands::memory::get_l1_memories,
            commands::memory::get_l2_events,
            commands::memory::get_l3_traits,
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
            // ---- Diagnostics ----
            commands::diagnostics::check_update,
            commands::diagnostics::get_version,
            commands::diagnostics::export_diagnostics,
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
