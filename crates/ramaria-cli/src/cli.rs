//! crates/ramaria-cli/src/cli.rs - CLI clap 参数定义模块
//!
//! 设计特点:
//! - clap derive 定义顶层 `Cli` 与全部子命令结构，全局选项对所有命令可用
//! - `Cli` 字段以 `pub(crate)` 暴露给入口与分发模块读取
//! - 子命令枚举按命令族聚合，词表（list/show/import/…）与帮助分组一一对应
//! - 参数值解析器（`parse_limit` / `parse_import_side`）与所校验字段就地定义
//! - 顶层命令的新增须同步登记到 `main.rs` 的帮助分组表

use std::path::PathBuf;

use clap::{Parser, Subcommand};

// =========================================================
// CLI 参数定义
// =========================================================

/// Ramaria — 带记忆能力的 AI 助手 CLI
#[derive(Parser)]
#[command(name = "ramaria", version, about, long_about = None)]
pub(crate) struct Cli {
    /// 全局选项
    #[command(subcommand)]
    pub(crate) command: Commands,

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
    pub(crate) db: PathBuf,

    /// 自动确认所有确认点（隐私/删除/导入等）；非 TTY 且无 --yes 时不挂起、直接失败并提示
    #[arg(long, global = true)]
    pub(crate) yes: bool,

    /// 跳过 LLM 连接验证（仅 setup 命令）
    #[arg(long, global = true)]
    pub(crate) skip_validate: bool,

    /// 以 JSON 信封输出结果（stdout 仅含 JSON；错误 code 复用 exit code 约定）
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// 抑制 stderr 提示（info/success/warn），仅保留错误输出
    #[arg(long, global = true)]
    pub(crate) quiet: bool,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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

        /// 输出文件路径（缺省 exports/export_{timestamp}.{ext}；`-` = stdout）
        #[arg(short, long)]
        output: Option<String>,

        /// 脱敏输出：消息正文与 L1 摘要替换为 <N chars>
        #[arg(long)]
        redact: bool,
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

    /// 行为规则管理（list / show / import / edit / enable / disable / delete / evidence / relearn / clusters）[管理]
    #[command(display_order = 44, subcommand)]
    Rule(RuleCmd),

    /// 表达层风格统计管理（update: 手动补跑风格统计，覆盖无封存来源的导入 persona）[管理]
    #[command(display_order = 47, subcommand)]
    Style(StyleCmd),

    /// 知识事实查询（list / show，只读）[管理]
    #[command(display_order = 45, subcommand)]
    Fact(FactCmd),

    /// 关键词词典管理（list / show / seed / alias list|confirm|reject）[管理]
    #[command(display_order = 46, subcommand)]
    Keyword(KeywordCmd),

    /// 导出诊断信息（打包日志、配置、系统信息为 .zip）[管理]
    #[command(display_order = 43)]
    Diagnostics {
        /// 输出文件路径（默认: ramaria-diagnostics-{timestamp}.zip）
        #[arg(short, long)]
        output: Option<String>,

        /// 诊断导出始终脱敏；该开关为脚本统一传参兼容（幂等）
        #[arg(long)]
        redact: bool,
    },

    /// 应用状态探活（agent 使用：状态/配置摘要/DB 路径）[高级]
    #[command(display_order = 50)]
    Status,

    /// 探针实验（build: 构建测试集 / run: 档位批量实验）[高级]
    #[command(display_order = 51, subcommand)]
    Probe(ProbeArgs),

    /// MCP 服务端（serve: 以 stdio 提供记忆与人格工具）[高级]
    #[command(display_order = 52, subcommand)]
    Mcp(McpCmd),
}

/// MCP 服务端子命令（供外部 MCP 客户端挂载）。
#[derive(Subcommand)]
pub(crate) enum McpCmd {
    /// 以 stdio 启动 MCP 服务端（配置片段见桌面「设置 → MCP 接入」面板）
    Serve,
}

