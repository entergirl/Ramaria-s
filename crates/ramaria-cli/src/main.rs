//! rust/crates/ramaria-cli/src/main.rs - Ramaria CLI 入口
//!
//! 设计特点:
//! - clap derive 模式定义命令结构（T-CLI-001）
//! - tracing-subscriber 初始化（RUST_LOG 控制日志级别）
//! - App 统一初始化（DB → storage → LLM → App）
//! - 所有命令委托给 commands/ 模块
//! - 错误统一通过 ui::fatal 输出
//! - --db 指定数据库路径
//! - --yes 在线隐私自动确认（T-CLI-010 规则：需显式指定 provider）

// 命令模块通过 lib.rs 暴露（pub mod），以供集成测试使用
use ramaria_cli::commands;
use ramaria_cli::ui;

use anyhow::Context;
use clap::{Parser, Subcommand};
use ramaria_core::StorageBackend;
use std::path::PathBuf;
use std::sync::Arc;

// =========================================================
// CLI 参数定义 (T-CLI-001)
// =========================================================

/// Ramaria — 带记忆能力的 AI 助手 CLI
#[derive(Parser)]
#[command(name = "ramaria", version, about, long_about = None)]
struct Cli {
    /// 全局选项
    #[command(subcommand)]
    command: Commands,

    /// 数据库文件路径（默认: ./data/ramaria_assistant.db，可通过 RAMARIA_DB_PATH 环境变量覆盖）
    #[arg(long, global = true, default_value = "data/ramaria_assistant.db")]
    db: PathBuf,

    /// 跳过隐私确认（仅线上 provider 生效，需显式配置 provider）
    #[arg(long, global = true)]
    yes: bool,

    /// 跳过 LLM 连接验证（仅 setup 命令）
    #[arg(long, global = true)]
    skip_validate: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// 运行首次配置向导
    Setup,

    /// 发送单条消息并获取回复（默认流式输出）
    Ask {
        /// 用户消息
        message: Vec<String>,

        /// 指定 persona_uid（默认: rama-0001）
        #[arg(long)]
        persona: Option<String>,

        /// 指定 session_id（复用已有会话）
        #[arg(long)]
        session: Option<String>,

        /// 非流式输出（等待完整回复后一次性打印）
        #[arg(long)]
        no_stream: bool,

        /// JSON 事件流输出（每行一个 StreamEvent JSON）
        #[arg(long)]
        json: bool,
    },

    /// 启动交互式对话 REPL
    Chat,

    /// 查看记忆（L1 摘要 / L2 事件 / L3 性格）
    Memory {
        /// 记忆层级: l1 / l2 / l3
        #[arg(default_value = "l1")]
        layer: String,

        /// 按 persona_uid 筛选
        #[arg(long)]
        persona: Option<String>,

        /// 输出条数上限（1-500）
        #[arg(long, default_value = "20", value_parser = parse_limit)]
        limit: usize,
    },

    /// 会话管理
    #[command(subcommand)]
    Session(SessionCmd),

    /// 配置管理
    #[command(subcommand)]
    Config(ConfigCmd),

    /// 人格文件管理（查看/重新加载）
    #[command(subcommand)]
    Persona(PersonaCmd),

    /// 索引管理
    #[command(subcommand)]
    Index(IndexCmd),

