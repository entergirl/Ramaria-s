//! crates/ramaria-cli/src/main.rs - Ramaria CLI 入口
//!
//! 设计特点:
//! - clap derive 模式定义命令结构，全局 --json / --yes / --quiet / --db 对所有命令可用
//! - 全局 --json：统一信封 `{"ok":true,"data":…}` / `{"ok":false,"error":{"code":…,"message":"…"}}`（统一信封 schema，见 docs/dev-1.5/v1.5-decisions.md §D-V15-011）
//! - stdout 只输出数据；状态/提示/警告走 stderr（ui::info/success/warn 已改 eprintln）
//! - exit code 约定：0 成功 / 2 参数错(clap) / 3 LLM 或后端不可用 / 4 业务校验失败
//! - `ramaria help` 按 对话/记忆/数据/管理/高级 分组（subcommand_help_heading）
//! - blocks 为 canonical 命令名，utt 保留为 alias（人性化别名决策，见 docs/dev-1.5/v1.5-decisions.md §D-V15-007）
//! - App 统一初始化（DB → storage → LLM → App）

// 命令模块通过 lib.rs 暴露（pub mod），以供集成测试使用
use ramaria_cli::commands;
use ramaria_cli::ui;

use anyhow::Context;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use ramaria_core::StorageBackend;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::EmbeddingProvider;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;

// =========================================================
// CLI 参数定义
// =========================================================

/// Ramaria — 带记忆能力的 AI 助手 CLI
#[derive(Parser)]
#[command(name = "ramaria", version, about, long_about = None)]
struct Cli {
    /// 全局选项
    #[command(subcommand)]
    command: Commands,

    /// 数据库文件路径（默认: ./data/ramaria_assistant.db，可通过 RAMARIA_DB_PATH 环境变量覆盖）
    ///
    /// 优先级: `--db` 命令行参数 > `RAMARIA_DB_PATH` 环境变量 > 默认路径
    /// （clap 原生保证：显式参数 > env > default_value）。
    #[arg(
        long,
        global = true,
        env = "RAMARIA_DB_PATH",
        default_value = "data/ramaria_assistant.db"
    )]
    db: PathBuf,

    /// 自动确认所有确认点（隐私/删除/导入等）；非 TTY 且无 --yes 时不挂起、直接失败并提示（M1 B 项）
    #[arg(long, global = true)]
    yes: bool,

    /// 跳过 LLM 连接验证（仅 setup 命令）
    #[arg(long, global = true)]
    skip_validate: bool,

    /// 以 JSON 信封输出结果（stdout 仅含 JSON；错误 code 复用 exit code 约定）
    #[arg(long, global = true)]
    json: bool,

    /// 抑制 stderr 提示（info/success/warn），仅保留错误输出
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// 发送单条消息并获取回复（默认流式输出）[对话]
    #[command(display_order = 10)]
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

        /// JSON 事件流输出（每行一个 StreamEvent JSON；与全局 --json 等价）
        #[arg(long)]
        json: bool,
    },

    /// 启动交互式对话 REPL [对话]
    #[command(display_order = 11)]
    Chat,

    /// 运行首次配置向导 [对话]
    #[command(display_order = 12)]
    Setup,

    /// 查看记忆（L1 摘要 / L2 事件 / L3 性格）[记忆]
    #[command(display_order = 20)]
    Memory {
        /// 记忆层级: l1|summary / l2|events / l3|profile
        #[arg(default_value = "l1")]
        layer: String,

        /// 按 persona_uid 筛选
        #[arg(long)]
        persona: Option<String>,

        /// 输出条数上限（1-500）
        #[arg(long, default_value = "20", value_parser = parse_limit)]
        limit: usize,

        /// 跳过前 N 条（与 --limit 组合分页）
        #[arg(long, default_value = "0")]
        offset: usize,
    },

    /// 话语块管理（utt 的 canonical 名称；切分参数定稿后重建）[记忆]
    #[command(display_order = 21, visible_alias = "utt", subcommand)]
    Blocks(BlocksCmd),

    /// 索引管理 [记忆]
    #[command(display_order = 22, subcommand)]
    Index(IndexCmd),

    /// 导入外部聊天记录（QQ）[数据]
    #[command(display_order = 30, subcommand)]
    Import(ImportCmd),

    /// 数据导出 [数据]
    #[command(display_order = 31)]
    Export {
        /// 导出格式: json / markdown
        #[arg(default_value = "json")]
        format: String,

        /// 按 persona_uid 筛选
        #[arg(long)]
        persona: Option<String>,

        /// 输出文件路径（默认 stdout，`-` 表示 stdout）
        #[arg(short, long)]
        output: Option<String>,
    },

    /// 会话管理 [管理]
    #[command(display_order = 40, subcommand)]
    Session(SessionCmd),

    /// 配置管理 [管理]
    #[command(display_order = 41, subcommand)]
    Config(ConfigCmd),

    /// 人格管理（list / show / reload）[管理]
    #[command(display_order = 42, subcommand)]
    Persona(PersonaCmd),

    /// 行为规则管理（list / show / import / edit / enable / disable / delete / evidence）[管理]
    #[command(display_order = 44, subcommand)]
    Rule(RuleCmd),

    /// 导出诊断信息（打包日志、配置、系统信息为 .zip）[管理]
    #[command(display_order = 43)]
    Diagnostics {
        /// 输出文件路径（默认: ramaria-diagnostics-{timestamp}.zip）
        #[arg(short, long)]
        output: Option<String>,
    },

    /// 应用状态探活（agent 使用：状态/配置摘要/DB 路径）[高级]
    #[command(display_order = 50)]
    Status,

    /// 探针实验（build: 构建测试集 / run: 档位批量实验）[高级]
    #[command(display_order = 51, subcommand)]
    Probe(ProbeArgs),
}

