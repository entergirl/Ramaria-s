//! rust/crates/ramaria-cli/src/commands/setup.rs - 首次配置向导
//!
//! 设计特点:
//! - 交互式三步: 选 provider → 配地址 → 输 API key（线上）
//! - 委托 ramaria-app 进行设置保存和状态刷新
//! - 验证 provider 连接可用性（可选）
//! - 本地 LM Studio 跳过 API key 步骤
//! - 人格初始化: 扫描 config/personas/ 目录下所有 .toml 文件，批量创建 persona
//! - 错误信息清晰，每步可重试

use anyhow::Context;
use ramaria_core::types::{BackendConfig, PersonaKind};
use std::path::Path;
use std::sync::Arc;

/// 运行首次配置向导。
///
/// 参数:
/// - `app`: App 实例引用（初始状态应为 NeedsSetup）。
/// - `skip_validate`: 跳过 LLM 连接验证（默认 false，用户可传入 true）。
pub async fn run(app: &Arc<ramaria_app::App>, skip_validate: bool) -> anyhow::Result<()> {
    crate::ui::separator();
    println!("  Ramaria 首次配置向导");
    crate::ui::separator();
    println!();

    // ---- Step 1: 选择 Provider ----
    let provider = select_provider()?;

    // ---- Step 2: 配置 base_url ----
    let base_url = configure_base_url(provider)?;

    // ---- Step 3: 配置 API key（仅线上 provider）----
    let api_key = if provider.is_online() {
        Some(configure_api_key(app, provider)?)
    } else {
        None
    };

    // ---- Step 4: 构建 BackendConfig 并保存 ----
    let config = BackendConfig {
        provider,
        base_url: base_url.clone(),
        embedding_model_id: None,
        embedding_model_path: None,
        temperature: 0.3,
        max_tokens: 1024,
        capability: ramaria_core::types::ModelCapability {
            provider,
            model_id: default_model_id(provider).to_string(),
            base_url,
            supports_streaming: true,
            supports_json_mode: provider.is_online(),
            context_window: 4096,
            max_output_tokens: 2048,
        },
    };

    // 保存后端配置到存储
    app.storage()
        .save_backend_config(&config)
        .await
        .context("保存后端配置失败")?;

    // 保存 API key 到 keychain
    if let Some(ref key) = api_key {
        let service = provider_service(provider);
        app.keychain()
            .set_api_key(service, key)
            .context("保存 API key 到 keychain 失败")?;
        crate::ui::success(&format!("API key 已安全保存到系统凭据管理器 ({service})"));
    }

    crate::ui::success("后端配置已保存");

    // ---- Step 5: 创建初始 persona（扫描 personas/ 目录批量初始化）----
    create_initial_personas(app).await?;

    // ---- Step 6: 刷新应用状态 ----
    let new_state = app
        .refresh_setup_state()
        .await
        .context("刷新应用状态失败")?;

    crate::ui::info(&format!("应用状态: {new_state}"));
    if new_state == ramaria_core::types::AppState::NeedsSetup {
        crate::ui::warn("应用仍需要进一步配置（如 embedding 模型下载）");
    }

    // ---- Step 7: 可选验证 LLM 连接 ----
    if !skip_validate {
        println!();
        crate::ui::info("正在验证 LLM 连接...");
        // 验证通过 run_setup 触发
        match app.run_setup(&config).await {
            Ok(state) => {
                crate::ui::success(&format!("LLM 连接验证通过，当前状态: {state}"));
            }
            Err(e) => {
                crate::ui::warn(&format!("LLM 连接验证失败: {e}"));
                crate::ui::info("配置已保存，可稍后在 config 中调整后重试");
            }
        }
    }

    crate::ui::separator();
    crate::ui::success("配置向导完成！使用 `ramaria ask <消息>` 开始对话。");
    Ok(())
}

// =========================================================
// 交互步骤
// =========================================================

