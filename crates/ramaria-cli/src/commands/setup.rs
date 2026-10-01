//! crates/ramaria-cli/src/commands/setup.rs - 首次配置向导
//!
//! 设计特点:
//! - 交互式步骤: 选 provider → 配地址 → 配嵌入模型路径（可选）→ 输 API key（线上）
//! - 委托服务层用例完成配置写入（密钥入 keychain → 配置落库 → provider 热替换 →
//!   文件侧 [backend] 同步）与状态刷新
//! - 可选验证 provider 连接可用性（与首次配置同一探测实现）
//! - 本地 LM Studio 跳过 API key 步骤
//! - 人格初始化: 扫描 config/personas/ 目录下所有 .toml 文件，批量创建 persona
//! - 错误信息清晰，每步可重试

use anyhow::Context;
use ramaria_core::types::BackendConfig;
use ramaria_service::{Engine, PersonaFileAction, PersonaLoadMode};
use std::path::Path;
use std::sync::Arc;

/// 运行首次配置向导。
///
/// 参数:
/// - `engine`: 服务层引擎引用（初始状态应为 NeedsSetup）。
/// - `skip_validate`: 跳过 LLM 连接验证（默认 false，用户可传入 true）。
pub async fn run(engine: &Arc<Engine>, skip_validate: bool) -> anyhow::Result<()> {
    crate::ui::separator();
    println!("  Ramaria 首次配置向导");
    crate::ui::separator();
    println!();

    // ---- Step 1: 选择 Provider ----
    let provider = select_provider()?;

    // ---- Step 2: 配置 base_url ----
    let base_url = configure_base_url(provider)?;

    // ---- Step 3: 配置 embedding 模型路径（可选，回车跳过）----
    let embedding_model_path = configure_embedding_model_path()?;

    // ---- Step 4: 配置 API key（仅线上 provider）----
    let api_key = if provider.is_online() {
        Some(configure_api_key(engine, provider)?)
    } else {
        None
    };

    // ---- Step 5: 构建 BackendConfig 并保存 ----
    // 继承已有配置中的 embedding 模型设置（重跑向导时不丢配置），
    // 向导新输入的路径优先。
    let existing = engine
        .backend_config()
        .await?
        .unwrap_or_else(BackendConfig::lm_studio_default);
    let config = BackendConfig {
        provider,
        base_url: base_url.clone(),
        embedding_model_id: existing.embedding_model_id,
        embedding_model_path: embedding_model_path.or(existing.embedding_model_path),
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

    // 保存后端配置（服务层用例：密钥入 keychain → 配置落库 → provider 热替换 →
    // 文件侧 [backend] 同步，保证下次启动以文件为准时不被模板覆盖）
    engine
        .update_backend_config(&config, api_key.as_deref())
        .await
        .context("保存后端配置失败")?;

    if api_key.is_some() {
        let service = provider_service(provider);
        crate::ui::success(&format!("API key 已安全保存到系统凭据管理器 ({service})"));
    }

    crate::ui::success("后端配置已保存");

    // ---- Step 6: 创建初始 persona（扫描 personas/ 目录批量初始化）----
    create_initial_personas(engine).await?;

    // ---- Step 7: 刷新应用状态 ----
    let new_state = engine
        .refresh_setup_state()
        .await
        .context("刷新应用状态失败")?;

    crate::ui::info(&format!("应用状态: {new_state}"));
    if new_state == ramaria_core::types::AppState::NeedsSetup {
        crate::ui::warn("应用仍需要进一步配置（如 embedding 模型下载）");
    }

    // ---- Step 8: 可选验证 LLM 连接 ----
    if !skip_validate {
        println!();
        crate::ui::info("正在验证 LLM 连接...");
        match validate_llm_connection(engine).await {
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
fn configure_api_key(
    engine: &Arc<Engine>,
    provider: ramaria_core::types::LlmProvider,
) -> anyhow::Result<String> {
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

/// 探测 LLM 连接并推进状态（与首次配置同一探测实现：最多 3 次、间隔 2 秒）。
///
/// 返回:
/// - 探测通过 → 按缺项诊断判定（含真实嵌入可用性）后的应用状态；
/// - 探测失败 → `Degraded`（不报错，用户可修正配置后重试）。
async fn validate_llm_connection(
    engine: &Arc<Engine>,
) -> anyhow::Result<ramaria_core::types::AppState> {
    let health_ok = engine.probe_llm_health().await;
    if health_ok {
        return engine
            .refresh_setup_state()
            .await
            .context("刷新应用状态失败");
    }
    engine.set_state(ramaria_core::types::AppState::Degraded);
    Ok(ramaria_core::types::AppState::Degraded)
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
