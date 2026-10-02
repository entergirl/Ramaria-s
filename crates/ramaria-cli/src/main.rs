//! crates/ramaria-cli/src/main.rs - Ramaria CLI 入口
//!
//! 设计特点:
//! - 装配并驱动 CLI：解析参数 → 初始化服务层引擎 → 分发到命令模块
//! - 全局 --json 统一信封 `{"ok":true,"data":…}` / `{"ok":false,"error":{…}}`
//! - stdout 只输出数据；状态/提示/警告走 stderr，保证管道取 stdout 即纯数据
//! - exit code 约定：0 成功 / 2 参数错(clap) / 3 LLM 或后端不可用 / 4 业务校验失败
//! - 错误文案经服务层统一映射：业务类原文直出，技术类 `{场景}: {类别标题}: {原因}`
//! - `ramaria help` 按 对话/记忆/数据/管理/高级 分组（subcommand_help_heading）

// 命令模块通过 lib.rs 暴露（pub mod），以供集成测试使用
use ramaria_cli::commands;
use ramaria_cli::ui;

use anyhow::Context;
use clap::{CommandFactory, FromArgMatches};
use ramaria_core::error::RamariaError;
use ramaria_core::types::BackendConfig;
use ramaria_service::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::{Cli, Commands, McpCmd};
use crate::dispatch::dispatch;

mod cli;
mod dispatch;

// =========================================================
// 主入口
// =========================================================

#[tokio::main]
async fn main() {
    // 初始化日志系统
    init_tracing();

    // 使用分组帮助的 Command 解析（--help 按 对话/记忆/数据/管理/高级 分组）
    let cli = Cli::from_arg_matches(&grouped_command().get_matches()).unwrap_or_else(|e| e.exit());

    // 设置 --quiet（抑制 stderr 提示，仅错误）
    ui::set_quiet(cli.quiet);

    let json_mode = cli.json;

    // 交互式命令与 --json 不兼容：显式 unsupported（错误信封 + exit 4）。
    // 在引擎初始化前拦截，避免生成 config.toml 等副作用。
    if cli.json {
        let interactive = match &cli.command {
            Commands::Setup => Some("setup"),
            Commands::Chat => Some("chat"),
            _ => None,
        };
        if let Some(name) = interactive {
            let err = anyhow::anyhow!(RamariaError::validation(format!(
                "{name} 为交互式命令，不支持 --json 输出"
            )));
            exit_with_error(&err, true);
        }
    }

    // MCP 服务端：走与传输无关的服务层（不构造应用层实例，避免重复连接池与宿主侧副作用）。
    // stdio 协议期间 stdout 只允许协议消息，故本分支不输出任何数据。
    if let Commands::Mcp(sub) = &cli.command {
        let result = match sub {
            McpCmd::Serve => commands::mcp::serve(cli.db.clone()).await,
        };
        if let Err(e) = result {
            exit_with_error(&e, json_mode);
        }
        return;
    }

    // 初始化服务层引擎（后端不可用视为 exit code 3）
    let engine = match init_app(cli.db.clone()).await {
        Ok(engine) => engine,
        Err(e) => exit_with_error(&e, json_mode),
    };

    // 调度命令
    let result = dispatch(&engine, cli).await;

    if let Err(e) = result {
        exit_with_error(&e, json_mode);
    }
}

/// 子命令帮助分组表（`(命令名, 分组标题)`）。
///
/// 说明: 新增顶层命令必须在此登记（有测试锁定完整性），
/// 否则该命令在 `--help` 中会落到无分组区域。
fn help_groups() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ask", "对话"),
        ("chat", "对话"),
        ("setup", "对话"),
        ("memory", "记忆"),
        ("blocks", "记忆"),
        ("index", "记忆"),
        ("import", "数据"),
        ("export", "数据"),
        ("session", "管理"),
        ("config", "管理"),
        ("persona", "管理"),
        ("rule", "管理"),
        ("fact", "管理"),
        ("keyword", "管理"),
        ("style", "管理"),
        ("diagnostics", "管理"),
        ("status", "高级"),
        ("probe", "高级"),
        ("mcp", "高级"),
    ]
}