/// 行为规则管理子命令（动词词表：list/show/import/edit/enable/disable/delete/evidence）。
#[derive(Subcommand)]
pub(crate) enum RuleCmd {
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
    /// 触发 persona 全量行为学习（基于全部事件重新聚类并生成/替换 Auto 规则）
    Relearn {
        /// 规则所属 persona（默认 rama-0001）
        #[arg(long)]
        persona: Option<String>,
    },
    /// 统计行为聚类结构（只读：不写库、不调 LLM；缺省用配置值）
    #[command(display_order = 100)]
    Clusters {
        /// 目标 persona（默认 rama-0001）
        #[arg(long)]
        persona: Option<String>,
        /// θ_nb 覆盖（0.0-1.0；缺省用配置值）
        #[arg(long)]
        theta_nb: Option<f64>,
        /// min_cluster_size 覆盖（≥1；缺省用配置值）
        #[arg(long)]
        min_cluster_size: Option<usize>,
        /// β1 覆盖（≥0；与 β2 之和 ≤ 1；缺省用配置值）
        #[arg(long)]
        beta1: Option<f64>,
        /// β2 覆盖（≥0；与 β1 之和 ≤ 1；缺省用配置值）
        #[arg(long)]
        beta2: Option<f64>,
        /// θ_join 档位（0.0-1.0；可重复/多值）——给出任一值时启用 θ_join 时序增量模拟（只读）
        #[arg(long, num_args = 1..)]
        theta_join: Vec<f64>,
        /// θ_join 模拟的前段事件占比（0.1-0.9；缺省 0.8）
        #[arg(long, default_value_t = 0.8)]
        split_ratio: f64,
    },
}

/// 表达层风格统计管理子命令（update: 手动补跑表达层风格统计）。
#[derive(Subcommand)]
pub(crate) enum StyleCmd {
    /// 触发 persona 风格统计增量更新（基于全部消息重新计算五维并生成/替换自动风格规则）
    Update {
        /// 目标 persona_uid（默认 rama-0001）
        #[arg(long)]
        persona: Option<String>,
    },
}

/// 知识事实查询子命令（**无 delete**，双端不做事实删除）。
#[derive(Subcommand)]
pub(crate) enum FactCmd {
    /// 列出 persona 的 active 知识事实（按 field 分组）
    List {
        /// 按 persona_uid 过滤（默认 rama-0001）
        #[arg(long)]
        persona: Option<String>,
        /// 按 field 过滤（basic_info/personal_status/interests/social/history/recent_context/speaking_style）
        #[arg(long)]
        field: Option<String>,
        /// 输出条数上限（1-500）
        #[arg(long, default_value = "100", value_parser = parse_limit)]
        limit: usize,
        /// 跳过前 N 条
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// 查看单条知识事实详情（含完整版本链）
    Show {
        /// 事实 id
        id: i64,
    },
}

/// 关键词词典管理子命令。
#[derive(Subcommand)]
pub(crate) enum KeywordCmd {
    /// 列出 keyword_pool 全部词条
    List,
    /// 查看单个词条详情
    Show {
        /// 关键词文本
        keyword: String,
    },
    /// 手工注入规范词（幂等：已存在保持现状，不递增 use_count）
    Seed {
        /// 待注入的规范词文本（可多个）
        #[arg(required = true)]
        keyword: Vec<String>,
    },
    /// 待确认别名管理
    #[command(subcommand)]
    Alias(KeywordAliasCmd),
}

/// keyword alias 子命令。
#[derive(Subcommand)]
pub(crate) enum KeywordAliasCmd {
    /// 列出待确认别名冲突（alias 文本 → 建议规范词）
    List,
    /// 确认别名合并（pending → alias）
    Confirm {
        /// 别名文本
        alias: String,
    },
    /// 驳回别名（pending → 独立规范词）
    Reject {
        /// 别名文本
        alias: String,
    },
}

/// 话语块管理子命令（canonical 名称 blocks，别名 utt）。
#[derive(Subcommand)]
pub(crate) enum BlocksCmd {
    /// 重建全部会话的话语块
    Rebuild {
        /// 强制模式：先清空全部旧块再全量重切
        /// （切分参数 θ_gap / 条数上限变更后必须使用）
        #[arg(long)]
        force: bool,
    },
}

/// 探针子命令。
#[derive(Subcommand)]
pub(crate) enum ProbeArgs {
    /// 构建测试集（`dataset` 保留为 alias）
    #[command(visible_alias = "dataset")]
    Build {
        /// 目标 persona_uid（默认自动选择白名单内角色类 persona，兜底 char-0001）
        #[arg(long)]
        persona: Option<String>,

        /// 每维题数（默认 10，3 维共 30 题；正式评估可扩大至 ≥30 题）
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

        /// 追加 15 档消融 Profile（B0/B1/F0/F1~F4/S_*/I_*），用于消融数据集构建
        #[arg(long)]
        ablation: bool,
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

        /// 不按档位参数重建 utt 块（复用库中已建块；θ_gap/条数档位仍需重建才生效，
        /// 非切分档位——如仅 top_k 变化——可跳过重建复用已建块）
        #[arg(long)]
        no_rebuild_utt: bool,

        /// 统计法重复次数 N（多次运行取均值 ± 置信区间；默认 1 即不聚合）
        #[arg(long, default_value_t = 1)]
        repeat: usize,

        /// 结果输出文件（`-` = stdout 输出原始结果 JSON）
        #[arg(long)]
        output: Option<String>,
    },

