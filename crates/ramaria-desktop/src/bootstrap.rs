//! crates/ramaria-desktop/src/bootstrap.rs - Ramaria 桌面启动引导模块
//!
//! 设计特点:
//! - 应用初始化全流程：数据目录 → 日志 → 服务层引擎装配（连接池 / migration / 配置 / LLM / 嵌入）
//!   → 配置双写链路 → 封存钩子链注册 → 生命周期拉起
//! - 与 Tauri 入口分离：`lib.rs` 只保留托管状态与命令注册，引导逻辑集中在本模块
//! - 日志与隐私: 默认只开 `info` 级（debug 含运行细节且日志随诊断包外发；排查用 `RUST_LOG` 开启）；
//!   日志不落绝对路径与用户原文（路径经 `path_guard::redact_path_label` 折叠为"文件名 + 短哈希"）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

// =========================================================
// 初始化日志
// =========================================================

/// 初始化 tracing 日志系统。
///
/// 说明:
/// - 始终输出到 stdout（控制台/终端）。
/// - 同时写入文件日志 `{log_dir}/ramaria.log`（每次启动覆盖旧日志）。
/// - 使用 `Mutex<File>` 保证线程安全，文件在初始化时立即创建，无后台线程延迟。
pub(crate) fn init_tracing(log_dir: &std::path::Path) {
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
        file = %crate::path_guard::redact_path_label(&log_file_path),
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
pub(crate) fn determine_data_dir() -> PathBuf {
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

/// 确保数据目录存在（含 logs / personas / image_cache 子目录）。
pub(crate) fn ensure_data_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    for sub in ["logs", "personas", "image_cache"] {
        std::fs::create_dir_all(path.join(sub))?;
    }
    Ok(())
}

// =========================================================
// 应用初始化
// =========================================================

/// 桌面运行时的装配产物。
///
/// 字段约定:
/// - `engine`: 服务层引擎（全部业务用例入口）；
/// - `lifecycle`: 服务层生命周期容器（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）。
pub(crate) struct DesktopRuntime {
    pub(crate) engine: Arc<ramaria_service::Engine>,
    pub(crate) lifecycle: Arc<ramaria_service::Lifecycle>,
}

/// 初始化桌面运行时。
///
/// 流程:
/// 1. 装配服务层引擎（连接池 + migration + 配置加载 + LLM / 嵌入恢复）；
/// 2. 配置双写链路：加载 config.toml + DB 两侧，一致性校验（以文件为准）并回写；
/// 3. 注册完整封存钩子链（行为 + 风格 + L2→L3 级联 + 知识抽取）；
/// 4. 拉起生命周期容器（空闲检查 + L2/L3 调度 + 启动期补扫全开）；
/// 5. 刷新启动状态。
///
/// 返回:
/// - 成功时返回桌面运行时（引擎 / 生命周期）；
/// - 失败时返回用户友好的错误描述（进程退出由调用方处理）。
///
/// 说明:
/// - 空闲检测与 L2/L3 调度统一由服务层生命周期负责，避免两套后台循环并存；
/// - 日志中的路径一律经 `path_guard::redact_path_label` 折叠（日志随诊断包外发）。
pub(crate) async fn init_runtime(data_dir: &Path) -> Result<DesktopRuntime, String> {
    let db_path = data_dir.join("ramaria.db");
    let config_path = data_dir.join("config.toml");

    // 确保数据目录存在
    ensure_data_dir(data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

    tracing::info!(
        db = %crate::path_guard::redact_path_label(&db_path),
        "初始化引擎"
    );

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
            file = %crate::path_guard::redact_path_label(&config_path),
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

    tracing::info!(
        state = %engine.current_state().as_str(),
        provider = %engine.llm().name(),
        "桌面运行时初始化完成"
    );

    Ok(DesktopRuntime { engine, lifecycle })
}