/// 带分组的 clap Command（`ramaria help` 按 对话/记忆/数据/管理/高级 分组显示子命令）。
fn grouped_command() -> clap::Command {
    let mut cmd = Cli::command();
    for (name, heading) in help_groups() {
        cmd = cmd.mut_subcommand(name, |c| c.subcommand_help_heading(heading));
    }
    cmd
}

// =========================================================
// 错误处理与 exit code 约定
// =========================================================

/// 将错误映射为 exit code（0 成功 / 2 参数错(clap) / 3 LLM 或后端不可用 / 4 业务校验失败）。
fn exit_code_for_error(err: &anyhow::Error) -> i32 {
    // 直接类型匹配 + source 链遍历（anyhow context 包裹后仍能识别 RamariaError）
    let mut current: Option<&dyn std::error::Error> = Some(err.as_ref());
    while let Some(e) = current {
        if let Some(re) = e.downcast_ref::<RamariaError>() {
            return match re {
                // 3: LLM 或后端不可用（可重试类）
                RamariaError::Llm { .. }
                | RamariaError::Embedding { .. }
                | RamariaError::Storage { .. } => 3,
                // 4: 业务校验失败（修正后可重试；隐私拒绝属业务侧决策）
                RamariaError::Validation { .. } | RamariaError::Privacy { .. } => 4,
                // 其余分类（Config/Serialization/Index/Io/Unsupported）归为通用失败
                _ => 1,
            };
        }
        current = e.source();
    }
    1
}

/// 入口错误文案的固定兜底场景（错误链中没有 anyhow 上下文时使用）。
const FALLBACK_ERROR_SCENE: &str = "命令执行失败";

/// 在 anyhow 错误链中定位服务层统一错误。
fn find_ramaria_error(err: &anyhow::Error) -> Option<&RamariaError> {
    err.chain().find_map(|e| e.downcast_ref::<RamariaError>())
}

/// 错误链最外层上下文文本（链首即服务层错误本身时视为无上下文）。
fn outermost_context(err: &anyhow::Error) -> Option<String> {
    let first = err.chain().next()?;
    if first.downcast_ref::<RamariaError>().is_some() {
        return None;
    }
    Some(first.to_string())
}

/// 服务层错误的入口统一文案；`None` 表示错误链中没有服务层错误（走纯 anyhow 渲染）。
///
/// 场景取 anyhow 链最外层上下文文本，缺省用 [`FALLBACK_ERROR_SCENE`]；
/// 业务类（validation / privacy）由映射函数原文直出，场景不参与拼接。
fn mapped_error_message(err: &anyhow::Error) -> Option<(&RamariaError, String)> {
    let ramaria_err = find_ramaria_error(err)?;
    let scene = outermost_context(err).unwrap_or_else(|| FALLBACK_ERROR_SCENE.to_string());
    Some((
        ramaria_err,
        ramaria_service::entry_error_message(ramaria_err, &scene),
    ))
}

/// 按 exit code 约定输出错误并退出进程。
///
/// json 模式下先向 stdout 输出错误信封（`{"ok":false,"error":{...}}`），
/// 文本错误始终走 stderr，随后以约定 exit code 退出。
///
/// 说明:
/// - 错误链中存在服务层错误时，json 信封与文本模式共用同一条统一映射文案
///   （业务类原文直出；技术类 `{场景}: {类别标题}: {原因}`）；
/// - 纯 anyhow 错误维持原渲染（`{err:#}` 单行链）。
fn exit_with_error(err: &anyhow::Error, json_mode: bool) -> ! {
    let code = exit_code_for_error(err);

    if let Some((ramaria_err, message)) = mapped_error_message(err) {
        if json_mode {
            // 错误信封走 stdout（agent 直接取 stdout 即纯数据，含错误）
            ramaria_cli::json::emit_err(code, &message);
        }
        ui::fatal_message(&message, ramaria_err, code);
    }

    if json_mode {
        ramaria_cli::json::emit_err(code, &format!("{err:#}"));
    }
    ui::fatal_anyhow(err, code);
}