    /// 数据导出
    Export {
        /// 导出格式: json / markdown
        #[arg(default_value = "json")]
        format: String,

        /// 按 persona_uid 筛选
        #[arg(long)]
        persona: Option<String>,

        /// 输出文件路径（默认 stdout）
        #[arg(short, long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// 列出所有会话
    List,
    /// 查看指定会话的消息历史
    Show {
        /// 会话 UUID
        session_id: String,
    },
    /// 删除指定会话
    Delete {
        /// 会话 UUID
        session_id: String,
    },
    /// v1.1: 为指定会话重新生成 L1 摘要（手动重试）
    Summarize {
        /// 会话 UUID
        session_id: String,
        /// 可选的人格标识
        #[arg(long)]
        persona: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// 列出当前完整配置
    List,
    /// 获取单个配置项
    Get {
        /// 配置项名称（provider / base_url / temperature / max_tokens / api_key / state）
        key: String,
    },
    /// 设置配置项
    Set {
        /// 配置项名称
        key: String,
        /// 配置项值
        value: String,
    },
}

#[derive(Subcommand)]
enum PersonaCmd {
    /// 显示所有人格摘要
    Show,
    /// 从 personas/ 目录重新加载人格文件到 DB
    Reload {
        /// 指定要重新加载的 persona UID（默认加载全部）
        #[arg(long)]
        uid: Option<String>,
    },
}

#[derive(Subcommand)]
enum IndexCmd {
    /// 重建检索索引
    Rebuild,
}

// =========================================================
// 主入口
// =========================================================

#[tokio::main]
async fn main() {
    // 初始化日志系统
    init_tracing();

    let cli = Cli::parse();

    // 初始化 App
    let app = match init_app(cli.db.clone()).await {
        Ok(a) => a,
        Err(e) => {
            ui::fatal_anyhow(&e, 1);
        }
    };

    // 调度命令
    let result = dispatch(&app, cli).await;

    if let Err(e) = result {
        // 检查是否有 RamariaError source
        if let Some(ramaria_err) = e.downcast_ref::<ramaria_core::error::RamariaError>() {
            ui::fatal(ramaria_err, 1);
        }
        ui::fatal_anyhow(&e, 1);
    }
}

// =========================================================
// App 初始化
// =========================================================

/// 初始化 App：连接数据库 → 迁移 → 创建 LLM → 构造 App。
async fn init_app(db_path: PathBuf) -> anyhow::Result<Arc<ramaria_app::App>> {
    tracing::info!(db = %db_path.display(), "初始化 App");

    // Step 1: 初始化数据库连接池 + 执行 migration
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .context("数据库初始化失败")?;

    let storage = Arc::new(ramaria_storage::SqliteStorage::new(pool));

    // Step 2: 读取已保存的后端配置（如有）
    let backend_config = storage
        .get_backend_config()
        .await
        .context("读取后端配置失败")?
        .unwrap_or_else(ramaria_core::types::BackendConfig::lm_studio_default);

    // Step 3: 创建 Keychain
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());

    // Step 4: 创建 LLM Provider
    let llm: Arc<dyn ramaria_core::LlmProviderTrait> = match backend_config.provider {
        ramaria_core::types::LlmProvider::LmStudio => {
            let provider = ramaria_llm::lm_studio::LmStudioProvider::new(backend_config.clone())
                .context("创建 LM Studio provider 失败")?;
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::DeepSeek => {
            let provider = ramaria_llm::deepseek::DeepSeekProvider::new(
                backend_config.clone(),
                Arc::clone(&keychain),
            )
            .context("创建 DeepSeek provider 失败")?;
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::OpenAI => {
            let provider = ramaria_llm::openai::OpenAIProvider::new(
                backend_config.clone(),
                Arc::clone(&keychain),
            )
            .context("创建 OpenAI provider 失败")?;
            Arc::new(provider)
        }
        _ => {
            anyhow::bail!("不支持的 LLM provider");
        }
    };

    // Step 5: 构造 App
    let config = ramaria_core::config::RamariaConfig::default();
    let app = ramaria_app::App::new(storage, llm, None, config, keychain);
    let app = Arc::new(app);

    // Step 6: 刷新状态
    app.refresh_setup_state()
        .await
        .context("刷新应用状态失败")?;

    tracing::info!(
        state = %app.current_state().as_str(),
        provider = %backend_config.provider.as_str(),
        "App 初始化完成"
    );

    Ok(app)
}

// =========================================================
// 命令调度
// =========================================================

async fn dispatch(app: &Arc<ramaria_app::App>, cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Setup => {
            commands::setup::run(app, cli.skip_validate).await?;
        }
        Commands::Ask {
            message,
            persona,
            session,
            no_stream,
            json,
        } => {
            let msg = message.join(" ");
            if msg.trim().is_empty() {
                anyhow::bail!("消息不能为空。用法: ramaria ask <消息>");
            }

            let args = commands::ask::AskArgs {
                message: msg,
                persona,
                session,
                no_stream,
                json,
                yes: cli.yes,
            };
            commands::ask::run(app, args).await?;
        }
        Commands::Chat => {
            commands::chat::run(app, cli.yes).await?;
        }
        Commands::Memory {
            layer,
            persona,
            limit,
        } => {
            let args = commands::memory::MemoryArgs {
                layer,
                persona,
                limit,
            };
            commands::memory::run(app, args).await?;
        }
        Commands::Session(sub) => {
            let cmd = match sub {
                SessionCmd::List => commands::session::SessionCmd::List,
                SessionCmd::Show { session_id } => {
                    commands::session::SessionCmd::Show { session_id }
                }
                SessionCmd::Delete { session_id } => {
                    commands::session::SessionCmd::Delete { session_id }
                }
                SessionCmd::Summarize {
                    session_id,
                    persona,
                } => commands::session::SessionCmd::Summarize {
                    session_id,
                    persona_uid: persona,
                },
            };
            commands::session::run(app, cmd).await?;
        }
        Commands::Config(sub) => {
            let cmd = match sub {
                ConfigCmd::List => commands::config::ConfigCmd::List,
                ConfigCmd::Get { key } => commands::config::ConfigCmd::Get { key },
                ConfigCmd::Set { key, value } => commands::config::ConfigCmd::Set { key, value },
            };
            commands::config::run(app, cmd).await?;
        }
        Commands::Index(sub) => match sub {
            IndexCmd::Rebuild => {
                commands::index_cmd::run(app).await?;
            }
        },
        Commands::Persona(sub) => {
            let cmd = match sub {
                PersonaCmd::Show => commands::persona::PersonaCmd::Show,
                PersonaCmd::Reload { uid } => commands::persona::PersonaCmd::Reload { uid },
            };
            commands::persona::run(app, cmd).await?;
        }
        Commands::Export {
            format,
            persona,
            output,
        } => {
            let args = commands::export::ExportArgs {
                format,
                persona,
                output,
            };
            commands::export::run(app, args).await?;
        }
    }

    Ok(())
}

// =========================================================
// 参数校验
// =========================================================

/// 校验 `--limit` 参数: 必须在 1..=500 范围内。
///
/// 参数:
/// - `s`: 用户输入的 limit 字符串。
///
/// 返回:
/// - `Ok(limit)`: 有效的 limit 值。
/// - `Err(msg)`: 无效输入（非数字 / 超出范围）。
fn parse_limit(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|_| format!("'{s}' 不是有效的正整数"))?;
    if !(1..=500).contains(&n) {
        return Err(format!("limit 必须在 1-500 之间，当前值: {n}"));
    }
    Ok(n)
}

// =========================================================
// 日志初始化
// =========================================================

fn init_tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;

    // 使用 RUST_LOG 环境变量控制日志级别，默认 info
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .try_init()
        .ok(); // 忽略重复初始化错误（测试等场景）
}