/// 选择 provider 类型。
fn select_provider() -> anyhow::Result<ramaria_core::types::LlmProvider> {
    use ramaria_core::types::LlmProvider;

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

/// 配置 base_url。
fn configure_base_url(provider: ramaria_core::types::LlmProvider) -> anyhow::Result<String> {
    let default_url = match provider {
        ramaria_core::types::LlmProvider::LmStudio => "http://localhost:1234/v1",
        ramaria_core::types::LlmProvider::DeepSeek => "https://api.deepseek.com/v1",
        ramaria_core::types::LlmProvider::OpenAI => "https://api.openai.com/v1",
        _ => "http://localhost:1234/v1",
    };

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

/// 配置 API key（仅线上 provider）。
fn configure_api_key(
    app: &Arc<ramaria_app::App>,
    provider: ramaria_core::types::LlmProvider,
) -> anyhow::Result<String> {
    let service = provider_service(provider);

    // 尝试读取已有 key
    if let Ok(Some(existing)) = app.keychain().get_api_key(service) {
        crate::ui::info(&format!(
            "检测到已有 {service} API key: {}",
            crate::ui::mask_key(&existing)
        ));
        let reuse = crate::ui::confirm("是否使用已有 key？")?;
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
fn provider_service(provider: ramaria_core::types::LlmProvider) -> &'static str {
    match provider {
        ramaria_core::types::LlmProvider::LmStudio => "lm_studio",
        ramaria_core::types::LlmProvider::DeepSeek => "deepseek",
        ramaria_core::types::LlmProvider::OpenAI => "openai",
        _ => "unknown",
    }
}

/// 根据 provider 返回默认模型 ID。
fn default_model_id(provider: ramaria_core::types::LlmProvider) -> &'static str {
    match provider {
        ramaria_core::types::LlmProvider::LmStudio => "local-model",
        ramaria_core::types::LlmProvider::DeepSeek => "deepseek-chat",
        ramaria_core::types::LlmProvider::OpenAI => "gpt-4o-mini",
        _ => "unknown",
    }
}

/// 创建初始 persona：user-0001（系统默认） + 扫描 config/personas/ 目录下所有 .toml 文件。
///
/// 说明:
/// - user-0001 始终创建（代表当前用户本人）。
/// - 扫描 `../config/personas/` 下的 .toml 文件，文件名 = persona UID。
/// - 每个文件的完整 TOML 内容存入 `persona.config` 字段，供 `build_system_prompt()` 加载。
/// - 已存在的 persona 跳过不重复创建。
async fn create_initial_personas(app: &Arc<ramaria_app::App>) -> anyhow::Result<()> {
    // 确保 user-0001 存在
    if app
        .storage()
        .get_persona_by_uid("user-0001")
        .await?
        .is_none()
    {
        let user = ramaria_core::types::Persona::new(
            "user-0001".to_string(),
            "用户".to_string(),
            PersonaKind::User,
            1,
            "system".to_string(),
        );
        app.storage()
            .create_persona(&user)
            .await
            .context("创建 user-0001 失败")?;
        crate::ui::info("已创建 persona: user-0001 (用户)");
    }

    // 扫描 personas/ 目录
    let persona_files = scan_personas_directory();

    if persona_files.is_empty() {
        crate::ui::warn("未找到人格文件，请将 .toml 文件放入 config/personas/ 目录");
        crate::ui::info("示例: config/personas/rama-0001.toml");
        return Ok(());
    }

    for (uid, name, config) in persona_files {
        if app.storage().get_persona_by_uid(&uid).await?.is_some() {
            tracing::debug!(%uid, "persona 已存在，跳过创建");
            continue;
        }

        let kind = PersonaKind::from_uid(&uid);
        let mut persona = ramaria_core::types::Persona::new(
            uid.clone(),
            name.clone(),
            kind,
            1,
            "file".to_string(),
        );
        persona.config = Some(config);
        app.storage()
            .create_persona(&persona)
            .await
            .with_context(|| format!("创建 persona 失败: {uid}"))?;
        crate::ui::info(&format!("已创建 persona: {uid} ({name})"));
    }

    Ok(())
}

/// 扫描 config/personas/ 目录，返回所有 .toml 文件的信息。
///
/// 注: `extract_toml_value` 使用共享的 `crate::util::extract_toml_value()`。
///
/// 返回:
/// - `Vec<(uid, assistant_name, raw_toml_content)>`。
///
/// 降级策略:
/// - 目录不存在 → 返回空 Vec，记录 warn 日志。
/// - 单文件读取失败 → 跳过该文件，继续处理其他文件。
/// - 所有文件为空 → 尝试旧路径 `../config/persona.toml` 作为兼容回退。
fn scan_personas_directory() -> Vec<(String, String, String)> {
    let dir = Path::new("../config/personas");
    let mut results: Vec<(String, String, String)> = Vec::new();

    if dir.exists() && dir.is_dir() {
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
                    {
                        continue;
                    }

                    let uid = match path.file_stem().and_then(|s| s.to_str()) {
                        Some(s) => s.to_string(),
                        None => {
                            tracing::warn!(path = %path.display(), "无法从文件名提取 UID，跳过");
                            continue;
                        }
                    };

                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(%e, path = %path.display(), "读取 persona 文件失败，跳过");
                            continue;
                        }
                    };

                    let name = crate::util::extract_toml_value(&content, "assistant_name")
                        .unwrap_or_else(|| uid.clone());

                    tracing::info!(%uid, %name, path = %path.display(), "发现人格文件");
                    results.push((uid, name, content));
                }
            }
            Err(e) => {
                tracing::warn!(%e, dir = %dir.display(), "读取 personas 目录失败");
            }
        }
    } else {
        tracing::warn!(dir = %dir.display(), "personas 目录不存在");
    }

    // 兼容回退：如果新目录没有文件，尝试旧单文件路径
    if results.is_empty() {
        let old_path = Path::new("../config/persona.toml");
        if old_path.exists() {
            match std::fs::read_to_string(old_path) {
                Ok(content) => {
                    let name = crate::util::extract_toml_value(&content, "assistant_name")
                        .unwrap_or_else(|| "Ramaria".to_string());
                    tracing::info!(%name, path = %old_path.display(), "从旧路径加载 persona.toml（兼容回退）");
                    results.push(("rama-0001".to_string(), name, content));
                }
                Err(e) => {
                    tracing::warn!(%e, path = %old_path.display(), "读取旧 persona.toml 失败");
                }
            }
        }
    }

    results
}

// `extract_toml_value` 已提取至 `crate::util` 模块。