    /// 对 `probe run` 实验结果自动评分（事实维 golden + 语气维 LLM-as-judge）
    Evaluate {
        /// 实验结果文件（`ramaria probe run --output` 的产物）
        #[arg(long)]
        results: PathBuf,

        /// 数据集文件（`ramaria probe build --output` 的产物；提供时按 golden reference 精确评分）
        #[arg(long)]
        dataset: Option<PathBuf>,

        /// 只评指定档位（逗号分隔 id，默认全部）
        #[arg(long)]
        variants: Option<String>,

        /// 评分数值输出文件（`-` = stdout 输出评分 JSON）
        #[arg(long)]
        output: Option<String>,

        /// 跳过语气维 LLM-as-judge（仅评事实维，节省 LLM 调用）
        #[arg(long)]
        no_tone_judge: bool,
    },

    /// 生成档位对比报告与定稿建议（markdown/JSON 双形态；支持人工抽检校准）
    Report {
        /// 实验结果文件（`ramaria probe run --output` 的产物）
        #[arg(long)]
        results: PathBuf,

        /// 评分数值文件（`ramaria probe evaluate --output` 的产物）
        #[arg(long)]
        evaluation: Option<PathBuf>,

        /// 人工抽检校准文件（JSON: 数组 of {item_id, score}）
        #[arg(long)]
        calibration: Option<PathBuf>,

        /// 报告输出文件（`-` = stdout；.md 为 markdown、.json 为 JSON）
        #[arg(long)]
        output: Option<String>,

        /// 消融对比报告模式：自动识别 F0 基线与 F1~F4（及 S 组 vs B1），
        /// 按题目配对 Wilcoxon + Cohen's d + 95% CI + FDR 校正输出统计判定；
        /// 需要 --evaluation 评分数值文件。
        #[arg(long)]
        ablation: bool,
    },
}

/// 导入子命令。
#[derive(Subcommand)]
pub(crate) enum ImportCmd {
    /// 导入 QQ 聊天记录
    Qq {
        /// 聊天记录文件路径（QQ Chat Exporter v6.x JSON 格式）
        #[arg(short, long)]
        file: PathBuf,

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

        /// 导出者 persona UID（可选，留空按优先级自动生成: 显式指定 > uin > uid > seq）
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

        /// 不输出解析报告（报告含导出者/对方标识；默认输出为掩码版）
        #[arg(long)]
        no_report: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionCmd {
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
        /// 渐进式感知重摘要：`[l1.progressive]` 开启且会话触发阈值时生成多段 L1（未触发回退单段）
        #[arg(long)]
        progressive: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCmd {
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
pub(crate) enum PersonaCmd {
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
pub(crate) enum IndexCmd {
    /// 重建检索索引
    Rebuild,
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