/// 行为规则管理子命令（§2.9 词表：list/show/import/edit/enable/disable/delete/evidence）。
#[derive(Subcommand)]
enum RuleCmd {
    /// 列出行为规则（按 persona 筛选）
    List {
        /// 按 persona_uid 筛选（默认 rama-0001）
        #[arg(long)]
        persona: Option<String>,
        /// 输出条数上限（1-500）
        #[arg(long, default_value = "100", value_parser = parse_limit)]
        limit: usize,
        /// 跳过前 N 条（与 --limit 组合分页）
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// 查看单条规则详情
    Show {
        /// 规则 id
        id: i64,
    },
    /// 手工导入规则（JSON 文件，`-` = stdin）
    Import {
        /// 导入源文件路径（`-` = stdin）
        file: String,
        /// 规则所属 persona（默认 rama-0001）
        #[arg(long)]
        persona: Option<String>,
    },
    /// 编辑规则（reaction / avoid；编辑后转为 Manual 并写 S1 反馈）
    Edit {
        /// 规则 id
        id: i64,
        /// 新的规则文本（缺省保留原值）
        #[arg(long)]
        reaction: Option<String>,
        /// 新的禁忌列表（逗号分隔，缺省保留原值）
        #[arg(long)]
        avoid: Option<String>,
    },
    /// 启用规则
    Enable {
        /// 规则 id
        id: i64,
    },
    /// 禁用规则（写 S1 反馈日志）
    Disable {
        /// 规则 id
        id: i64,
    },
    /// 删除规则（需确认；--yes/--force 自动通过）
    Delete {
        /// 规则 id
        id: i64,
        /// 跳过交互确认（双保险）
        #[arg(long)]
        force: bool,
    },
    /// 展示规则证据链（规则 → 事件 → 原文摘要）
    Evidence {
        /// 规则 id
        id: i64,
    },
}

/// 话语块管理子命令（canonical 名称 blocks，别名 utt）。
#[derive(Subcommand)]
enum BlocksCmd {
    /// 重建全部会话的话语块
    Rebuild {
        /// 强制模式：先清空全部旧块再全量重切
        /// （切分参数 θ_gap / 条数上限变更后必须使用）
        #[arg(long)]
        force: bool,
    },
}

/// 探针子命令（build 的旧名 `dataset` 保留为 alias，人性化别名决策，见 docs/dev-1.5/v1.5-decisions.md §D-V15-007）。
#[derive(Subcommand)]
enum ProbeArgs {
    /// 构建测试集（原 `probe dataset`，动词化后保留 alias）
    #[command(visible_alias = "dataset")]
    Build {
        /// 目标 persona_uid（默认自动选择白名单内角色类 persona，兜底 char-0001）
        #[arg(long)]
        persona: Option<String>,

        /// 每维题数（默认 10，2 维共 20 题；v1.7 正式评估可扩大至 ≥30 题）
        #[arg(long, default_value_t = ramaria_cli::commands::probe::DEFAULT_QUESTIONS_PER_DIM)]
        questions_per_dim: usize,

        /// 抽样 seed（固定可复跑；同 seed 输出相同测试集）
        #[arg(long, default_value_t = ramaria_cli::commands::probe::DEFAULT_SEED)]
        seed: u64,

        /// 显式数据源文件（JSON；不指定则从数据库构建，无真实数据时夹具兜底）
        #[arg(long)]
        source: Option<PathBuf>,

        /// 数据集输出文件（`-` = stdout；不指定时 --json 输出完整数据集）
        #[arg(long)]
        output: Option<String>,
    },

