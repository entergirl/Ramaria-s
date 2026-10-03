//! crates/ramaria-mcp/src/host.rs - MCP 服务宿主（stdio 生命周期）
//!
//! 设计特点:
//! - 宿主职责：装配服务层引擎 → 注入召回策略与默认封存钩子 → 拉起空闲检查 → 以 stdio 提供服务
//! - 策略注入：`[mcp]` 的原文开关与人格白名单转成 `RecallPolicy` 进入服务层强制执行
//! - 钩子注入：注册与传输无关的默认封存钩子（行为 / 风格 / L2 触发，不依赖 app）
//! - 空闲检查：进程内按 `[session].idle_check_interval_seconds` 自动封存超时会话
//!   （与桌面同一空闲口径；`[mcp].enabled` / `allow_seal` 任一关闭则不拉起，见 `serve_stdio`）
//! - 日志纪律：日志初始化到 stderr（stdout 只允许 MCP 协议消息）
//! - 退出路径：传输关闭（客户端退出）或 Ctrl+C 均可收敛退出，退出前优雅关停空闲检查
//! - 降级纪律：嵌入模型缺失时向量通道降级（BM25 + 关键词镜像继续工作），不阻塞启动

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{Engine, EngineOptions, RecallPolicy, default_seal_hooks};
use rmcp::ServiceExt;

use crate::server::RamariaMcpServer;

/// MCP 宿主启动选项。
///
/// 字段约定:
/// - `db_path`: 数据库文件路径（不存在时自动创建并执行 migration）。
/// - `config_path`: 配置文件路径；缺省取数据库同目录 `config.toml`。
#[derive(Debug, Clone)]
pub struct McpHostOptions {
    pub db_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

impl McpHostOptions {
    /// 以数据库路径创建选项（配置路径缺省）。
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            config_path: None,
        }
    }

    /// 指定配置文件路径（链式调用）。
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }
}

/// 初始化日志（全部输出到 stderr）。
///
/// 说明:
/// - 已初始化时静默跳过（`try_init` 失败不报错），便于被 CLI 等宿主复用；
/// - 级别由 `RUST_LOG` 控制（缺省 info），便于客户端日志侧排查。
pub fn init_stderr_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init()
        .ok();
}

/// 以 stdio 启动 MCP 服务端（阻塞至传输关闭或收到 Ctrl+C）。
///
/// 流程:
/// 1. 装配服务层引擎（config.toml 为权威源，嵌入缺失降级）；
/// 2. 注入召回策略（原文开关 + 人格白名单）与默认封存钩子；
/// 3. 按门禁拉起进程内空闲检查（超时会话生成 L1）；
/// 4. 启动 stdio 服务端，等待退出信号后优雅关停后台任务。
///
/// 参数:
/// - `options`: 数据库与配置文件路径。
///
/// 返回:
/// - `Ok(())`: 服务端正常退出（传输关闭或收到退出信号）。
/// - `Err(..)`: 引擎装配失败（库不可用 / provider 不合法）或初始化握手失败。
pub async fn serve_stdio(options: McpHostOptions) -> RamariaResult<()> {
    init_stderr_logging();

    // ---- 1. 服务层引擎 ----
    let db_path = options.db_path.clone();
    let engine = Arc::new(
        Engine::open_with(EngineOptions {
            db_path: options.db_path,
            config_path: options.config_path,
        })
        .await?,
    );
    let mcp_config = engine.config().mcp.clone();
    if !mcp_config.enabled {
        tracing::warn!(
            "MCP 接入未开启（[mcp].enabled = false）：服务端仍可挂载，但所有工具将返回可操作错误，请在桌面「设置 → MCP 接入」中开启"
        );
    }
    if let Some(warning) = engine.config_warning() {
        tracing::warn!(
            warning = %warning,
            "config.toml 解析失败已回退默认配置（[mcp] 门禁可能非用户本意）"
        );
    }

    // ---- 2. 召回策略：原文开关与人格白名单进入服务层强制执行 ----
    engine.set_recall_policy(
        RecallPolicy::default()
            .with_allow_raw_text(mcp_config.allow_raw_text)
            .with_allowed_personas(mcp_config.allowed_personas.clone()),
    );

    // ---- 2b. 封存许可：写侧治理开关进入服务层强制门禁（D-V21-009） ----
    // 关闭时：封存用例 / 惰性体检 / 空闲检查全部跳过（含摘要补扫），只保留写入
    engine.set_seal_allowed(mcp_config.allow_seal);

    // ---- 3. 默认封存钩子（行为 / 风格 / L2 触发；MCP 进程不依赖 app） ----
    engine.set_seal_hooks(default_seal_hooks(&engine));

    // ---- 4. 进程内空闲检查（超时会话生成 L1，与桌面同一空闲口径）----
    // 门禁:
    // - `enabled = false`: 用户未在面板开启 MCP 接入，宿主不做任何后台加工；
    // - `allow_seal = false`: 明确禁止 MCP 侧触发封存与摘要生成（消耗 LLM 并改变记忆状态），
    //   此时只保留客户端驱动的写入，超时会话交由桌面或下次调用时的惰性体检处理。
    let idle_loop = if mcp_config.enabled && mcp_config.allow_seal {
        Some(Arc::clone(&engine).spawn_idle_loop())
    } else {
        tracing::info!(
            enabled = mcp_config.enabled,
            allow_seal = mcp_config.allow_seal,
            "MCP 侧空闲封存未启用（[mcp] 门禁）：超时会话改由桌面宿主或下次调用时的惰性体检封存"
        );
        None
    };

    // ---- 5. stdio 服务端 ----
    let server = RamariaMcpServer::new(Arc::clone(&engine), mcp_config);
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| RamariaError::io(format!("MCP 初始化握手失败：{e}"), None))?;
    tracing::info!(
        db = %db_path.display(),
        idle_sealing = idle_loop.is_some(),
        "MCP 服务端已就绪（stdio；工具错误会以结果内 isError 返回）"
    );

    let cancel = running.cancellation_token();
    tokio::select! {
        reason = running.waiting() => {
            // 传输关闭 = 客户端退出（正常路径）
            tracing::info!(reason = ?reason, "MCP 传输已关闭，服务端退出");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("收到退出信号，关闭 MCP 服务端");
            cancel.cancel();
        }
    }

    // ---- 6. 优雅关停后台任务（在途封存等待收敛，最多 15 秒）----
    if let Some(mut idle_loop) = idle_loop {
        idle_loop.shutdown().await;
    }
    Ok(())
}
