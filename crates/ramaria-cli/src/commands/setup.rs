//! crates/ramaria-cli/src/commands/setup.rs - 首次配置向导
//!
//! 设计特点:
//! - 双入口: 无 `--provider` 时逐项提问（选 provider → 配地址 → 配嵌入路径 → 输 API key），
//!   给出 `--provider` 时按参数装配，全程不读 stdin
//! - 配置收口服务层用例: 密钥入 keychain → 后端配置落库 → provider 热替换 → 健康探测 →
//!   完成标记均经 `Engine::run_setup`，入口不写第二套配置逻辑
//! - 非交互终端且未给出 `--provider` 时直接报校验错误，不进入 read_line 等待
//! - `--no-validate` 跳过末尾健康探测；本地 LM Studio 跳过 API key 步骤
//! - 人格初始化在配置完成后执行（扫描 config/personas/ 目录批量创建 persona）
//! - 状态回执区分 NeedsSetup / Degraded / Ready，降级不阻塞对话

use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_core::types::{AppState, LlmProvider};
use ramaria_service::{Engine, PersonaFileAction, PersonaLoadMode, SetupRequest};
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

/// 首次配置向导的入口参数（来自 `setup` 子命令）。
///
/// 字段约定:
/// - `provider`: LLM 服务类型文本；为 Some 时走非交互，None 时进入交互提问。
/// - `base_url` / `model`: 覆盖值；空白或 None 时取 provider 默认值。
/// - `api_key`: 线上 provider 的密钥；命令行未给出时，非交互路径回退
///   `RAMARIA_API_KEY` 环境变量（交互模式不读取，避免环境变量被误判为参数）。
/// - `embedding_path`: 嵌入模型目录覆盖；空白或 None 保留已保存路径。
/// - `no_validate`: 跳过末尾健康探测（配置与完成标记照常写入）。
///
/// 说明: `base_url` / `model` / `api_key` / `embedding_path` 属非交互参数，
/// 只与 `provider` 一起使用；`no_validate` 两种模式均可用。
pub struct SetupOptions {
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub embedding_path: Option<String>,
    pub no_validate: bool,
}

/// 运行首次配置向导（交互或非交互）。
///
/// 参数:
/// - `engine`: 服务层引擎引用（初始状态应为 NeedsSetup）。
/// - `opts`: 命令参数；`provider` 为 Some 时非交互，None 时进入交互提问。
///
/// 返回:
/// - 配置写入、完成标记与人格初始化完成后返回；降级状态不算失败（以 warn 提示）。
///
/// 说明:
/// - 非交互终端且未给出 `--provider` 时返回 `Validation`，不进入 read_line 等待；
/// - 交互模式下给出任一非交互参数（base_url / model / api_key / embedding_path）返回
///   `Validation`，避免参数被静默忽略；
/// - 配置写入与状态推进统一由 `Engine::run_setup` 完成，入口只做参数装配与结果展示。
pub async fn run(engine: &Arc<Engine>, opts: SetupOptions) -> anyhow::Result<()> {
    crate::ui::separator();
    println!("  Ramaria 首次配置向导");
    crate::ui::separator();
    println!();

    let request = match opts.provider.as_deref() {
        Some(provider) => build_non_interactive_request(
            provider,
            opts.base_url,
            opts.model,
            resolve_api_key(opts.api_key),
            opts.embedding_path,
            opts.no_validate,
        )?,
        None => {
            // 交互模式只接受 --no-validate：其余参数无从对应，静默忽略会让用户误以为已生效
            reject_non_interactive_args_without_provider(&opts)?;
            // 无参数时需要 stdin 提问；非 TTY 下提问会阻塞，直接给出可操作错误
            if !std::io::stdin().is_terminal() {
                return Err(anyhow::anyhow!(RamariaError::validation(
                    "当前环境非交互式终端；请提供 --provider 及配套参数"
                )));
            }
            build_interactive_request(engine, opts.no_validate)?
        }
    };

    // ---- 配置写入与状态推进（服务层单一入口：含完成标记与健康探测） ----
    let state = engine.run_setup(&request).await.context("首次配置失败")?;

    // ---- 人格初始化（配置就绪后执行，与桌面向导一致） ----
    create_initial_personas(engine).await?;

    crate::ui::info(&format!("应用状态: {state}"));
    if state == AppState::NeedsSetup {
        crate::ui::warn("应用仍需要进一步配置（如 embedding 模型下载）");
    } else if state == AppState::Degraded {
        crate::ui::warn("配置已保存，当前以降级状态运行（LLM 不可达或嵌入模型缺失）");
    }

    crate::ui::separator();
    crate::ui::success("配置向导完成！使用 `ramaria ask <消息>` 开始对话。");
    Ok(())
}