// =========================================================
// 引擎初始化
// =========================================================

/// 初始化服务层引擎：连接数据库 → 迁移 → 配置双写链路 → 后端配置对齐 → 恢复状态。
///
/// 返回装配完成的服务层引擎句柄（存储连接池由引擎持有，导入等用例经其取用）。
///
/// CLI 初始化（启动前置）:
/// - config.toml 经服务层配置用例加载：缺失生成模板、损坏回退默认记 warn，
///   一致性校验以文件为准并回写 DB，`[utt]` 等配置组对对话链路生效；
/// - 后端配置以同步后的库内记录为真相源：装配时按旧记录构建的 provider
///   与同步结果不一致时热更新（新库首次启动按 config.toml 的 `[backend]` 生效）；
/// - 恢复已保存的嵌入模型；缺失 / 加载失败 → 向量通道降级（BM25 + 关键词镜像继续可用）。
async fn init_app(db_path: PathBuf) -> anyhow::Result<Arc<Engine>> {
    tracing::info!(db = %db_path.display(), "初始化引擎");

    let config_path = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("config.toml");

    // Step 1: 装配服务层引擎（连接池 + migration + 配置读取 + LLM / 嵌入恢复）
    let engine = Arc::new(
        Engine::open_with(
            EngineOptions::new(db_path.clone()).with_config_path(config_path.clone()),
        )
        .await?,
    );

    // Step 2: 配置双写链路：加载 config.toml + DB 两侧，
    // 一致性校验以文件为准（config.toml 与数据库同级目录，约定同桌面端）。
    let sync_outcome = engine.reload_config().await.context("配置同步加载失败")?;
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

    // Step 3: 后端配置对齐。
    //
    // 装配阶段按同步前的库内记录构建 provider（新库无记录时回退 lm_studio_default），
    // 同步完成后以库内记录为真相源：与当前 provider 不一致时热更新，
    // 使 config.toml 的 `[backend]`（新库首次启动）或文件侧变更立即生效。
    let backend_config = engine
        .backend_config()
        .await
        .context("重新读取后端配置失败")?
        .unwrap_or_else(BackendConfig::lm_studio_default);
    if !backend_matches(&engine.llm().config().clone(), &backend_config) {
        engine
            .update_backend_config(&backend_config, None)
            .await
            .context("后端配置热更新失败")?;
    }

    // Step 4: 刷新状态
    engine
        .refresh_setup_state()
        .await
        .context("刷新应用状态失败")?;

    tracing::info!(
        state = %engine.current_state().as_str(),
        provider = %backend_config.provider.as_str(),
        "引擎初始化完成"
    );

    Ok(engine)
}

/// 判断 provider 快照与同步后的后端配置是否等价。
///
/// 说明:
/// - 比较字段与后端配置同步口径一致（provider / 地址 / 模型 / 温度 / 输出预算），
///   等价时跳过 provider 重建，避免无谓的热更新与文件重写。
fn backend_matches(active: &BackendConfig, synced: &BackendConfig) -> bool {
    active.provider == synced.provider
        && active.base_url == synced.base_url
        && active.capability.model_id == synced.capability.model_id
        && (active.temperature - synced.temperature).abs() < f64::EPSILON
        && active.max_tokens == synced.max_tokens
}

// =========================================================
// 日志初始化
// =========================================================

fn init_tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;

    // 使用 RUST_LOG 环境变量控制日志级别，默认 info
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // 日志必须走 stderr（stdout 只输出数据，保证管道/agent 取 stdout 即纯数据）
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(std::io::stderr)
        .try_init()
        .ok(); // 忽略重复初始化错误（测试等场景）
}

// =========================================================
// 单元测试（cli 参数解析，不启动引擎）
// =========================================================

#[cfg(test)]
mod tests;
