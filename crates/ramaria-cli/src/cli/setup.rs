//! crates/ramaria-cli/src/cli/setup.rs - 首次配置子命令参数定义
//!
//! 设计特点:
//! - 配置向导命令族的 clap 参数单独成文件（顶层定义文件按命令族拆分，控制单文件规模）
//! - 全部参数可选：缺省进入逐项提问交互向导，给出 `--provider` 即转非交互
//! - API key 可经 `RAMARIA_API_KEY` 环境变量注入，避免落入 shell 历史
//! - 除 provider 外的参数缺省值由 provider 推导，不在此处硬编码地址与模型

use clap::Args;

// =========================================================
// 首次配置参数
// =========================================================

/// `ramaria setup` 的参数（缺省交互，给出 provider 即非交互）。
#[derive(Args)]
pub(crate) struct SetupArgs {
    /// LLM 服务类型: lmstudio / lm_studio / lm-studio / deepseek / openai
    ///
    /// 缺省（不给出本参数）进入交互式向导。
    #[arg(long)]
    pub(crate) provider: Option<String>,

    /// API 基础地址（缺省用 provider 的默认地址）
    #[arg(long)]
    pub(crate) base_url: Option<String>,

    /// 模型标识（缺省用 provider 的默认模型）
    #[arg(long)]
    pub(crate) model: Option<String>,

    /// 线上 provider 的 API key（亦可用 RAMARIA_API_KEY 环境变量，避免进 shell 历史）
    #[arg(long)]
    pub(crate) api_key: Option<String>,

    /// 本地嵌入模型目录（缺省保留已保存的路径）
    #[arg(long)]
    pub(crate) embedding_path: Option<String>,

    /// 跳过末尾 LLM 健康探测（配置与完成标记照常写入，状态按配置完整性判定）
    #[arg(long)]
    pub(crate) no_validate: bool,
}