    /// 按参数档位批量跑对话管线，结构化输出（档位 → 输出 → 指标）
    Run {
        /// 数据集文件（`ramaria probe build` 的产物）
        #[arg(long)]
        dataset: PathBuf,

        /// 只跑指定档位（逗号分隔 id，默认全部；无效 id 跳过）
        #[arg(long)]
        variants: Option<String>,

        /// 每档位最多跑题数（默认全部）
        #[arg(long)]
        limit: Option<usize>,

        /// 按档位参数重建 utt 块（默认开启；θ_gap/条数档位必须重建才生效，
        /// 用 --no-rebuild-utt 关闭）
        #[arg(long, default_value_t = true)]
        rebuild_utt: bool,

        /// 结果输出文件（`-` = stdout 输出原始结果 JSON）
        #[arg(long)]
        output: Option<String>,
    },
}

/// 导入子命令。
#[derive(Subcommand)]
enum ImportCmd {
    /// 导入 QQ 聊天记录
    Qq {
        /// 聊天记录文件路径（QQ Chat Exporter v6.x JSON 格式）
        #[arg(short, long)]
        file: String,

        /// 深度导入模式（触发完整 L0→L1→L2→L3 记忆管线）
        #[arg(long)]
        deep: bool,

        /// 强制导入（跳过确认，等同 --yes 双保险）
        #[arg(long)]
        force: bool,

        /// 仅解析预览（输出结构化 JSON 摘要，不写入数据库）
        #[arg(long)]
        dry_run: bool,

        /// 导出者 persona 名称（向后兼容，默认使用文件中解析的导出者名称）
        #[arg(long)]
        persona: Option<String>,

        /// 导出者 persona 名称（功能同 --persona，用于语义明确场景）
        #[arg(long)]
        persona_self_name: Option<String>,

        /// 导出者 persona UID（可选，留空按优先级自动生成: uin > uid > seq）
        #[arg(long)]
        persona_self_uid: Option<String>,

        /// 对话对方 persona 名称（默认使用文件中解析的对方名称）
        #[arg(long)]
        persona_other_name: Option<String>,

        /// 对话对方 persona UID（可选，留空按优先级自动生成）
        #[arg(long)]
        persona_other_uid: Option<String>,

        /// 导入侧过滤: self（仅我方）| other（仅对方）| both（默认）
        #[arg(long, default_value = "both", value_parser = parse_import_side)]
        side: ramaria_importer::qq::ImportSide,

        /// session 切割时间间隔（分钟），默认 10
        #[arg(long, default_value = "10")]
        gap: u32,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// 列出所有会话
    #[command(display_order = 10)]
    List {
        /// 输出条数上限（默认全部）
        #[arg(long)]
        limit: Option<usize>,
        /// 跳过前 N 条
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// 查看指定会话的消息历史
    #[command(display_order = 20)]
    Show {
        /// 会话 UUID
        session_id: String,
    },
    /// 删除指定会话（需确认；非 TTY 需 --yes，--force 双保险）
    #[command(display_order = 30)]
    Delete {
        /// 会话 UUID
        session_id: String,
        /// 强制删除（跳过确认，等同 --yes 双保险）
        #[arg(long)]
        force: bool,
    },
    /// 为指定会话重新生成 L1 摘要（手动重试）
    #[command(display_order = 40)]
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
        /// 配置项名称（provider / base_url / temperature / max_tokens / api_key / state / utt.* / bridge.*）
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
    /// 列出所有人格（uid / 名称 / kind 结构化）
    List {
        /// 输出条数上限（默认全部）
        #[arg(long)]
        limit: Option<usize>,
        /// 跳过前 N 条
        #[arg(long, default_value = "0")]
        offset: usize,
    },
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

    // 使用分组帮助的 Command 解析（--help 按 对话/记忆/数据/管理/高级 分组）
    let cli = Cli::from_arg_matches(&grouped_command().get_matches()).unwrap_or_else(|e| e.exit());

    // 设置 --quiet（抑制 stderr 提示，仅错误）
    ui::set_quiet(cli.quiet);

    let json_mode = cli.json;

    // 初始化 App（后端不可用视为 exit code 3）
    let (app, pool) = match init_app(cli.db.clone()).await {
        Ok((a, p)) => (a, p),
        Err(e) => exit_with_error(&e, json_mode),
    };

    // 调度命令
    let result = dispatch(&app, &pool, cli).await;

    if let Err(e) = result {
        exit_with_error(&e, json_mode);
    }
}

/// 带分组的 clap Command（`ramaria help` 按 对话/记忆/数据/管理/高级 分组显示子命令）。
fn grouped_command() -> clap::Command {
    let mut cmd = Cli::command();
    for (name, heading) in [
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
        ("diagnostics", "管理"),
        ("status", "高级"),
        ("probe", "高级"),
    ] {
        cmd = cmd.mut_subcommand(name, |c| c.subcommand_help_heading(heading));
    }
    cmd
}

// =========================================================
// 错误处理与 exit code 约定（见 docs/dev-1.5/v1.5-decisions.md §D-V15-011）
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

/// 按 exit code 约定输出错误并退出进程。
///
/// json 模式下先向 stdout 输出错误信封（`{"ok":false,"error":{...}}`），
/// 文本错误始终走 stderr，随后以约定 exit code 退出。
fn exit_with_error(err: &anyhow::Error, json_mode: bool) -> ! {
    let code = exit_code_for_error(err);
    if json_mode {
        // 错误信封走 stdout（agent 直接取 stdout 即纯数据，含错误）
        ramaria_cli::json::emit_err(code, &format!("{err:#}"));
    }
    // 检查是否有 RamariaError source
    if let Some(ramaria_err) = err.downcast_ref::<RamariaError>() {
        ui::fatal(ramaria_err, code);
    }
    ui::fatal_anyhow(err, code);
}

// =========================================================
// App 初始化
// =========================================================

/// 初始化 App：连接数据库 → 迁移 → 配置双写同步 → 恢复 embedding → 创建 LLM → 构造 App。
///
/// 返回 (App实例, 数据库连接池)。连接池供导入器等需要直接访问 SQLite 的命令使用。
///
/// v1.6 CLI 一致性修复（启动前置）:
/// - config.toml 经 `ConfigSyncService` 加载（对齐桌面端 lib.rs:297-327），
///   `[utt]` 等配置组对 CLI 对话链路生效；缺失生成模板、损坏回退默认记 warn。
/// - 恢复已保存的 embedding provider（`backend_config.embedding_model_path`），
///   目录存在时创建 native provider；缺失/失败 → `None`（BM25 降级记 warn，不阻塞）。
async fn init_app(db_path: PathBuf) -> anyhow::Result<(Arc<ramaria_app::App>, sqlx::SqlitePool)> {
    tracing::info!(db = %db_path.display(), "初始化 App");

    // Step 1: 初始化数据库连接池 + 执行 migration
    let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
        .await
        .context("数据库初始化失败")?;

    let storage = Arc::new(ramaria_storage::SqliteStorage::new(pool.clone()));

    // Step 2: 读取已保存的后端配置（如有）
    let backend_config = storage
        .get_backend_config()
        .await
        .context("读取后端配置失败")?
        .unwrap_or_else(ramaria_core::types::BackendConfig::lm_studio_default);

    // Step 3: 创建 Keychain
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());

    // Step 4: 配置双写同步（v1.4+）：加载 config.toml + DB 两侧，
    // 一致性校验以文件为准（config.toml 与数据库同级目录，约定同桌面端）。
    let data_dir = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let config_path = data_dir.join("config.toml");
    let storage_dyn: Arc<dyn StorageBackend> = storage.clone();
    let config_sync = ramaria_app::ConfigSyncService::new(storage_dyn, config_path.clone());
    let sync_outcome = config_sync.load().await.context("配置同步加载失败")?;
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

    // Step 5: 构造 App 配置（基于同步后的配置，填充实际路径；缓存策略取 [cache] 配置组）
    let mut config = sync_outcome.config;
    config.paths.data_dir = data_dir.to_string_lossy().to_string();
    config.paths.log_dir = data_dir.join("logs").to_string_lossy().to_string();
    config.paths.config_dir = data_dir.to_string_lossy().to_string();
    config.paths.vector_index_dir = data_dir.join("vectors").to_string_lossy().to_string();

    // Step 6: 尝试恢复已保存的嵌入模型（对齐桌面端加载逻辑）。
    // 目录存在 → 创建 native provider（向量通道真实可用）；
    // 目录缺失 / 创建失败 → None（BM25 降级，记 warn 不阻塞启动）。
    let embedding: Option<Arc<dyn EmbeddingProvider>> = {
        match &backend_config.embedding_model_path {
            Some(saved_path) if !saved_path.is_empty() => {
                let model_dir = std::path::Path::new(saved_path);
                if !model_dir.exists() {
                    tracing::warn!(
                        path = %saved_path,
                        "已保存的嵌入模型目录不存在，CLI 将以 BM25 降级模式运行"
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
                                "已恢复嵌入模型（向量通道可用）"
                            );
                            Some(Arc::new(provider) as Arc<dyn EmbeddingProvider>)
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %saved_path,
                                error = %e,
                                "加载已保存的嵌入模型失败，CLI 将以 BM25 降级模式运行"
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

    // Step 7: 创建 LLM Provider（按 [cache] 配置注入精确缓存）
    //
    // - `config.cache.enabled`（默认 true）：创建 SqliteLlmCache 并注入 provider，
    //   重跑导入/重试/失败恢复场景命中缓存不重复花费 API 账单；
    // - 缓存实例同时保存到 App（set_llm_cache），供热更新路径复用。
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
    let llm: Arc<dyn ramaria_core::LlmProviderTrait> = match backend_config.provider {
        ramaria_core::types::LlmProvider::LmStudio => {
            let provider = ramaria_llm::lm_studio::LmStudioProvider::new(backend_config.clone())
                .context("创建 LM Studio provider 失败")?;
            let provider = match &llm_cache {
                Some(cache) => provider.with_cache(Arc::clone(cache)),
                None => provider,
            };
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::DeepSeek => {
            let provider = ramaria_llm::deepseek::DeepSeekProvider::new(
                backend_config.clone(),
                Arc::clone(&keychain),
            )
            .context("创建 DeepSeek provider 失败")?;
            let provider = match &llm_cache {
                Some(cache) => provider.with_cache(Arc::clone(cache)),
                None => provider,
            };
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::OpenAI => {
            let provider = ramaria_llm::openai::OpenAIProvider::new(
                backend_config.clone(),
                Arc::clone(&keychain),
            )
            .context("创建 OpenAI provider 失败")?;
            let provider = match &llm_cache {
                Some(cache) => provider.with_cache(Arc::clone(cache)),
                None => provider,
            };
            Arc::new(provider)
        }
        _ => {
            anyhow::bail!("不支持的 LLM provider");
        }
    };

    // Step 8: 构造 App（注入 embedding；None = 向量通道降级）
    let app = ramaria_app::App::new(storage, llm, embedding, config, keychain);
    // 保存缓存实例引用：后端热更新（update_llm）时复用同一缓存
    app.set_llm_cache(llm_cache);
    let app = Arc::new(app);

    // Step 9: 刷新状态
    app.refresh_setup_state()
        .await
        .context("刷新应用状态失败")?;

    tracing::info!(
        state = %app.current_state().as_str(),
        provider = %backend_config.provider.as_str(),
        "App 初始化完成"
    );

    Ok((app, pool))
}

// =========================================================
// 命令调度
// =========================================================

async fn dispatch(app: &Arc<ramaria_app::App>, pool: &SqlitePool, cli: Cli) -> anyhow::Result<()> {
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
                // 业务校验失败（exit code 4，见 docs/dev-1.5/v1.5-decisions.md §D-V15-011）
                return Err(anyhow::anyhow!(RamariaError::validation(
                    "消息不能为空。用法: ramaria ask <消息>"
                )));
            }

            let args = commands::ask::AskArgs {
                message: msg,
                persona,
                session,
                no_stream,
                // 子命令 --json 与全局 --json 等价（任一开启即事件流输出）
                json: cli.json || json,
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
            offset,
        } => {
            let args = commands::memory::MemoryArgs {
                layer,
                persona,
                limit,
                offset,
                json: cli.json,
            };
            commands::memory::run(app, args).await?;
        }
        Commands::Blocks(sub) => match sub {
            BlocksCmd::Rebuild { force } => {
                commands::utt::run(app, commands::utt::UttCmd::Rebuild { force }).await?;
            }
        },
        Commands::Index(sub) => match sub {
            IndexCmd::Rebuild => {
                commands::index_cmd::run(app).await?;
            }
        },
        Commands::Session(sub) => {
            let cmd = match sub {
                SessionCmd::List { limit, offset } => {
                    commands::session::SessionCmd::List { limit, offset }
                }
                SessionCmd::Show { session_id } => {
                    commands::session::SessionCmd::Show { session_id }
                }
                SessionCmd::Delete { session_id, force } => {
                    commands::session::SessionCmd::Delete { session_id, force }
                }
                SessionCmd::Summarize {
                    session_id,
                    persona,
                } => commands::session::SessionCmd::Summarize {
                    session_id,
                    persona_uid: persona,
                },
            };
            commands::session::run(app, cmd, cli.json, cli.yes).await?;
        }
        Commands::Config(sub) => {
            let cmd = match sub {
                ConfigCmd::List => commands::config::ConfigCmd::List,
                ConfigCmd::Get { key } => commands::config::ConfigCmd::Get { key },
                ConfigCmd::Set { key, value } => commands::config::ConfigCmd::Set { key, value },
            };
            commands::config::run(app, cmd, cli.json).await?;
        }
        Commands::Persona(sub) => {
            let cmd = match sub {
                PersonaCmd::List { limit, offset } => {
                    commands::persona::PersonaCmd::List { limit, offset }
                }
                PersonaCmd::Show => commands::persona::PersonaCmd::Show,
                PersonaCmd::Reload { uid } => commands::persona::PersonaCmd::Reload { uid },
            };
            commands::persona::run(app, cmd, cli.json).await?;
        }
        Commands::Rule(sub) => {
            let cmd = match sub {
                RuleCmd::List {
                    persona,
                    limit,
                    offset,
                } => commands::rule::RuleCmd::List {
                    persona,
                    limit: Some(limit),
                    offset,
                },
                RuleCmd::Show { id } => commands::rule::RuleCmd::Show { id },
                RuleCmd::Import { file, persona } => {
                    commands::rule::RuleCmd::Import { file, persona }
                }
                RuleCmd::Edit {
                    id,
                    reaction,
                    avoid,
                } => commands::rule::RuleCmd::Edit {
                    id,
                    reaction,
                    avoid,
                },
                RuleCmd::Enable { id } => commands::rule::RuleCmd::Enable { id },
                RuleCmd::Disable { id } => commands::rule::RuleCmd::Disable { id },
                RuleCmd::Delete { id, force } => commands::rule::RuleCmd::Delete { id, force },
                RuleCmd::Evidence { id } => commands::rule::RuleCmd::Evidence { id },
            };
            commands::rule::run(app, cmd, cli.json, cli.yes).await?;
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
                json: cli.json,
            };
            commands::export::run(app, args).await?;
        }
        Commands::Import(sub) => match sub {
            ImportCmd::Qq {
                file,
                deep,
                force,
                dry_run,
                persona,
                persona_self_name,
                persona_self_uid,
                persona_other_name,
                persona_other_uid,
                side,
                gap,
            } => {
                // --persona 向后兼容（映射为 self_name）
                let effective_self_name = persona_self_name.or(persona);
                let args = commands::import_cmd::ImportArgs {
                    file,
                    deep,
                    dry_run,
                    persona_self_name: effective_self_name,
                    persona_self_uid,
                    persona_other_name,
                    persona_other_uid,
                    gap,
                    side,
                    // --force 与 --yes 双保险
                    yes: cli.yes || force,
                    json: cli.json,
                };
                // 导入命令需要 App（触发 L1 摘要）和数据库连接池
                commands::import_cmd::run(app, pool, args).await?;
            }
        },
        Commands::Diagnostics { output } => {
            let args = commands::diagnostics::DiagnosticsArgs { output };
            commands::diagnostics::run(app, pool, args).await?;
        }
        Commands::Status => {
            let args = commands::status::StatusArgs {
                db_path: cli.db,
                json: cli.json,
            };
            commands::status::run(app, args).await?;
        }
        Commands::Probe(sub) => {
            let cmd = match sub {
                ProbeArgs::Build {
                    persona,
                    questions_per_dim,
                    seed,
                    source,
                    output,
                } => commands::probe::ProbeCmd::Build {
                    persona,
                    questions_per_dim,
                    seed,
                    source,
                    output,
                    json: cli.json,
                },
                ProbeArgs::Run {
                    dataset,
                    variants,
                    limit,
                    rebuild_utt,
                    output,
                } => commands::probe::ProbeCmd::Run {
                    dataset,
                    variants,
                    limit,
                    rebuild_utt,
                    output,
                    json: cli.json,
                },
            };
            commands::probe::run(app, cmd, cli.yes).await?;
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

/// 校验 `--side` 参数: self | other | both。
fn parse_import_side(s: &str) -> Result<ramaria_importer::qq::ImportSide, String> {
    ramaria_importer::qq::ImportSide::parse_cli(Some(s))
}

// =========================================================
// 日志初始化
// =========================================================

fn init_tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;

    // 使用 RUST_LOG 环境变量控制日志级别，默认 info
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // 日志必须走 stderr（M1 A 项：stdout 只输出数据，保证管道/agent 取 stdout 即纯数据）
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
// 单元测试（cli 参数解析，不启动 App）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行化 env 变量测试（多个 #[test] 并行时会互相干扰环境变量）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 解析 `ramaria <args>` 并返回 db 路径（仅解析，不执行命令）。
    fn parse_db(args: &[&str]) -> PathBuf {
        Cli::try_parse_from(args).unwrap().db
    }

    /// RAMARIA_DB_PATH 生效：无 `--db` 时使用环境变量。
    #[test]
    fn db_path_uses_env_when_no_flag() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // safety: edition 2024 下 set_var 为 unsafe；测试内串行使用，无并发读
        unsafe { std::env::set_var("RAMARIA_DB_PATH", "env-data/assistant.db") };
        let db = parse_db(&["ramaria", "status"]);
        unsafe { std::env::remove_var("RAMARIA_DB_PATH") };
        assert_eq!(db, PathBuf::from("env-data/assistant.db"));
    }

    /// `--db` 优先于环境变量（优先级：--db > env > 默认）。
    #[test]
    fn db_path_flag_overrides_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // safety: 同 db_path_uses_env_when_no_flag
        unsafe { std::env::set_var("RAMARIA_DB_PATH", "env-data/assistant.db") };
        let db = parse_db(&["ramaria", "--db", "flag-data/custom.db", "status"]);
        unsafe { std::env::remove_var("RAMARIA_DB_PATH") };
        assert_eq!(db, PathBuf::from("flag-data/custom.db"));
    }

    /// 无 env 且无 `--db` 时使用默认路径。
    #[test]
    fn db_path_defaults_when_nothing_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("RAMARIA_DB_PATH") };
        let db = parse_db(&["ramaria", "status"]);
        assert_eq!(db, PathBuf::from("data/ramaria_assistant.db"));
    }

    // =========================================================
    // init_app 集成测试（真实 SQLite 临时库，不连网）
    // =========================================================

    /// 创建唯一临时测试目录（自动清理）。
    fn temp_test_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("ramaria-cli-init-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup_temp_dir(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 断言 config.toml 参数被 CLI 链路读取。
    #[tokio::test]
    async fn init_app_loads_config_toml() {
        let dir = temp_test_dir("toml");
        let db_path = dir.join("assistant.db");
        // 预写 config.toml：`[utt] theta_gap_minutes = 45`（与默认 30 不同，用于断言读取生效）
        std::fs::write(dir.join("config.toml"), "[utt]\ntheta_gap_minutes = 45\n").unwrap();

        let (app, pool) = init_app(db_path).await.expect("init_app 应成功");
        assert_eq!(
            app.config().utt.theta_gap_minutes,
            45,
            "config.toml 的 [utt] 参数必须被 CLI 链路读取"
        );
        pool.close().await;
        cleanup_temp_dir(&dir);
    }

    /// config.toml 缺失 → 生成含默认值的模板，CLI 以默认配置启动。
    #[tokio::test]
    async fn init_app_generates_template_when_missing() {
        let dir = temp_test_dir("missing");
        let db_path = dir.join("assistant.db");

        let (app, pool) = init_app(db_path).await.expect("init_app 应成功");
        assert_eq!(app.config().utt.theta_gap_minutes, 30, "缺失时用默认值");
        assert!(
            dir.join("config.toml").exists(),
            "config.toml 缺失时应生成模板"
        );
        pool.close().await;
        cleanup_temp_dir(&dir);
    }

    /// config.toml 损坏 → 回退默认值记 warn，启动不失败。
    #[tokio::test]
    async fn init_app_falls_back_on_corrupt_config() {
        let dir = temp_test_dir("corrupt");
        let db_path = dir.join("assistant.db");
        std::fs::write(dir.join("config.toml"), "这不是合法的 TOML [[[").unwrap();

        let (app, pool) = init_app(db_path).await.expect("损坏 config 不应阻塞启动");
        assert_eq!(app.config().utt.theta_gap_minutes, 30, "损坏时回退默认值");
        pool.close().await;
        cleanup_temp_dir(&dir);
    }

    /// embedding_model_path 指向不存在目录 → 降级为 None 不阻塞。
    #[tokio::test]
    async fn init_app_degrades_when_embedding_missing() {
        let dir = temp_test_dir("embed");
        let db_path = dir.join("assistant.db");
        let pool = ramaria_storage::database::init_pool(Some(db_path.clone()))
            .await
            .unwrap();
        let storage = ramaria_storage::SqliteStorage::new(pool.clone());
        let mut backend = ramaria_core::types::BackendConfig::lm_studio_default();
        backend.embedding_model_path =
            Some(dir.join("no-such-model").to_string_lossy().to_string());
        storage.save_backend_config(&backend).await.unwrap();
        pool.close().await;

        let (app, pool) = init_app(db_path).await.expect("embedding 缺失不应阻塞启动");
        assert!(
            !app.is_embedding_available(),
            "模型目录不存在时 embedding 不可用（BM25 降级）"
        );
        pool.close().await;
        cleanup_temp_dir(&dir);
    }
}