/// 逐项提问组装首次配置请求（调用方已确认 stdin 为终端）。
///
/// 参数:
/// - `engine`: 服务层引擎（读取已保存的 API key 供复用）。
/// - `no_validate`: 跳过末尾健康探测。
///
/// 返回:
/// - 可直接提交给 `Engine::run_setup` 的请求；模型标识取 provider 默认值，
///   嵌入模型路径始终按向导提问（命令行预填由非交互模式承担）。
fn build_interactive_request(
    engine: &Arc<Engine>,
    no_validate: bool,
) -> anyhow::Result<SetupRequest> {
    let provider = select_provider()?;
    let base_url = configure_base_url(provider)?;
    let embedding_model_path = configure_embedding_model_path()?;
    let api_key = if provider.is_online() {
        Some(configure_api_key(engine, provider)?)
    } else {
        None
    };

    Ok(SetupRequest {
        provider,
        model_id: default_model_id(provider).to_string(),
        base_url,
        api_key,
        embedding_model_path,
        skip_validate: no_validate,
    })
}

// =========================================================
// 非交互参数装配（纯函数，供脚本与单测使用）
// =========================================================

/// 交互模式下拒绝非交互专属参数。
///
/// 参数:
/// - `opts`: 命令参数；`provider` 为 None 时期望其余非交互参数均为空。
///
/// 返回:
/// - `Ok`: 未给 `provider` 且只带 `--no-validate`（或什么都没带）；
/// - `Err`: 给了 `provider` 之外的非交互参数，错误为 `Validation` 并列出触发的参数。
///
/// 说明:
/// - 交互模式按向导逐项提问，这些参数没有对应输入位；静默丢弃会让脚本误判已生效；
/// - `--no-validate` 与模式无关，不参与拦截。
fn reject_non_interactive_args_without_provider(opts: &SetupOptions) -> anyhow::Result<()> {
    if opts.provider.is_some() {
        return Ok(());
    }

    let provided: Vec<&str> = [
        ("--base-url", &opts.base_url),
        ("--model", &opts.model),
        ("--api-key", &opts.api_key),
        ("--embedding-path", &opts.embedding_path),
    ]
    .into_iter()
    .filter(|(_, value)| has_text(value))
    .map(|(flag, _)| flag)
    .collect();

    if provided.is_empty() {
        return Ok(());
    }

    Err(anyhow::anyhow!(RamariaError::validation(format!(
        "{} 需与 --provider 一起使用（交互模式按向导逐项提问）",
        provided.join(" / ")
    ))))
}

/// 解析线上 provider 的 API key：命令行优先，其次 `RAMARIA_API_KEY` 环境变量。
///
/// 参数:
/// - `explicit`: `--api-key` 给出的值。
///
/// 返回:
/// - 去首尾空白后的 key；命令行与环境变量都为空时为 None。
///
/// 说明:
/// - 只在非交互路径调用：交互模式不读环境变量，避免被误判成命令行参数。
fn resolve_api_key(explicit: Option<String>) -> Option<String> {
    non_empty(explicit).or_else(|| non_empty(std::env::var("RAMARIA_API_KEY").ok()))
}

/// 可选文本参数是否有内容（去首尾空白后非空）。
fn has_text(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(str::trim)
        .is_some_and(|text| !text.is_empty())
}

/// 解析 `--provider` 取值。
///
/// 参数:
/// - `value`: 用户给定的 provider 文本（大小写不敏感，忽略首尾空白）。
///
/// 返回:
/// - `Ok`: `lmstudio` / `lm_studio` / `lm-studio` / `deepseek` / `openai`；
/// - `Err`: 其它取值，错误为 `Validation` 类别并列出可选值。
fn parse_provider(value: &str) -> anyhow::Result<LlmProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "lmstudio" | "lm_studio" | "lm-studio" => Ok(LlmProvider::LmStudio),
        "deepseek" => Ok(LlmProvider::DeepSeek),
        "openai" => Ok(LlmProvider::OpenAI),
        _ => Err(anyhow::anyhow!(RamariaError::validation(format!(
            "不支持的 provider: {value}（可选值: lmstudio / lm_studio / lm-studio / deepseek / openai）"
        )))),
    }
}

/// provider 的默认 API 基础地址。
///
/// 参数:
/// - `provider`: LLM 服务类型。
///
/// 返回:
/// - 本地 LM Studio 或未知类型为 `http://localhost:1234/v1`；
///   线上 provider 返回各自官方地址。
fn default_base_url(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::LmStudio => "http://localhost:1234/v1",
        LlmProvider::DeepSeek => "https://api.deepseek.com/v1",
        LlmProvider::OpenAI => "https://api.openai.com/v1",
        _ => "http://localhost:1234/v1",
    }
}

/// 按命令行参数组装首次配置请求（不读 stdin）。
///
/// 参数:
/// - `provider`: provider 文本，取值见 [`parse_provider`]。
/// - `base_url` / `model`: 空白或 None 时取 provider 默认值。
/// - `api_key`: 空白或 None 视为未提供（线上 provider 由服务层校验报错）。
/// - `embedding_path`: 空白或 None 表示保留已保存的嵌入模型路径。
/// - `no_validate`: 映射为请求的 `skip_validate`。
///
/// 返回:
/// - 可直接提交给 `Engine::run_setup` 的请求；provider 非法时返回 `Validation`。
fn build_non_interactive_request(
    provider: &str,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    embedding_path: Option<String>,
    no_validate: bool,
) -> anyhow::Result<SetupRequest> {
    let provider = parse_provider(provider)?;
    Ok(SetupRequest {
        provider,
        model_id: non_empty(model).unwrap_or_else(|| default_model_id(provider).to_string()),
        base_url: non_empty(base_url).unwrap_or_else(|| default_base_url(provider).to_string()),
        api_key: non_empty(api_key),
        embedding_model_path: non_empty(embedding_path),
        skip_validate: no_validate,
    })
}

/// 清理可选文本参数：去首尾空白，空白结果视为未提供。
///
/// 参数:
/// - `value`: 命令行或环境变量给出的可选文本。
///
/// 返回:
/// - 非空时为去空白后的值，否则为 None。
fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// =========================================================
// 交互步骤
// =========================================================

/// 选择 provider 类型。
fn select_provider() -> anyhow::Result<LlmProvider> {
    println!("请选择 AI 服务类型：");
    println!("  1) LM Studio     — 本地运行，无需联网（推荐新手）");
    println!("  2) DeepSeek      — 线上服务，需 API key");
    println!("  3) OpenAI        — 线上服务，需 API key");
    println!();

    loop {
        let input = crate::ui::read_line("请输入数字 (1/2/3):")?;
        match input.trim() {
            "1" => {
                crate::ui::info("已选择: LM Studio（本地）");
                return Ok(LlmProvider::LmStudio);
            }
            "2" => {
                crate::ui::info("已选择: DeepSeek（线上）");
                return Ok(LlmProvider::DeepSeek);
            }
            "3" => {
                crate::ui::info("已选择: OpenAI（线上）");
                return Ok(LlmProvider::OpenAI);
            }
            other => {
                crate::ui::warn(&format!("无效选择: '{other}'，请输入 1、2 或 3"));
            }
        }
    }
}

/// 配置 base_url（回车采用 provider 默认地址）。
fn configure_base_url(provider: LlmProvider) -> anyhow::Result<String> {
    let default_url = default_base_url(provider);

    println!();
    println!("API 地址（直接回车使用默认值）：");
    let input = crate::ui::read_line(&format!("  [{default_url}]:"))?;

    let url = if input.trim().is_empty() {
        default_url.to_string()
    } else {
        input.trim().to_string()
    };

    crate::ui::info(&format!("API 地址: {url}"));
    Ok(url)
}

/// 配置 embedding 模型路径（可选）。
///
/// 说明:
/// - 直接回车跳过（保留已有配置或保持未设置）。
/// - 本地嵌入模型文件路径用于离线语义检索，与远程 embedding_model_id 二选一。
fn configure_embedding_model_path() -> anyhow::Result<Option<String>> {
    println!();
    println!("嵌入模型路径（可选，直接回车跳过）：");
    println!("  本地嵌入模型文件路径（如 GGUF），用于离线语义检索");
    let input = crate::ui::read_line("  [回车跳过]:")?;

    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        crate::ui::info(&format!("嵌入模型路径: {trimmed}"));
        Ok(Some(trimmed.to_string()))
    }
}

/// 配置 API key（仅线上 provider）。
fn configure_api_key(engine: &Arc<Engine>, provider: LlmProvider) -> anyhow::Result<String> {
    let service = provider_service(provider);

    // 尝试读取已有 key
    if let Ok(Some(existing)) = engine.keychain().get_api_key(service) {
        crate::ui::info(&format!(
            "检测到已有 {service} API key: {}",
            crate::ui::mask_key(&existing)
        ));
        // setup 是交互式向导，确认始终走 TTY（false = 不自动确认）
        let reuse = crate::ui::confirm("是否使用已有 key？", false)?;
        if reuse {
            return Ok(existing);
        }
    }

    println!();
    println!("请输入 {service} API key（输入不会显示）：");
    let key = crate::ui::read_secret("  API key:")?;

    if key.is_empty() {
        return Err(anyhow::anyhow!(
            "API key 不能为空。{service} 需要有效的 API key 才能使用。"
        ));
    }

    Ok(key)
}

// =========================================================
// 辅助函数
// =========================================================

/// 根据 provider 返回 keychain service 名称。
fn provider_service(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::LmStudio => "lm_studio",
        LlmProvider::DeepSeek => "deepseek",
        LlmProvider::OpenAI => "openai",
        _ => "unknown",
    }
}

/// 根据 provider 返回默认模型 ID。
fn default_model_id(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::LmStudio => "local-model",
        LlmProvider::DeepSeek => "deepseek-chat",
        LlmProvider::OpenAI => "gpt-4o-mini",
        _ => "unknown",
    }
}

/// 创建初始 persona：user-0001（系统默认） + 扫描 config/personas/ 目录下所有 .toml 文件。
///
/// 说明:
/// - user-0001 始终创建（代表当前用户本人）。
/// - 扫描 `config/personas/` 下的 .toml 文件，文件名 = persona UID；已存在记录跳过（幂等）。
/// - 每个文件的完整 TOML 内容存入 `persona.config` 字段，供 `build_system_prompt` 加载。
async fn create_initial_personas(engine: &Arc<Engine>) -> anyhow::Result<()> {
    // ---- Step 1: 确保 user-0001 存在（幂等） ----
    if engine
        .persona_ensure_user()
        .await
        .context("创建 user-0001 失败")?
    {
        crate::ui::info("已创建 persona: user-0001 (用户)");
    }

    // ---- Step 2: 新目录导入（已存在跳过） ----
    let dir = crate::commands::persona::personas_dir();
    let mut handled = false;
    if dir.exists() && dir.is_dir() {
        match engine
            .persona_load_from_dir(&dir, None, PersonaLoadMode::CreateMissing)
            .await
        {
            Ok(outcomes) => {
                for outcome in &outcomes {
                    if outcome.action == PersonaFileAction::Created {
                        crate::ui::info(&outcome.message);
                    }
                }
                handled = !outcomes.is_empty();
            }
            Err(e) => {
                tracing::warn!(error = %e, "读取 personas 目录失败");
            }
        }
    } else {
        tracing::warn!(dir = %dir.display(), "personas 目录不存在");
    }

    // ---- Step 3: 旧单文件路径兼容回退（新目录无文件时） ----
    let mut legacy_found = false;
    if !handled {
        let old_path = Path::new("../config/persona.toml");
        if old_path.exists() {
            legacy_found = load_legacy_persona_file(engine, old_path).await;
        }
    }

    // ---- Step 4: 未找到任何人格文件：引导提示 ----
    if !handled && !legacy_found {
        crate::ui::warn("未找到人格文件，请将 .toml 文件放入 config/personas/ 目录");
        crate::ui::info("示例: config/personas/rama-0001.toml");
    }

    Ok(())
}

/// 旧单文件布局的兼容回退：`config/persona.toml` → `rama-0001`。
///
/// 行为:
/// - 仅创建缺失：记录不存在时新建（`config` = 文件全文）；已存在时跳过（不写库）；
/// - 读取 / 查询 / 写入失败记 warn 并继续（不阻塞向导）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `old_path`: 旧单文件路径（存在性由调用方预判）。
///
/// 返回:
/// - `true`: 旧文件已处理（新建或跳过）；`false`: 处理失败（按未命中处理）。
async fn load_legacy_persona_file(engine: &Arc<Engine>, old_path: &Path) -> bool {
    let outcome = engine
        .persona_load_file(
            old_path,
            "rama-0001",
            "Ramaria",
            PersonaLoadMode::CreateMissing,
        )
        .await;

    match outcome.action {
        PersonaFileAction::Created => {
            tracing::info!(path = %old_path.display(), "从旧路径加载 persona.toml（兼容回退）");
            crate::ui::info(&outcome.message);
            true
        }
        PersonaFileAction::Updated | PersonaFileAction::Skipped => {
            tracing::info!(path = %old_path.display(), "从旧路径加载 persona.toml（兼容回退）");
            true
        }
        _ => {
            tracing::warn!(
                path = %old_path.display(),
                error = %outcome.message,
                "旧 persona.toml 加载失败（跳过，不阻塞向导）"
            );
            false
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
